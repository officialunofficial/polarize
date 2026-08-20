//! The two notification-banner tools: `describe_notifications` reads
//! every banner macOS shows right now, and `dismiss_notification`
//! closes one.
//!
//! ## Why this reuses `describe`
//!
//! macOS draws every notification banner from one process,
//! `com.apple.notificationcenterui`. That process publishes an
//! accessibility tree like any other app. So neither tool needs a new
//! native API. `describe_notifications` reads the same tree
//! [`crate::traits::AccessibilityInspector`] already returns, and
//! `dismiss_notification` presses a button through the same
//! [`crate::traits::ActionPerformer`] `perform_action` uses. Both tools
//! need the Accessibility permission, and nothing else.
//!
//! ## Why the extraction lives here
//!
//! [`extract_banners`] turns an [`AxNode`] tree into a list of banner
//! records. It is a pure function over an in-memory tree, so
//! `cargo test -p polarize-core` covers it against hand-built trees.
//! That is the only real coverage this feature can have: nobody can
//! post a real notification inside CI. See PINV-35.
//!
//! ## Known limitation: the banner tree shapes here are informed guesses
//!
//! Nobody has run this code against a real macOS notification. The
//! shapes the tests build come from Apple's published accessibility
//! conventions and from how the banner looks on screen, not from a
//! recorded tree. A human must run `describe_notifications` against a
//! real banner and compare. See PINV-35's enforcement entry.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ax::{AxNode, NormalizedFrame};
use crate::error::PolarizeError;
use crate::schema::AppIdentifier;
use crate::selector::ElementPath;
use crate::traits::{AccessibilityInspector, ActionPerformer};

// ---- the target process -------------------------------------------------

/// The process that draws every notification banner on macOS.
///
/// Apple has published this bundle id since OS X 10.8. It is the one
/// part of a banner's identity that does not move between releases,
/// which is why both tools address it by bundle id and never by name.
pub const NOTIFICATION_CENTER_BUNDLE_ID: &str = "com.apple.notificationcenterui";

/// The app identifier both tools describe and act on.
pub fn notification_center_app() -> AppIdentifier {
    AppIdentifier {
        bundle_id: Some(NOTIFICATION_CENTER_BUNDLE_ID.to_string()),
        app_name: None,
    }
}

// ---- structural vocabulary ----------------------------------------------

/// Roles whose label describes a control, not the banner's prose.
///
/// A banner can carry a "Reply" button and a reply text field. Neither
/// label belongs in the banner's body text.
const CONTROL_ROLES: [&str; 8] = [
    "AXButton",
    "AXMenuButton",
    "AXPopUpButton",
    "AXCheckBox",
    "AXRadioButton",
    "AXTextField",
    "AXTextArea",
    "AXComboBox",
];

/// Lower-case fragments that mark a container as one banner.
///
/// These are hints, never requirements. [`extract_banners`] finds a
/// banner from tree structure alone; a hint only settles a container
/// the structure would otherwise split. See PINV-35.
const BANNER_HINTS: [&str; 3] = ["notification", "banner", "alert"];

/// Lower-case fragments that mark a control as the close control.
const DISMISS_HINTS: [&str; 3] = ["close", "dismiss", "clear"];

/// Lower-case fragments that mark a control as acting on **every**
/// notification, not on one banner.
///
/// Notification Centre publishes a "Clear All" button. Its label matches
/// [`DISMISS_HINTS`], and macOS is free to place it inside a banner's own
/// container — a stacked notification group is the obvious case. Pressing
/// it discards notifications the caller never named, and nothing undoes
/// that. See PINV-35.
const CLEAR_ALL_HINTS: [&str; 2] = ["all", "everything"];

/// The `AXSubrole` macOS gives a window's close button. A close control
/// that publishes it needs no other evidence.
const CLOSE_BUTTON_SUBROLE: &str = "AXCloseButton";

// ---- the banner record --------------------------------------------------

/// One notification banner, read out of the notification centre's
/// accessibility tree.
///
/// `app`, `title`, and `body` come from a positional reading of the
/// banner's text run — see [`extract_banners`]. `texts` keeps that run
/// as it was found, so a caller can recover the real fields when the
/// positional reading is wrong on some future macOS.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotificationBanner {
    /// Position in the list `describe_notifications` returned, from `0`.
    /// `dismiss_notification` selects a banner by this number.
    pub index: usize,
    /// The posting app's name, when the banner names one.
    pub app: Option<String>,
    /// The banner's headline.
    pub title: Option<String>,
    /// The banner's remaining text, joined with newlines.
    pub body: Option<String>,
    /// Every text this banner published, in tree order.
    pub texts: Vec<String>,
    /// The banner's on-screen frame, normalized like every other
    /// [`AxNode`] frame.
    pub frame: NormalizedFrame,
    /// The child indices from the notification centre's tree root down
    /// to this banner's own container element.
    pub path: ElementPath,
    /// The path of the control that closes this banner, when it has
    /// one. `None` means no control in the banner looked like a close
    /// control, so `dismiss_notification` refuses to guess.
    pub dismiss_path: Option<ElementPath>,
    /// The AX action `dismiss_notification` performs on that control.
    pub dismiss_action: Option<String>,
    /// The close control's label, so a caller can see what will be
    /// pressed before it presses it.
    pub dismiss_label: Option<String>,
}

impl NotificationBanner {
    /// The text identity this banner is recognized by after a dismiss.
    ///
    /// A path is not an identity: closing a banner renumbers the ones
    /// below it. The text run is stable while the banner is on screen.
    fn key(&self) -> Vec<String> {
        self.texts.clone()
    }

    /// One line for a `formatted` rendering.
    fn format_line(&self) -> String {
        let mut parts = vec![format!("[{}]", self.index)];
        if let Some(app) = &self.app {
            parts.push(format!("{app}:"));
        }
        if let Some(title) = &self.title {
            parts.push(title.clone());
        }
        if let Some(body) = &self.body {
            parts.push(format!("— {}", body.replace('\n', " ")));
        }
        if self.dismiss_path.is_none() {
            parts.push("(no dismiss control)".to_string());
        }
        parts.join(" ")
    }
}

// ---- extraction ---------------------------------------------------------

/// Whether this node's own label reads as banner prose.
fn is_text_leaf(node: &AxNode) -> bool {
    node.children.is_empty()
        && !CONTROL_ROLES.contains(&node.role.as_str())
        && node.label.as_ref().is_some_and(|label| !label.is_empty())
}

/// Whether any prose text sits in this subtree.
fn has_text(node: &AxNode) -> bool {
    if CONTROL_ROLES.contains(&node.role.as_str()) {
        return false;
    }
    is_text_leaf(node) || node.children.iter().any(has_text)
}

/// How much evidence says a node is a banner's close control.
///
/// A close control must publish at least one action; a label alone
/// presses nothing. `Named` evidence is a subrole, identifier, or label
/// that names the control. There is no weaker level on purpose: a
/// "Reply" button is also a pressable button in a banner, and pressing
/// it instead of the close control would send a message the caller
/// never wrote. See PINV-35.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DismissEvidence {
    /// A label or identifier that names a close control.
    Named,
    /// `AXSubrole` is `AXCloseButton`. macOS itself said so.
    Subrole,
}

fn dismiss_evidence(node: &AxNode) -> Option<DismissEvidence> {
    if node.actions.is_empty() {
        return None;
    }
    // Never a single banner's control, whatever else it looks like. A
    // wrong guess here is not recoverable.
    if clears_everything(node) {
        return None;
    }
    if node.subrole.as_deref() == Some(CLOSE_BUTTON_SUBROLE) {
        return Some(DismissEvidence::Subrole);
    }
    let named = [&node.identifier, &node.label, &node.help]
        .into_iter()
        .flatten()
        .any(|value| contains_hint(value, &DISMISS_HINTS));
    // A subrole that names a close control is still only a name: a
    // future macOS may invent `AXNotificationClearButtonV4`, and the
    // string alone must not be the only thing this looks at.
    let named = named
        || node
            .subrole
            .as_deref()
            .is_some_and(|subrole| contains_hint(subrole, &DISMISS_HINTS));
    named.then_some(DismissEvidence::Named)
}

/// Whether a control's own name says it acts on every notification.
///
/// This asks for both halves: a dismiss word and an all-of-them word.
/// "Clear All" and "Close all notifications" match; "Close" does not,
/// and neither does an unrelated control that merely says "all".
fn clears_everything(node: &AxNode) -> bool {
    [&node.identifier, &node.label, &node.help]
        .into_iter()
        .flatten()
        .any(|value| contains_hint(value, &DISMISS_HINTS) && contains_hint(value, &CLEAR_ALL_HINTS))
}

fn contains_hint(value: &str, hints: &[&str]) -> bool {
    let lowered = value.to_lowercase();
    hints.iter().any(|hint| lowered.contains(hint))
}

/// The best close control among a node's own children, as a child index.
fn direct_dismiss_child(node: &AxNode) -> Option<usize> {
    node.children
        .iter()
        .enumerate()
        .filter_map(|(index, child)| dismiss_evidence(child).map(|evidence| (evidence, index)))
        .max_by_key(|(evidence, _)| *evidence)
        .map(|(_, index)| index)
}

/// Whether a container names itself a banner.
///
/// Only the developer-set fields count: `AXSubrole`, `AXIdentifier`,
/// and `AXRoleDescription`. A node's label is the notification's own
/// text, and matching a hint against it would call any notification
/// about "alerts" a banner container.
fn has_banner_hint(node: &AxNode) -> bool {
    [&node.subrole, &node.identifier, &node.role_description]
        .into_iter()
        .flatten()
        .any(|value| contains_hint(value, &BANNER_HINTS))
}

/// Whether this node is one whole banner, rather than a container of
/// several or a part of one. See PINV-35.
fn is_banner_unit(node: &AxNode) -> bool {
    direct_dismiss_child(node).is_some() || has_banner_hint(node)
}

/// # PINV-35: a banner is found by structure, not by a subrole string
///
/// - Always: [`extract_banners`] identifies a banner from the shape of
///   the notification centre's tree — a container holding prose text,
///   and usually a close control next to it. A subrole, an identifier,
///   or a role description only ever *adds* evidence; no string value
///   is required for a banner to be found. A tree shape this code has
///   never seen still yields every banner it can identify, rather than
///   an empty list.
/// - Because: Apple renames banner subroles between macOS releases, and
///   has restructured the banner hierarchy more than once. A matcher
///   keyed on `"AXNotificationCenterBanner"` reports zero banners on the
///   first macOS that renames it, and reports that as a normal empty
///   result. Structure — text inside a container, a close control
///   beside it — has stayed stable across every one of those changes.
/// - If violated: `describe_notifications` returns an empty list on a
///   Mac that is showing a banner, and a caller cannot tell that from
///   "no notification is on screen".
pub fn extract_banners(root: &AxNode) -> Vec<NotificationBanner> {
    let mut roots: Vec<ElementPath> = Vec::new();
    collect_banner_roots(root, &mut Vec::new(), &mut roots);
    roots
        .into_iter()
        .enumerate()
        .filter_map(|(index, path)| build_banner(root, path, index))
        .collect()
}

/// Walks down to each banner container.
///
/// The descent has three rules, in this order:
///
/// 1. A node that names itself a banner, or that holds a close control
///    among its own children, is one banner. Stop there.
/// 2. A node whose text-bearing children are *all* containers is a
///    grouping node — a window, a list, a scroll area, or a wrapper.
///    Recurse into each of those children.
/// 3. Anything else holds the text itself. It is one banner.
fn collect_banner_roots(node: &AxNode, path: &mut ElementPath, out: &mut Vec<ElementPath>) {
    // The tree root is the application element. Its own label is the
    // app's name, not a banner's text, so only its descendants count.
    let is_root = path.is_empty();
    let holds_text = if is_root {
        node.children.iter().any(has_text)
    } else {
        has_text(node)
    };
    if !holds_text {
        return;
    }
    // Rule 1. The application element is never itself one banner.
    if !is_root && is_banner_unit(node) {
        out.push(path.clone());
        return;
    }

    let parts: Vec<usize> = node
        .children
        .iter()
        .enumerate()
        .filter(|(_, child)| has_text(child))
        .map(|(index, _)| index)
        .collect();
    let all_containers = !parts.is_empty()
        && parts
            .iter()
            .all(|&index| !node.children[index].children.is_empty());

    // Rule 2.
    if all_containers {
        for index in parts {
            path.push(index);
            collect_banner_roots(&node.children[index], path, out);
            path.pop();
        }
        return;
    }

    // Rule 3.
    out.push(path.clone());
}

/// Reads one banner's fields out of the container at `path`.
fn build_banner(root: &AxNode, path: ElementPath, index: usize) -> Option<NotificationBanner> {
    let node = crate::selector::node_at_path(root, &path)?;

    let mut texts = Vec::new();
    collect_texts(node, &mut texts);
    if texts.is_empty() {
        return None;
    }

    // The positional reading. A real banner prints the posting app
    // first, then the headline, then the body. Two texts are an app and
    // a headline. One text is a headline: with nothing to compare it
    // against, calling it an app name would be a guess that reads worse
    // than no guess at all.
    let (app, title, body) = match texts.len() {
        1 => (None, Some(texts[0].clone()), None),
        2 => (Some(texts[0].clone()), Some(texts[1].clone()), None),
        _ => (
            Some(texts[0].clone()),
            Some(texts[1].clone()),
            Some(texts[2..].join("\n")),
        ),
    };

    let dismiss = find_dismiss(node, &path);
    Some(NotificationBanner {
        index,
        app,
        title,
        body,
        texts,
        frame: node.frame,
        path,
        dismiss_path: dismiss.as_ref().map(|(path, _, _)| path.clone()),
        dismiss_action: dismiss.as_ref().map(|(_, action, _)| action.clone()),
        dismiss_label: dismiss.and_then(|(_, _, label)| label),
    })
}

/// Collects every prose text in `node`'s subtree, in tree order.
/// A control's own label never counts; see [`CONTROL_ROLES`].
fn collect_texts(node: &AxNode, out: &mut Vec<String>) {
    if CONTROL_ROLES.contains(&node.role.as_str()) {
        return;
    }
    if is_text_leaf(node)
        && let Some(label) = &node.label
    {
        out.push(label.clone());
    }
    for child in &node.children {
        collect_texts(child, out);
    }
}

/// The strongest close control in this banner, as an absolute path, the
/// action to perform on it, and its label.
///
/// `AXPress` wins when the control offers it, because that is what a
/// button publishes for a click. Otherwise the control's first action
/// is used: a future close control may publish only `AXCancel`.
fn find_dismiss(
    node: &AxNode,
    banner_path: &[usize],
) -> Option<(ElementPath, String, Option<String>)> {
    let mut best: Option<(DismissEvidence, ElementPath, String, Option<String>)> = None;
    let mut relative = Vec::new();
    walk_dismiss(node, &mut relative, &mut best);
    best.map(|(_, relative, action, label)| {
        let mut path = banner_path.to_vec();
        path.extend(relative);
        (path, action, label)
    })
}

fn walk_dismiss(
    node: &AxNode,
    relative: &mut ElementPath,
    best: &mut Option<(DismissEvidence, ElementPath, String, Option<String>)>,
) {
    if let Some(evidence) = dismiss_evidence(node) {
        let better = best
            .as_ref()
            .is_none_or(|(found, _, _, _)| evidence > *found);
        if better {
            let action = if node.actions.iter().any(|name| name == "AXPress") {
                "AXPress".to_string()
            } else {
                node.actions[0].clone()
            };
            *best = Some((evidence, relative.clone(), action, node.label.clone()));
        }
    }
    for (index, child) in node.children.iter().enumerate() {
        relative.push(index);
        walk_dismiss(child, relative, best);
        relative.pop();
    }
}

// ---- errors -------------------------------------------------------------

/// Why a notification tool could not answer.
///
/// `polarize_core::error::PolarizeError` has no variant of its own for
/// these, and this change does not own `error.rs`. Each one converts to
/// [`PolarizeError::Platform`] and keeps its whole message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotificationError {
    /// The notification centre is showing nothing.
    #[error("no notification banner is on screen")]
    NoBanners,

    /// Banners are on screen, but none matched the request's filters.
    #[error("no notification banner matches ({filter}); {count} banner(s) are on screen")]
    NoMatch { filter: String, count: usize },

    /// The filters matched, but fewer banners than `index` needs.
    #[error("{matches} notification banner(s) match, so index {index} is out of range")]
    IndexOutOfRange { index: usize, matches: usize },

    /// The banner publishes no control that closes it.
    #[error(
        "notification banner ({banner}) publishes no dismiss control, \
         so polarize will not guess which control to press"
    )]
    NoDismissControl { banner: String },
}

impl From<NotificationError> for PolarizeError {
    fn from(error: NotificationError) -> Self {
        PolarizeError::Platform(error.to_string())
    }
}

// ---- describe_notifications ---------------------------------------------

/// A `describe_notifications` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct DescribeNotificationsRequest {
    /// Report only banners whose app name holds this text. The match
    /// ignores letter case. `None` reports every banner.
    #[serde(default)]
    pub from_app: Option<String>,
}

/// The result of a `describe_notifications` tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DescribeNotificationsResponse {
    /// The app the tool read, as the platform resolved it. This is the
    /// notification centre, not the app that posted a banner.
    pub app_name: String,
    /// How many banners the response carries, after any filter.
    pub count: usize,
    pub banners: Vec<NotificationBanner>,
    /// One line per banner, for a reader.
    pub formatted: String,
}

/// Reads every notification banner macOS is showing right now.
///
/// The tool always describes `com.apple.notificationcenterui`. A
/// caller never names an app, because only one process draws banners.
pub fn perform_describe_notifications<A>(
    inspector: &A,
    request: &DescribeNotificationsRequest,
) -> Result<DescribeNotificationsResponse, PolarizeError>
where
    A: AccessibilityInspector,
{
    let (resolved, root) = inspector.describe(Some(&notification_center_app()))?;
    let banners = filtered_banners(&root, request.from_app.as_deref(), None);
    Ok(DescribeNotificationsResponse {
        app_name: resolved.name,
        count: banners.len(),
        formatted: format_banners(&banners),
        banners,
    })
}

/// Every banner that passes the two text filters, renumbered from `0`.
fn filtered_banners(
    root: &AxNode,
    from_app: Option<&str>,
    title_contains: Option<&str>,
) -> Vec<NotificationBanner> {
    extract_banners(root)
        .into_iter()
        .filter(|banner| {
            from_app.is_none_or(|needle| {
                banner
                    .app
                    .as_deref()
                    .is_some_and(|app| contains_ignoring_case(app, needle))
            })
        })
        .filter(|banner| {
            title_contains.is_none_or(|needle| {
                banner
                    .texts
                    .iter()
                    .any(|text| contains_ignoring_case(text, needle))
            })
        })
        .enumerate()
        .map(|(index, mut banner)| {
            banner.index = index;
            banner
        })
        .collect()
}

fn contains_ignoring_case(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn format_banners(banners: &[NotificationBanner]) -> String {
    if banners.is_empty() {
        return "no notification banner is on screen".to_string();
    }
    banners
        .iter()
        .map(NotificationBanner::format_line)
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- dismiss_notification -----------------------------------------------

/// How many times a dismiss re-reads the tree when the banner is still
/// there. One read always happens; this is the total.
pub const DEFAULT_VERIFY_ATTEMPTS: u32 = 3;

/// The most re-reads a caller may ask for.
pub const MAX_VERIFY_ATTEMPTS: u32 = 20;

/// How long a dismiss waits between two re-reads.
pub const DEFAULT_VERIFY_DELAY_MS: u64 = 120;

/// The longest pause a caller may ask for between two re-reads.
pub const MAX_VERIFY_DELAY_MS: u64 = 2_000;

/// A caller's verification settings, after defaults and clamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifySettings {
    pub attempts: u32,
    pub delay_ms: u64,
}

impl VerifySettings {
    /// Applies the defaults and the clamps this module documents.
    ///
    /// Zero attempts becomes one. A dismiss that never re-reads the
    /// tree could only report the action it performed, which is the
    /// exact claim PINV-35 forbids.
    pub fn resolve(attempts: Option<u32>, delay_ms: Option<u64>) -> Self {
        Self {
            attempts: attempts
                .unwrap_or(DEFAULT_VERIFY_ATTEMPTS)
                .clamp(1, MAX_VERIFY_ATTEMPTS),
            delay_ms: delay_ms
                .unwrap_or(DEFAULT_VERIFY_DELAY_MS)
                .min(MAX_VERIFY_DELAY_MS),
        }
    }
}

/// Pauses the calling thread.
///
/// A banner leaves the screen with an animation, so the tree can still
/// carry it for a moment after the press. Verification therefore waits
/// between re-reads. A test drives a fake instead, so no test sleeps.
pub trait Sleeper {
    fn sleep_ms(&self, ms: u64);
}

/// The real [`Sleeper`], over [`std::thread::sleep`].
#[derive(Debug, Default)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep_ms(&self, ms: u64) {
        if ms > 0 {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

/// A `dismiss_notification` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct DismissNotificationRequest {
    /// Which of the matching banners to close, counted from `0`.
    /// `None` means the first one.
    #[serde(default)]
    pub index: Option<usize>,
    /// Close only a banner whose app name holds this text, ignoring
    /// letter case.
    #[serde(default)]
    pub from_app: Option<String>,
    /// Close only a banner whose text holds this text, ignoring letter
    /// case.
    #[serde(default)]
    pub title_contains: Option<String>,
    /// How many times to re-read the tree before reporting the result.
    /// Defaults to [`DEFAULT_VERIFY_ATTEMPTS`].
    #[serde(default)]
    pub verify_attempts: Option<u32>,
    /// How long to wait between two re-reads, in milliseconds.
    /// Defaults to [`DEFAULT_VERIFY_DELAY_MS`].
    #[serde(default)]
    pub verify_delay_ms: Option<u64>,
}

/// The result of a `dismiss_notification` tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DismissNotificationResponse {
    /// Whether the banner really left the screen. This is read from a
    /// fresh accessibility tree, not from the press. See PINV-35.
    pub dismissed: bool,
    /// The banner the tool acted on, as it read before the press.
    pub banner: NotificationBanner,
    /// The AX action the tool performed.
    pub action: String,
    /// How many banners are still on screen after the press.
    pub remaining: usize,
    /// How many times the tool re-read the tree.
    pub verify_attempts: u32,
}

/// Closes one notification banner, then reports whether it went away.
///
/// The tool reads the notification centre's tree, picks one banner,
/// presses that banner's own close control, and reads the tree again.
/// `dismissed` reports the second read, never the press. See PINV-35.
pub fn perform_dismiss_notification<A, P, S>(
    inspector: &A,
    performer: &P,
    sleeper: &S,
    request: &DismissNotificationRequest,
) -> Result<DismissNotificationResponse, PolarizeError>
where
    A: AccessibilityInspector,
    P: ActionPerformer,
    S: Sleeper,
{
    let app = notification_center_app();
    let (_, root) = inspector.describe(Some(&app))?;
    let all = extract_banners(&root);
    if all.is_empty() {
        return Err(NotificationError::NoBanners.into());
    }

    let matching = filtered_banners(
        &root,
        request.from_app.as_deref(),
        request.title_contains.as_deref(),
    );
    if matching.is_empty() {
        return Err(NotificationError::NoMatch {
            filter: describe_filter(request),
            count: all.len(),
        }
        .into());
    }
    let index = request.index.unwrap_or(0);
    let banner = matching
        .get(index)
        .cloned()
        .ok_or(NotificationError::IndexOutOfRange {
            index,
            matches: matching.len(),
        })?;

    let (dismiss_path, action) = match (&banner.dismiss_path, &banner.dismiss_action) {
        (Some(path), Some(action)) => (path.clone(), action.clone()),
        _ => {
            return Err(NotificationError::NoDismissControl {
                banner: banner.texts.join(" / "),
            }
            .into());
        }
    };

    performer.perform_action_at_path(Some(&app), &dismiss_path, &action)?;

    // Verification. The banner animates away, so the first re-read can
    // still carry it. Counting matching banners, rather than testing
    // whether the text is absent, keeps two identical banners honest:
    // closing one of them is a real dismiss.
    let settings = VerifySettings::resolve(request.verify_attempts, request.verify_delay_ms);
    let key = banner.key();
    let before = all.iter().filter(|other| other.key() == key).count();

    let mut dismissed = false;
    let mut remaining = all.len();
    let mut attempts = 0;
    for _ in 0..settings.attempts {
        sleeper.sleep_ms(settings.delay_ms);
        attempts += 1;
        let (_, tree) = inspector.describe(Some(&app))?;
        let now = extract_banners(&tree);
        remaining = now.len();
        if now.iter().filter(|other| other.key() == key).count() < before {
            dismissed = true;
            break;
        }
    }

    Ok(DismissNotificationResponse {
        dismissed,
        banner,
        action,
        remaining,
        verify_attempts: attempts,
    })
}

/// A short rendering of a request's filters, for an error message.
fn describe_filter(request: &DismissNotificationRequest) -> String {
    let mut parts = Vec::new();
    if let Some(app) = &request.from_app {
        parts.push(format!("from_app={app:?}"));
    }
    if let Some(title) = &request.title_contains {
        parts.push(format!("title_contains={title:?}"));
    }
    if let Some(index) = request.index {
        parts.push(format!("index={index}"));
    }
    if parts.is_empty() {
        parts.push("no filter".to_string());
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A control that clears EVERY notification is never one banner's
    /// dismiss control, even when it sits inside that banner's own
    /// container. Pressing it discards notifications the caller never
    /// named, and nothing can undo that.
    #[test]
    fn a_clear_all_inside_a_banner_is_not_its_dismiss_control() {
        let tree = AxNode {
            role: "AXGroup".to_string(),
            children: vec![AxNode {
                role: "AXGroup".to_string(),
                children: vec![
                    AxNode {
                        role: "AXStaticText".to_string(),
                        label: Some("Messages".to_string()),
                        ..AxNode::default()
                    },
                    AxNode {
                        role: "AXStaticText".to_string(),
                        label: Some("Hello there".to_string()),
                        ..AxNode::default()
                    },
                    AxNode {
                        role: "AXButton".to_string(),
                        label: Some("Clear All".to_string()),
                        actions: vec!["AXPress".to_string()],
                        ..AxNode::default()
                    },
                ],
                ..AxNode::default()
            }],
            ..AxNode::default()
        };

        let banners = extract_banners(&tree);
        assert_eq!(banners.len(), 1);
        assert_eq!(
            banners[0].dismiss_path, None,
            "a dismiss would press Clear All and wipe every notification"
        );
    }

    #[test]
    fn a_plain_close_control_inside_a_banner_still_dismisses_it() {
        // The guard must not disarm a real close control.
        let tree = AxNode {
            role: "AXGroup".to_string(),
            children: vec![AxNode {
                role: "AXGroup".to_string(),
                children: vec![
                    AxNode {
                        role: "AXStaticText".to_string(),
                        label: Some("Messages".to_string()),
                        ..AxNode::default()
                    },
                    AxNode {
                        role: "AXButton".to_string(),
                        label: Some("Close".to_string()),
                        actions: vec!["AXPress".to_string()],
                        ..AxNode::default()
                    },
                ],
                ..AxNode::default()
            }],
            ..AxNode::default()
        };
        assert_eq!(extract_banners(&tree)[0].dismiss_path, Some(vec![0, 1]));
    }

    /// The same hazard, on a tree whose banners do NOT name themselves.
    /// Here the only thing that can mark a container is its children —
    /// and the root's children include Clear All, whose label matches
    /// the "clear" hint. Without a guard the root itself reads as one
    /// banner, so both notifications merge into one record and a dismiss
    /// presses Clear All, wiping every notification on screen.
    #[test]
    fn a_group_level_clear_all_does_not_swallow_unlabelled_banners() {
        let banner = |app: &str, body: &str| AxNode {
            role: "AXGroup".to_string(),
            children: vec![
                AxNode {
                    role: "AXStaticText".to_string(),
                    label: Some(app.to_string()),
                    ..AxNode::default()
                },
                AxNode {
                    role: "AXStaticText".to_string(),
                    label: Some(body.to_string()),
                    ..AxNode::default()
                },
                AxNode {
                    role: "AXButton".to_string(),
                    label: Some("Close".to_string()),
                    actions: vec!["AXPress".to_string()],
                    ..AxNode::default()
                },
            ],
            ..AxNode::default()
        };
        let tree = AxNode {
            role: "AXGroup".to_string(),
            children: vec![
                banner("Messages", "Hello there"),
                banner("Calendar", "Standup at 10"),
                AxNode {
                    role: "AXButton".to_string(),
                    label: Some("Clear All".to_string()),
                    actions: vec!["AXPress".to_string()],
                    ..AxNode::default()
                },
            ],
            ..AxNode::default()
        };

        let banners = extract_banners(&tree);
        assert_eq!(
            banners.len(),
            2,
            "Clear All made the whole root read as one banner: {banners:#?}"
        );
        let clear_all_path = Some(vec![2]);
        assert!(
            banners.iter().all(|b| b.dismiss_path != clear_all_path),
            "a dismiss would press Clear All and wipe every notification"
        );
    }

    /// Notification Centre publishes a "Clear All" button that wipes
    /// every banner at once. It must never be mistaken for one banner's
    /// own close control: pressing it discards notifications the caller
    /// never named, and nothing can undo that.
    #[test]
    fn clear_all_is_not_a_banner_dismiss_control() {
        let tree = AxNode {
            role: "AXGroup".to_string(),
            children: vec![
                AxNode {
                    role: "AXGroup".to_string(),
                    subrole: Some("AXNotificationCenterBanner".to_string()),
                    children: vec![
                        AxNode {
                            role: "AXStaticText".to_string(),
                            label: Some("Messages".to_string()),
                            ..AxNode::default()
                        },
                        AxNode {
                            role: "AXStaticText".to_string(),
                            label: Some("Hello there".to_string()),
                            ..AxNode::default()
                        },
                        AxNode {
                            role: "AXButton".to_string(),
                            label: Some("Close".to_string()),
                            actions: vec!["AXPress".to_string()],
                            ..AxNode::default()
                        },
                    ],
                    ..AxNode::default()
                },
                AxNode {
                    role: "AXGroup".to_string(),
                    subrole: Some("AXNotificationCenterBanner".to_string()),
                    children: vec![
                        AxNode {
                            role: "AXStaticText".to_string(),
                            label: Some("Calendar".to_string()),
                            ..AxNode::default()
                        },
                        AxNode {
                            role: "AXStaticText".to_string(),
                            label: Some("Standup at 10".to_string()),
                            ..AxNode::default()
                        },
                        AxNode {
                            role: "AXButton".to_string(),
                            label: Some("Close".to_string()),
                            actions: vec!["AXPress".to_string()],
                            ..AxNode::default()
                        },
                    ],
                    ..AxNode::default()
                },
                // The group-level control that wipes everything.
                AxNode {
                    role: "AXButton".to_string(),
                    label: Some("Clear All".to_string()),
                    actions: vec!["AXPress".to_string()],
                    ..AxNode::default()
                },
            ],
            ..AxNode::default()
        };

        let banners = extract_banners(&tree);
        assert_eq!(
            banners.len(),
            2,
            "the Clear All button collapsed both banners into one unit"
        );
        for banner in &banners {
            assert!(
                banner.dismiss_path.is_some(),
                "each banner keeps its own close control"
            );
        }
        // No banner may point at the Clear All button, which is the last
        // child of the root.
        let clear_all_path = Some(vec![2]);
        assert!(
            banners.iter().all(|b| b.dismiss_path != clear_all_path),
            "a banner's dismiss would press Clear All"
        );
    }

    use crate::ax::{AxNode, NormalizedFrame};
    use crate::error::PolarizeError;
    use crate::schema::AppIdentifier;
    use crate::selector;
    use crate::traits::{AccessibilityInspector, ActionPerformer, ResolvedApp};
    use std::cell::RefCell;

    // ---- tree builders --------------------------------------------------

    fn text(label: &str) -> AxNode {
        AxNode {
            role: "AXStaticText".to_string(),
            label: Some(label.to_string()),
            ..AxNode::default()
        }
    }

    fn close_button() -> AxNode {
        AxNode {
            role: "AXButton".to_string(),
            label: Some("Close".to_string()),
            subrole: Some("AXCloseButton".to_string()),
            actions: vec!["AXPress".to_string()],
            interactive: true,
            ..AxNode::default()
        }
    }

    fn group(subrole: Option<&str>, children: Vec<AxNode>) -> AxNode {
        AxNode {
            role: "AXGroup".to_string(),
            subrole: subrole.map(str::to_string),
            children,
            ..AxNode::default()
        }
    }

    fn app_root(children: Vec<AxNode>) -> AxNode {
        AxNode {
            role: "AXApplication".to_string(),
            label: Some("Notification Center".to_string()),
            children,
            ..AxNode::default()
        }
    }

    /// The shape macOS publishes today: one window, one group per
    /// banner, three static texts, and a close button.
    fn todays_tree() -> AxNode {
        app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            label: Some("Notification Center".to_string()),
            children: vec![group(
                Some("AXNotificationCenterBanner"),
                vec![
                    text("Messages"),
                    text("Ada Lovelace"),
                    text("The engine is ready."),
                    close_button(),
                ],
            )],
            ..AxNode::default()
        }])
    }

    // ---- the target process ---------------------------------------------

    #[test]
    fn notification_center_app_names_the_documented_bundle_id() {
        let app = notification_center_app();
        assert_eq!(
            app.bundle_id.as_deref(),
            Some("com.apple.notificationcenterui")
        );
        assert_eq!(app.app_name, None);
        assert_eq!(
            NOTIFICATION_CENTER_BUNDLE_ID,
            "com.apple.notificationcenterui"
        );
    }

    // ---- extraction: today's shape --------------------------------------

    #[test]
    fn extract_banners_finds_todays_banner_shape() {
        let banners = extract_banners(&todays_tree());
        assert_eq!(banners.len(), 1);
        assert_eq!(banners[0].app.as_deref(), Some("Messages"));
        assert_eq!(banners[0].title.as_deref(), Some("Ada Lovelace"));
        assert_eq!(banners[0].body.as_deref(), Some("The engine is ready."));
    }

    #[test]
    fn extract_banners_keeps_the_raw_text_run() {
        let banners = extract_banners(&todays_tree());
        assert_eq!(
            banners[0].texts,
            vec![
                "Messages".to_string(),
                "Ada Lovelace".to_string(),
                "The engine is ready.".to_string(),
            ]
        );
    }

    #[test]
    fn extract_banners_finds_the_close_button_by_its_own_path() {
        let tree = todays_tree();
        let banners = extract_banners(&tree);
        let dismiss = banners[0].dismiss_path.clone().expect("a dismiss path");
        let node = selector::node_at_path(&tree, &dismiss).expect("a node");
        assert_eq!(node.subrole.as_deref(), Some("AXCloseButton"));
        assert_eq!(banners[0].dismiss_action.as_deref(), Some("AXPress"));
    }

    #[test]
    fn a_banner_path_reads_back_to_the_banner_root() {
        let tree = todays_tree();
        let banners = extract_banners(&tree);
        let node = selector::node_at_path(&tree, &banners[0].path).expect("a node");
        assert_eq!(node.role, "AXGroup");
    }

    #[test]
    fn a_banner_frame_comes_from_its_own_root_node() {
        let frame = NormalizedFrame {
            x: 0.7,
            y: 0.02,
            width: 0.25,
            height: 0.1,
        };
        let mut tree = todays_tree();
        tree.children[0].children[0].frame = frame;
        let banners = extract_banners(&tree);
        assert_eq!(banners[0].frame, frame);
    }

    #[test]
    fn extract_banners_finds_two_sibling_banners_in_order() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![
                group(
                    Some("AXNotificationCenterBanner"),
                    vec![text("Mail"), text("Invoice"), close_button()],
                ),
                group(
                    Some("AXNotificationCenterBanner"),
                    vec![text("Calendar"), text("Standup"), close_button()],
                ),
            ],
            ..AxNode::default()
        }]);
        let banners = extract_banners(&tree);
        assert_eq!(banners.len(), 2);
        assert_eq!(banners[0].app.as_deref(), Some("Mail"));
        assert_eq!(banners[1].app.as_deref(), Some("Calendar"));
        assert_eq!(banners[0].index, 0);
        assert_eq!(banners[1].index, 1);
    }

    // ---- extraction: shapes this code has never seen ---------------------

    /// PINV-35. A future macOS renames every subrole, wraps each banner
    /// in a list cell, and splits the text across two sub-groups. The
    /// extractor must still find one banner per cell.
    #[test]
    fn extract_banners_finds_a_future_tree_shape() {
        let cell = |app: &str, title: &str, body: &str| AxNode {
            role: "AXCell".to_string(),
            children: vec![
                group(Some("AXNotificationHeaderV4"), vec![text(app), text(title)]),
                group(Some("AXNotificationBodyV4"), vec![text(body)]),
                AxNode {
                    role: "AXButton".to_string(),
                    label: Some("Clear".to_string()),
                    subrole: Some("AXNotificationClearButtonV4".to_string()),
                    actions: vec!["AXPress".to_string()],
                    interactive: true,
                    ..AxNode::default()
                },
            ],
            ..AxNode::default()
        };
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![AxNode {
                role: "AXScrollArea".to_string(),
                children: vec![AxNode {
                    role: "AXList".to_string(),
                    children: vec![
                        cell("Slack", "#general", "ship it"),
                        cell("Reminders", "Water plants", "now"),
                    ],
                    ..AxNode::default()
                }],
                ..AxNode::default()
            }],
            ..AxNode::default()
        }]);

        let banners = extract_banners(&tree);
        assert_eq!(banners.len(), 2, "one banner per cell");
        assert_eq!(banners[0].app.as_deref(), Some("Slack"));
        assert_eq!(banners[0].title.as_deref(), Some("#general"));
        assert_eq!(banners[0].body.as_deref(), Some("ship it"));
        assert!(banners[0].dismiss_path.is_some(), "the Clear button");
        assert_eq!(banners[1].app.as_deref(), Some("Reminders"));
    }

    /// PINV-35. No subrole at all, and a role this code does not know.
    #[test]
    fn extract_banners_needs_no_subrole_string() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![AxNode {
                role: "AXUnknownFutureRole".to_string(),
                children: vec![text("Music"), text("Now playing"), close_button()],
                ..AxNode::default()
            }],
            ..AxNode::default()
        }]);
        let banners = extract_banners(&tree);
        assert_eq!(banners.len(), 1);
        assert_eq!(banners[0].app.as_deref(), Some("Music"));
    }

    /// PINV-35. A banner with no close control is still reported.
    #[test]
    fn extract_banners_reports_a_banner_with_no_dismiss_control() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![group(None, vec![text("Finder"), text("Copy done")])],
            ..AxNode::default()
        }]);
        let banners = extract_banners(&tree);
        assert_eq!(banners.len(), 1);
        assert_eq!(banners[0].dismiss_path, None);
        assert_eq!(banners[0].dismiss_action, None);
    }

    #[test]
    fn extract_banners_returns_nothing_when_no_text_exists() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![group(None, vec![])],
            ..AxNode::default()
        }]);
        assert!(extract_banners(&tree).is_empty());
    }

    #[test]
    fn extract_banners_returns_nothing_for_a_bare_application_element() {
        assert!(extract_banners(&app_root(vec![])).is_empty());
    }

    #[test]
    fn extract_banners_keeps_action_button_text_out_of_the_body() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![group(
                Some("AXNotificationCenterBanner"),
                vec![
                    text("Messages"),
                    text("Ada"),
                    text("Hello"),
                    AxNode {
                        role: "AXButton".to_string(),
                        label: Some("Reply".to_string()),
                        actions: vec!["AXPress".to_string()],
                        interactive: true,
                        ..AxNode::default()
                    },
                    close_button(),
                ],
            )],
            ..AxNode::default()
        }]);
        let banners = extract_banners(&tree);
        assert_eq!(banners[0].body.as_deref(), Some("Hello"));
        assert!(!banners[0].texts.iter().any(|t| t == "Reply"));
    }

    /// A "Reply" button must never be mistaken for the close control.
    #[test]
    fn a_plain_action_button_is_not_a_dismiss_control() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![group(
                None,
                vec![
                    text("Messages"),
                    text("Ada"),
                    AxNode {
                        role: "AXButton".to_string(),
                        label: Some("Reply".to_string()),
                        actions: vec!["AXPress".to_string()],
                        interactive: true,
                        ..AxNode::default()
                    },
                ],
            )],
            ..AxNode::default()
        }]);
        let banners = extract_banners(&tree);
        assert_eq!(banners.len(), 1);
        assert_eq!(banners[0].dismiss_path, None);
    }

    #[test]
    fn one_text_reads_as_the_title_not_the_app() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![group(None, vec![text("Backup finished")])],
            ..AxNode::default()
        }]);
        let banners = extract_banners(&tree);
        assert_eq!(banners[0].app, None);
        assert_eq!(banners[0].title.as_deref(), Some("Backup finished"));
        assert_eq!(banners[0].body, None);
    }

    #[test]
    fn two_texts_read_as_the_app_and_the_title() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![group(None, vec![text("Mail"), text("New message")])],
            ..AxNode::default()
        }]);
        let banners = extract_banners(&tree);
        assert_eq!(banners[0].app.as_deref(), Some("Mail"));
        assert_eq!(banners[0].title.as_deref(), Some("New message"));
        assert_eq!(banners[0].body, None);
    }

    #[test]
    fn four_texts_join_the_last_two_into_the_body() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![group(
                None,
                vec![
                    text("Mail"),
                    text("Ada"),
                    text("line one"),
                    text("line two"),
                ],
            )],
            ..AxNode::default()
        }]);
        let banners = extract_banners(&tree);
        assert_eq!(banners[0].body.as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn an_identifier_hint_marks_a_banner_the_structure_would_split() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![AxNode {
                role: "AXGroup".to_string(),
                identifier: Some("notification-item-3".to_string()),
                children: vec![
                    group(None, vec![text("Mail"), text("Ada")]),
                    group(None, vec![text("Hello")]),
                ],
                ..AxNode::default()
            }],
            ..AxNode::default()
        }]);
        let banners = extract_banners(&tree);
        assert_eq!(banners.len(), 1, "the hint keeps the two groups together");
        assert_eq!(banners[0].body.as_deref(), Some("Hello"));
    }

    // ---- fakes ----------------------------------------------------------

    struct FakeInspector {
        trees: RefCell<Vec<AxNode>>,
        asked: RefCell<Vec<Option<AppIdentifier>>>,
    }

    impl FakeInspector {
        fn new(trees: Vec<AxNode>) -> Self {
            Self {
                trees: RefCell::new(trees),
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl AccessibilityInspector for FakeInspector {
        fn describe(
            &self,
            app: Option<&AppIdentifier>,
        ) -> Result<(ResolvedApp, AxNode), PolarizeError> {
            self.asked.borrow_mut().push(app.cloned());
            let mut trees = self.trees.borrow_mut();
            let tree = if trees.len() > 1 {
                trees.remove(0)
            } else {
                trees[0].clone()
            };
            Ok((
                ResolvedApp {
                    name: "Notification Center".to_string(),
                    bundle_id: Some(NOTIFICATION_CENTER_BUNDLE_ID.to_string()),
                },
                tree,
            ))
        }
    }

    /// One recorded `perform_action_at_path` call: the app it
    /// addressed, the path it walked, and the action it performed.
    type RecordedCall = (Option<AppIdentifier>, Vec<usize>, String);

    #[derive(Default)]
    struct FakePerformer {
        calls: RefCell<Vec<RecordedCall>>,
    }

    impl ActionPerformer for FakePerformer {
        fn perform_action_at_path(
            &self,
            app: Option<&AppIdentifier>,
            path: &[usize],
            action: &str,
        ) -> Result<(), PolarizeError> {
            self.calls
                .borrow_mut()
                .push((app.cloned(), path.to_vec(), action.to_string()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeSleeper {
        slept: RefCell<Vec<u64>>,
    }

    impl Sleeper for FakeSleeper {
        fn sleep_ms(&self, ms: u64) {
            self.slept.borrow_mut().push(ms);
        }
    }

    // ---- describe_notifications -----------------------------------------

    #[test]
    fn describe_notifications_addresses_the_notification_centre() {
        let inspector = FakeInspector::new(vec![todays_tree()]);
        let request = DescribeNotificationsRequest::default();
        perform_describe_notifications(&inspector, &request).expect("a response");
        let asked = inspector.asked.borrow();
        assert_eq!(asked.len(), 1);
        assert_eq!(
            asked[0].as_ref().and_then(|a| a.bundle_id.as_deref()),
            Some(NOTIFICATION_CENTER_BUNDLE_ID)
        );
    }

    #[test]
    fn describe_notifications_reports_every_banner_and_a_count() {
        let inspector = FakeInspector::new(vec![todays_tree()]);
        let response =
            perform_describe_notifications(&inspector, &DescribeNotificationsRequest::default())
                .expect("a response");
        assert_eq!(response.count, 1);
        assert_eq!(response.banners.len(), 1);
        assert_eq!(response.app_name, "Notification Center");
        assert!(response.formatted.contains("Ada Lovelace"));
    }

    #[test]
    fn describe_notifications_reports_an_empty_list_without_an_error() {
        let inspector = FakeInspector::new(vec![app_root(vec![])]);
        let response =
            perform_describe_notifications(&inspector, &DescribeNotificationsRequest::default())
                .expect("a response");
        assert_eq!(response.count, 0);
        assert!(response.banners.is_empty());
        assert!(response.formatted.contains("no notification"));
    }

    #[test]
    fn describe_notifications_filters_by_app_without_case() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![
                group(None, vec![text("Mail"), text("Invoice"), close_button()]),
                group(
                    None,
                    vec![text("Calendar"), text("Standup"), close_button()],
                ),
            ],
            ..AxNode::default()
        }]);
        let inspector = FakeInspector::new(vec![tree]);
        let request = DescribeNotificationsRequest {
            from_app: Some("cal".to_string()),
        };
        let response = perform_describe_notifications(&inspector, &request).expect("a response");
        assert_eq!(response.count, 1);
        assert_eq!(response.banners[0].app.as_deref(), Some("Calendar"));
    }

    // ---- dismiss_notification -------------------------------------------

    fn empty_tree() -> AxNode {
        app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![],
            ..AxNode::default()
        }])
    }

    #[test]
    fn dismiss_presses_the_close_button_at_its_resolved_path() {
        let inspector = FakeInspector::new(vec![todays_tree(), empty_tree()]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        let response = perform_dismiss_notification(
            &inspector,
            &performer,
            &sleeper,
            &DismissNotificationRequest::default(),
        )
        .expect("a response");

        let calls = performer.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, vec![0, 0, 3]);
        assert_eq!(calls[0].2, "AXPress");
        assert_eq!(
            calls[0].0.as_ref().and_then(|a| a.bundle_id.as_deref()),
            Some(NOTIFICATION_CENTER_BUNDLE_ID),
            "a dismiss never addresses the frontmost app"
        );
        assert!(response.dismissed);
        assert_eq!(response.remaining, 0);
    }

    #[test]
    fn dismiss_reports_a_banner_that_did_not_go_away() {
        let inspector = FakeInspector::new(vec![todays_tree(), todays_tree()]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        let request = DismissNotificationRequest {
            verify_attempts: Some(1),
            ..DismissNotificationRequest::default()
        };
        let response = perform_dismiss_notification(&inspector, &performer, &sleeper, &request)
            .expect("a response");
        assert!(!response.dismissed, "the banner is still in the tree");
        assert_eq!(response.remaining, 1);
        assert_eq!(performer.calls.borrow().len(), 1, "pressed exactly once");
    }

    #[test]
    fn dismiss_re_reads_the_tree_until_the_banner_goes() {
        let inspector = FakeInspector::new(vec![
            todays_tree(),
            todays_tree(),
            todays_tree(),
            empty_tree(),
        ]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        let request = DismissNotificationRequest {
            verify_attempts: Some(3),
            verify_delay_ms: Some(40),
            ..DismissNotificationRequest::default()
        };
        let response = perform_dismiss_notification(&inspector, &performer, &sleeper, &request)
            .expect("a response");
        assert!(response.dismissed);
        assert_eq!(response.verify_attempts, 3);
        assert_eq!(sleeper.slept.borrow().as_slice(), &[40, 40, 40]);
    }

    #[test]
    fn dismiss_refuses_a_banner_with_no_close_control_and_presses_nothing() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![group(None, vec![text("Finder"), text("Copy done")])],
            ..AxNode::default()
        }]);
        let inspector = FakeInspector::new(vec![tree]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        let err = perform_dismiss_notification(
            &inspector,
            &performer,
            &sleeper,
            &DismissNotificationRequest::default(),
        )
        .expect_err("a refusal");
        assert!(err.to_string().contains("no dismiss control"));
        assert!(performer.calls.borrow().is_empty());
    }

    #[test]
    fn dismiss_selects_a_banner_by_title() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![
                group(None, vec![text("Mail"), text("Invoice"), close_button()]),
                group(
                    None,
                    vec![text("Calendar"), text("Standup"), close_button()],
                ),
            ],
            ..AxNode::default()
        }]);
        let inspector = FakeInspector::new(vec![tree, empty_tree()]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        let request = DismissNotificationRequest {
            title_contains: Some("Standup".to_string()),
            ..DismissNotificationRequest::default()
        };
        let response = perform_dismiss_notification(&inspector, &performer, &sleeper, &request)
            .expect("a response");
        assert_eq!(response.banner.app.as_deref(), Some("Calendar"));
        assert_eq!(performer.calls.borrow()[0].1, vec![0, 1, 2]);
    }

    #[test]
    fn dismiss_rejects_an_index_past_the_last_banner() {
        let inspector = FakeInspector::new(vec![todays_tree()]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        let request = DismissNotificationRequest {
            index: Some(4),
            ..DismissNotificationRequest::default()
        };
        let err = perform_dismiss_notification(&inspector, &performer, &sleeper, &request)
            .expect_err("an error");
        assert!(err.to_string().contains("index 4"));
        assert!(performer.calls.borrow().is_empty());
    }

    #[test]
    fn dismiss_reports_no_banner_at_all() {
        let inspector = FakeInspector::new(vec![empty_tree()]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        let err = perform_dismiss_notification(
            &inspector,
            &performer,
            &sleeper,
            &DismissNotificationRequest::default(),
        )
        .expect_err("an error");
        assert!(err.to_string().contains("no notification banner"));
    }

    #[test]
    fn dismiss_reports_a_filter_that_matches_nothing() {
        let inspector = FakeInspector::new(vec![todays_tree()]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        let request = DismissNotificationRequest {
            from_app: Some("Xcode".to_string()),
            ..DismissNotificationRequest::default()
        };
        let err = perform_dismiss_notification(&inspector, &performer, &sleeper, &request)
            .expect_err("an error");
        assert!(err.to_string().contains("Xcode"));
    }

    /// Two identical banners: dismissing one must not read as a failure
    /// just because a banner with the same text is still on screen.
    #[test]
    fn dismiss_counts_identical_banners_rather_than_matching_text() {
        let twin = || {
            group(
                Some("AXNotificationCenterBanner"),
                vec![text("Mail"), text("Invoice"), close_button()],
            )
        };
        let before = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![twin(), twin()],
            ..AxNode::default()
        }]);
        let after = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![twin()],
            ..AxNode::default()
        }]);
        let inspector = FakeInspector::new(vec![before, after]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        let response = perform_dismiss_notification(
            &inspector,
            &performer,
            &sleeper,
            &DismissNotificationRequest::default(),
        )
        .expect("a response");
        assert!(response.dismissed);
        assert_eq!(response.remaining, 1);
    }

    #[test]
    fn dismiss_uses_the_only_action_a_close_control_offers() {
        let tree = app_root(vec![AxNode {
            role: "AXWindow".to_string(),
            children: vec![group(
                None,
                vec![
                    text("Mail"),
                    text("Invoice"),
                    AxNode {
                        role: "AXButton".to_string(),
                        label: Some("Dismiss".to_string()),
                        actions: vec!["AXCancel".to_string()],
                        interactive: true,
                        ..AxNode::default()
                    },
                ],
            )],
            ..AxNode::default()
        }]);
        let inspector = FakeInspector::new(vec![tree, empty_tree()]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        perform_dismiss_notification(
            &inspector,
            &performer,
            &sleeper,
            &DismissNotificationRequest::default(),
        )
        .expect("a response");
        assert_eq!(performer.calls.borrow()[0].2, "AXCancel");
    }

    #[test]
    fn dismiss_clamps_its_verification_attempts() {
        let request = DismissNotificationRequest {
            verify_attempts: Some(999),
            verify_delay_ms: Some(999_999),
            ..DismissNotificationRequest::default()
        };
        let settings = VerifySettings::resolve(request.verify_attempts, request.verify_delay_ms);
        assert_eq!(settings.attempts, MAX_VERIFY_ATTEMPTS);
        assert_eq!(settings.delay_ms, MAX_VERIFY_DELAY_MS);

        let defaults = VerifySettings::resolve(None, None);
        assert_eq!(defaults.attempts, DEFAULT_VERIFY_ATTEMPTS);
        assert_eq!(defaults.delay_ms, DEFAULT_VERIFY_DELAY_MS);
    }

    #[test]
    fn zero_verification_attempts_still_re_reads_the_tree_once() {
        let inspector = FakeInspector::new(vec![todays_tree(), empty_tree()]);
        let performer = FakePerformer::default();
        let sleeper = FakeSleeper::default();
        let request = DismissNotificationRequest {
            verify_attempts: Some(0),
            ..DismissNotificationRequest::default()
        };
        let response = perform_dismiss_notification(&inspector, &performer, &sleeper, &request)
            .expect("a response");
        assert!(response.dismissed);
        assert_eq!(response.verify_attempts, 1);
    }

    // ---- errors and serde ------------------------------------------------

    #[test]
    fn a_notification_error_travels_as_a_platform_error_with_its_message() {
        let err: PolarizeError = NotificationError::NoBanners.into();
        assert!(matches!(err, PolarizeError::Platform(_)));
        assert!(err.to_string().contains("no notification banner"));
    }

    #[test]
    fn responses_round_trip_through_json() {
        let inspector = FakeInspector::new(vec![todays_tree()]);
        let response =
            perform_describe_notifications(&inspector, &DescribeNotificationsRequest::default())
                .expect("a response");
        let json = serde_json::to_string(&response).expect("json");
        let back: DescribeNotificationsResponse = serde_json::from_str(&json).expect("a response");
        assert_eq!(back, response);
    }

    #[test]
    fn a_dismiss_request_deserializes_from_an_empty_object() {
        let request: DismissNotificationRequest = serde_json::from_str("{}").expect("a request");
        assert_eq!(request, DismissNotificationRequest::default());
    }
}
