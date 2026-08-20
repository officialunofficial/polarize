//! Workspace awareness: which app is frontmost now, and a bounded wait
//! for the next `NSWorkspace` event.
//!
//! ## The design decision behind this module
//!
//! `NSWorkspace` publishes `didActivateApplicationNotification`,
//! `willSleepNotification`, `didWakeNotification`, and
//! `sessionDidResignActiveNotification`. Each one needs a run loop, in
//! the same way an `AXObserver` does (see PINV-20 and
//! `polarize-macos/src/observer.rs`).
//!
//! `polarize` is an `rmcp` stdio server. It answers discrete tool
//! calls. It has no channel to push an asynchronous event stream into,
//! and no client subscribed to one. So this module does **not** offer a
//! notification stream. It offers two tools instead:
//!
//! 1. `frontmost_app` — reads the workspace state right now. No wait,
//!    no run loop, no permission.
//! 2. `await_workspace_event` — blocks the calling tool call until the
//!    next event, or until a timeout. The tool call itself is where the
//!    event is delivered, so nothing an observer sees is dropped.
//!
//! An event that happens while no `await_workspace_event` call is
//! running is not reported. That is a real limit, and it is written
//! into the tool's own documentation rather than hidden. An API that
//! promises a complete stream and quietly loses events is worse.
//!
//! ## Why a wait watches two channels
//!
//! [`perform_await_workspace_event`] combines a real notification
//! observer with a poll of the workspace state, in the same hybrid
//! shape `crate::wait` uses. Both channels report into one list, and
//! every event says which channel saw it.
//!
//! The poll exists because nobody has yet confirmed that `NSWorkspace`
//! notifications reach a process whose main run loop never runs.
//! `polarize`'s main thread runs `tokio`, not a `CFRunLoop`. If the
//! notifications never arrive, the poll still reports an activation, a
//! wake, and a Fast User Switch, and the response says `source: poll`.
//! The first real run on macOS therefore tells a human which channel
//! works. See PINV-36.
//!
//! [`WorkspaceEventKind::WillSleep`] is the one event no poll can see.
//! A sleep that has not happened yet leaves no trace to read.
//!
//! ## What is pure here
//!
//! The notification-name table, the snapshot difference, the event
//! filter, and the whole wait policy are pure logic, and
//! `cargo test -p polarize-core` covers them. `polarize-macos` only
//! reads a snapshot and runs one observer thread.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::PolarizeError;
use crate::schema::AppIdentifier;
use crate::traits::ResolvedApp;
use crate::wait::{Clock, WaitBudget};

// ---- the event vocabulary -----------------------------------------------

/// One workspace event `polarize` can report.
///
/// Each name matches one `NSWorkspace` notification. The issue that
/// asked for this module named four of them; `SessionBecameActive` is
/// the pair of `SessionResignedActive`, and a Fast User Switch back is
/// as useful to know about as a Fast User Switch away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceEventKind {
    /// Another app came to the front.
    AppActivated,
    /// The Mac is about to sleep. Only a notification reports this.
    WillSleep,
    /// The Mac woke up.
    DidWake,
    /// Fast User Switching gave the console to another login session.
    SessionResignedActive,
    /// This login session got the console back.
    SessionBecameActive,
}

impl WorkspaceEventKind {
    /// Every kind, in the order this module reports them.
    pub const ALL: [WorkspaceEventKind; 5] = [
        WorkspaceEventKind::AppActivated,
        WorkspaceEventKind::WillSleep,
        WorkspaceEventKind::DidWake,
        WorkspaceEventKind::SessionResignedActive,
        WorkspaceEventKind::SessionBecameActive,
    ];

    /// The `NSWorkspace` notification name that carries this kind.
    ///
    /// These are literal strings, not the framework's extern symbols,
    /// for the reason `crate::ax_ffi` in `polarize-macos` gives for
    /// attribute names: the values are long-stable public API, and a
    /// literal cannot link against a wrong symbol.
    pub fn notification_name(self) -> &'static str {
        match self {
            WorkspaceEventKind::AppActivated => "NSWorkspaceDidActivateApplicationNotification",
            WorkspaceEventKind::WillSleep => "NSWorkspaceWillSleepNotification",
            WorkspaceEventKind::DidWake => "NSWorkspaceDidWakeNotification",
            WorkspaceEventKind::SessionResignedActive => {
                "NSWorkspaceSessionDidResignActiveNotification"
            }
            WorkspaceEventKind::SessionBecameActive => {
                "NSWorkspaceSessionDidBecomeActiveNotification"
            }
        }
    }

    /// The kind a notification name carries, or `None` for a name this
    /// module does not watch.
    pub fn from_notification_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.notification_name() == name)
    }

    /// Whether a poll of the workspace state can find this kind.
    ///
    /// Everything but [`Self::WillSleep`] leaves a trace a later
    /// snapshot can read. A sleep that has not happened yet does not.
    /// See PINV-36.
    pub fn is_poll_observable(self) -> bool {
        !matches!(self, WorkspaceEventKind::WillSleep)
    }
}

/// Which channel saw an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceEventSource {
    /// An `NSWorkspace` notification arrived.
    Notification,
    /// A snapshot of the workspace state differed from the one before
    /// it.
    Poll,
}

/// One app, as the workspace reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceApp {
    /// The localized display name. Empty when the platform published
    /// none.
    pub name: String,
    /// The bundle id, e.g. `"com.apple.Safari"`.
    pub bundle_id: Option<String>,
}

impl WorkspaceApp {
    /// The most precise identifier that addresses this app again, for
    /// a follow-up `describe` or `screenshot` call. A bundle id wins.
    pub fn identifier(&self) -> Option<AppIdentifier> {
        match (&self.bundle_id, self.name.is_empty()) {
            (Some(bundle_id), _) => Some(AppIdentifier {
                bundle_id: Some(bundle_id.clone()),
                app_name: None,
            }),
            (None, false) => Some(AppIdentifier {
                bundle_id: None,
                app_name: Some(self.name.clone()),
            }),
            (None, true) => None,
        }
    }
}

impl From<&ResolvedApp> for WorkspaceApp {
    fn from(app: &ResolvedApp) -> Self {
        Self {
            name: app.name.clone(),
            bundle_id: app.bundle_id.clone(),
        }
    }
}

/// One workspace event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceEvent {
    pub kind: WorkspaceEventKind,
    pub source: WorkspaceEventSource,
    /// The app an [`WorkspaceEventKind::AppActivated`] event names.
    /// `None` for every other kind.
    pub app: Option<WorkspaceApp>,
}

// ---- the workspace snapshot ---------------------------------------------

/// How far the wall clock may run ahead of the monotonic clock before
/// the difference reads as a sleep.
///
/// A Mac corrects its clock by a second or two after an NTP sync. It
/// sleeps for far longer than that. Five seconds sits well clear of
/// both.
pub const WAKE_GAP_MS: u64 = 5_000;

/// Everything a poll reads about the workspace, in one sample.
///
/// The two clocks are sampled with the rest of the state, so a
/// difference between two snapshots measures one interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    /// The app that is frontmost. `None` when the platform names none.
    pub frontmost: Option<WorkspaceApp>,
    /// Whether this login session owns the console. See PINV-23.
    pub on_console: bool,
    /// A clock that stops while the Mac sleeps.
    pub monotonic_ms: u64,
    /// A clock that keeps running while the Mac sleeps.
    pub wall_ms: u64,
}

/// Reads the workspace state. `polarize-macos` implements this over
/// `NSWorkspace` and `CGSessionCopyCurrentDictionary`.
pub trait WorkspaceInspector {
    /// One sample of the workspace state.
    fn snapshot(&self) -> Result<WorkspaceSnapshot, PolarizeError>;
}

/// Blocks until an `NSWorkspace` notification arrives.
/// `polarize-macos` implements this over `NSNotificationCenter` and
/// `CFRunLoop`, on a thread of its own — see PINV-20 and PINV-36.
pub trait WorkspaceNotificationWaiter {
    /// Blocks until at least one watched notification arrives, or until
    /// `budget` elapses. An empty list means nothing arrived.
    ///
    /// An implementation must consume the whole budget before it
    /// returns an empty list, exactly as
    /// [`crate::traits::UiChangeWaiter`] must (PINV-20). A caller
    /// counts one call as one poll interval of elapsed time.
    ///
    /// `Err` means the implementation could not observe at all. That
    /// ends the wait; see PINV-36.
    fn wait_for_workspace_notification(
        &self,
        budget: Duration,
    ) -> Result<Vec<WorkspaceEvent>, PolarizeError>;
}

/// Whether two snapshots name the same frontmost app.
///
/// A bundle id decides it when both snapshots carry one, because an app
/// can change its display name without ever losing focus. Two apps with
/// no bundle id are compared by name, which is the only thing left.
fn same_app(before: Option<&WorkspaceApp>, after: Option<&WorkspaceApp>) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => match (&before.bundle_id, &after.bundle_id) {
            (Some(before), Some(after)) => before == after,
            _ => before.name == after.name,
        },
        _ => false,
    }
}

/// # PINV-36: a workspace wait watches two channels, and names the one that saw the event
///
/// - Always: [`diff_snapshots`] derives every workspace event that a
///   pair of snapshots can prove, and marks each one
///   [`WorkspaceEventSource::Poll`]. [`perform_await_workspace_event`]
///   reports an event from either channel, and every event it returns
///   names the channel that saw it.
///   [`WorkspaceEventKind::WillSleep`] is the one kind a poll cannot
///   produce, and [`WorkspaceEventKind::is_poll_observable`] says so.
/// - Because: `NSWorkspace` delivers its notifications through a run
///   loop, and `polarize`'s main thread runs `tokio` rather than a
///   `CFRunLoop`. Nobody has confirmed that those notifications reach
///   this process at all. A wait built on notifications alone would
///   answer "nothing happened" while an app really did come to the
///   front, which a caller cannot tell from a quiet Mac. A poll of
///   `NSWorkspace.frontmostApplication` and the login-session flags
///   proves the same facts without a run loop. Naming the source is
///   what lets the first human run on real macOS see which channel
///   works.
/// - If violated: `await_workspace_event` times out on a Mac where the
///   event really happened, and nothing in the response explains it.
///   Or a `willSleep` result implies a poll can see a sleep coming,
///   which it cannot.
pub fn diff_snapshots(
    previous: &WorkspaceSnapshot,
    current: &WorkspaceSnapshot,
) -> Vec<WorkspaceEvent> {
    let mut events = Vec::new();

    // A wake first. It is the change that explains the other two.
    let monotonic = current.monotonic_ms.saturating_sub(previous.monotonic_ms);
    let wall = current.wall_ms.saturating_sub(previous.wall_ms);
    if wall.saturating_sub(monotonic) >= WAKE_GAP_MS {
        events.push(poll_event(WorkspaceEventKind::DidWake, None));
    }

    if previous.on_console != current.on_console {
        let kind = if current.on_console {
            WorkspaceEventKind::SessionBecameActive
        } else {
            WorkspaceEventKind::SessionResignedActive
        };
        events.push(poll_event(kind, None));
    }

    // An activation needs an app to name. A frontmost app that went
    // away names none, so it is not an activation.
    if !same_app(previous.frontmost.as_ref(), current.frontmost.as_ref())
        && let Some(app) = &current.frontmost
    {
        events.push(poll_event(
            WorkspaceEventKind::AppActivated,
            Some(app.clone()),
        ));
    }

    events
}

fn poll_event(kind: WorkspaceEventKind, app: Option<WorkspaceApp>) -> WorkspaceEvent {
    WorkspaceEvent {
        kind,
        source: WorkspaceEventSource::Poll,
        app,
    }
}

// ---- errors -------------------------------------------------------------

/// Why a workspace tool could not answer.
///
/// `crate::error::PolarizeError` has no variant of its own for these,
/// and this change does not own `error.rs`. Each one converts to
/// [`PolarizeError::Platform`] and keeps its whole message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceEventError {
    /// The request named an empty list of kinds, so nothing could ever
    /// match it.
    #[error("kinds must name at least one workspace event, or be left out entirely")]
    EmptyKinds,

    /// The wait reached its deadline.
    #[error("no workspace event of [{kinds}] arrived within {timeout_ms} ms")]
    Timeout { kinds: String, timeout_ms: u64 },
}

impl From<WorkspaceEventError> for PolarizeError {
    fn from(error: WorkspaceEventError) -> Self {
        PolarizeError::Platform(error.to_string())
    }
}

// ---- frontmost_app ------------------------------------------------------

/// The result of a `frontmost_app` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FrontmostAppResponse {
    /// The app that is frontmost now. `None` when the platform names
    /// none, which happens while the login window is on screen.
    pub app: Option<WorkspaceApp>,
    /// Whether this login session owns the console. `false` means Fast
    /// User Switching gave the console away, so the frontmost app here
    /// is not the app the user sees. See PINV-23.
    pub on_console: bool,
}

/// Reads which app is frontmost right now.
///
/// This tool takes no arguments and needs no permission. It reads
/// `NSWorkspace`, not the accessibility tree, not pixels, and it posts
/// no input.
pub fn perform_frontmost_app<I>(inspector: &I) -> Result<FrontmostAppResponse, PolarizeError>
where
    I: WorkspaceInspector,
{
    let snapshot = inspector.snapshot()?;
    Ok(FrontmostAppResponse {
        app: snapshot.frontmost,
        on_console: snapshot.on_console,
    })
}

// ---- await_workspace_event ----------------------------------------------

/// An `await_workspace_event` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct AwaitWorkspaceEventRequest {
    /// Which kinds end the wait. `None` means every kind. An empty
    /// list is refused, because nothing could match it.
    #[serde(default)]
    pub kinds: Option<Vec<WorkspaceEventKind>>,
    /// End the wait only when *this* app comes to the front. The filter
    /// applies to [`WorkspaceEventKind::AppActivated`] alone; a sleep or
    /// a Fast User Switch still ends the wait.
    #[serde(default)]
    pub app: Option<AppIdentifier>,
    /// How long to wait, in milliseconds. Defaults to
    /// `crate::wait::DEFAULT_TIMEOUT_MS`, capped at
    /// `crate::wait::MAX_TIMEOUT_MS`.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// How long one wait slice runs, in milliseconds. Defaults to
    /// `crate::wait::DEFAULT_POLL_INTERVAL_MS`.
    #[serde(default)]
    pub poll_interval_ms: Option<u64>,
}

/// The result of an `await_workspace_event` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AwaitWorkspaceEventResponse {
    /// The event that ended the wait, including which channel saw it.
    pub event: WorkspaceEvent,
    /// What is frontmost now, read in the same snapshot.
    pub frontmost: Option<WorkspaceApp>,
    /// How long the wait ran, in milliseconds.
    pub waited_ms: u64,
    /// How many wait slices ran.
    pub polls: u32,
}

/// Blocks until the next workspace event, or until the timeout.
///
/// See PINV-36 for why the wait watches a notification channel and a
/// poll channel at once, and [the module documentation](self) for why
/// `polarize` has no event stream.
pub fn perform_await_workspace_event<I, W, C>(
    inspector: &I,
    waiter: &W,
    clock: &C,
    request: &AwaitWorkspaceEventRequest,
) -> Result<AwaitWorkspaceEventResponse, PolarizeError>
where
    I: WorkspaceInspector,
    W: WorkspaceNotificationWaiter,
    C: Clock,
{
    let kinds: Vec<WorkspaceEventKind> = match &request.kinds {
        Some(kinds) if kinds.is_empty() => return Err(WorkspaceEventError::EmptyKinds.into()),
        Some(kinds) => kinds.clone(),
        None => WorkspaceEventKind::ALL.to_vec(),
    };
    let budget = WaitBudget::resolve(request.timeout_ms, request.poll_interval_ms);

    let start = clock.now_ms();
    let deadline = start.saturating_add(budget.timeout_ms);
    let mut previous = inspector.snapshot()?;
    let mut now = start;
    let mut polls = 0;

    while now < deadline {
        let slice = budget.poll_interval_ms.min(deadline - now);
        // A waiter failure ends the wait. The waiter is what makes a
        // slice take time, so carrying on without it would re-read the
        // workspace as fast as the CPU allows. See PINV-36.
        let notified = waiter.wait_for_workspace_notification(Duration::from_millis(slice))?;
        polls += 1;
        now = clock.now_ms();

        let current = inspector.snapshot()?;
        let events = merge_events(notified, diff_snapshots(&previous, &current), &current);
        if let Some(event) = events
            .into_iter()
            .find(|event| matches_request(event, &kinds, request.app.as_ref()))
        {
            return Ok(AwaitWorkspaceEventResponse {
                event,
                frontmost: current.frontmost,
                waited_ms: now.saturating_sub(start),
                polls,
            });
        }
        previous = current;
    }

    Err(WorkspaceEventError::Timeout {
        kinds: kinds
            .iter()
            .map(|kind| format!("{kind:?}"))
            .collect::<Vec<_>>()
            .join(", "),
        timeout_ms: budget.timeout_ms,
    }
    .into())
}

/// Puts the two channels into one list, notifications first.
///
/// A notification is the more precise report: it names the moment the
/// event happened, not the interval it happened in. So a polled event
/// of a kind a notification already reported is dropped as a duplicate.
///
/// An `AppActivated` notification that named no app is filled in from
/// the snapshot. `polarize-macos` reads the app out of the
/// notification's `userInfo`, and that read can come back empty.
fn merge_events(
    notified: Vec<WorkspaceEvent>,
    polled: Vec<WorkspaceEvent>,
    current: &WorkspaceSnapshot,
) -> Vec<WorkspaceEvent> {
    let mut events: Vec<WorkspaceEvent> = notified
        .into_iter()
        .map(|mut event| {
            if event.kind == WorkspaceEventKind::AppActivated && event.app.is_none() {
                event.app = current.frontmost.clone();
            }
            event
        })
        .collect();
    for event in polled {
        if !events.iter().any(|seen| seen.kind == event.kind) {
            events.push(event);
        }
    }
    events
}

/// Whether one event satisfies the request.
fn matches_request(
    event: &WorkspaceEvent,
    kinds: &[WorkspaceEventKind],
    app: Option<&AppIdentifier>,
) -> bool {
    if !kinds.contains(&event.kind) {
        return false;
    }
    // The app filter narrows an activation, and only an activation. A
    // caller waiting for Xcode still wants to hear that the Mac is
    // about to sleep.
    if event.kind != WorkspaceEventKind::AppActivated {
        return true;
    }
    match app {
        None => true,
        Some(identifier) => event
            .app
            .as_ref()
            .is_some_and(|app| app_matches_identifier(app, identifier)),
    }
}

/// Whether an app answers to an identifier. Both fields ignore letter
/// case, and an identifier that sets both matches on either one.
fn app_matches_identifier(app: &WorkspaceApp, identifier: &AppIdentifier) -> bool {
    let same = |left: &str, right: &str| left.eq_ignore_ascii_case(right);
    let by_bundle_id = identifier.bundle_id.as_ref().is_some_and(|wanted| {
        app.bundle_id
            .as_deref()
            .is_some_and(|actual| same(actual, wanted))
    });
    let by_name = identifier
        .app_name
        .as_ref()
        .is_some_and(|wanted| same(&app.name, wanted));
    if identifier.bundle_id.is_none() && identifier.app_name.is_none() {
        return true;
    }
    by_bundle_id || by_name
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PolarizeError;
    use crate::schema::AppIdentifier;
    use crate::wait::Clock;
    use std::cell::RefCell;
    use std::time::Duration;

    fn app(name: &str, bundle_id: Option<&str>) -> WorkspaceApp {
        WorkspaceApp {
            name: name.to_string(),
            bundle_id: bundle_id.map(str::to_string),
        }
    }

    fn snapshot(frontmost: Option<WorkspaceApp>, on_console: bool, ms: u64) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            frontmost,
            on_console,
            monotonic_ms: ms,
            wall_ms: ms,
        }
    }

    // ---- the notification-name table -------------------------------------

    #[test]
    fn every_kind_maps_to_the_notification_the_issue_names() {
        assert_eq!(
            WorkspaceEventKind::AppActivated.notification_name(),
            "NSWorkspaceDidActivateApplicationNotification"
        );
        assert_eq!(
            WorkspaceEventKind::WillSleep.notification_name(),
            "NSWorkspaceWillSleepNotification"
        );
        assert_eq!(
            WorkspaceEventKind::DidWake.notification_name(),
            "NSWorkspaceDidWakeNotification"
        );
        assert_eq!(
            WorkspaceEventKind::SessionResignedActive.notification_name(),
            "NSWorkspaceSessionDidResignActiveNotification"
        );
        assert_eq!(
            WorkspaceEventKind::SessionBecameActive.notification_name(),
            "NSWorkspaceSessionDidBecomeActiveNotification"
        );
    }

    #[test]
    fn every_notification_name_reads_back_to_its_kind() {
        for kind in WorkspaceEventKind::ALL {
            assert_eq!(
                WorkspaceEventKind::from_notification_name(kind.notification_name()),
                Some(kind)
            );
        }
    }

    #[test]
    fn an_unknown_notification_name_maps_to_nothing() {
        assert_eq!(
            WorkspaceEventKind::from_notification_name(
                "NSWorkspaceDidLaunchApplicationNotification"
            ),
            None
        );
        assert_eq!(WorkspaceEventKind::from_notification_name(""), None);
    }

    /// PINV-36. A sleep that has not happened yet cannot be polled for.
    #[test]
    fn will_sleep_is_the_one_kind_a_poll_cannot_see() {
        for kind in WorkspaceEventKind::ALL {
            assert_eq!(
                kind.is_poll_observable(),
                kind != WorkspaceEventKind::WillSleep,
                "{kind:?}"
            );
        }
    }

    // ---- the snapshot diff -----------------------------------------------

    #[test]
    fn an_unchanged_snapshot_reports_no_event() {
        let before = snapshot(Some(app("Safari", Some("com.apple.Safari"))), true, 0);
        let after = snapshot(Some(app("Safari", Some("com.apple.Safari"))), true, 250);
        assert!(diff_snapshots(&before, &after).is_empty());
    }

    #[test]
    fn a_new_frontmost_app_reports_an_activation() {
        let before = snapshot(Some(app("Safari", Some("com.apple.Safari"))), true, 0);
        let after = snapshot(Some(app("Mail", Some("com.apple.mail"))), true, 250);
        let events = diff_snapshots(&before, &after);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, WorkspaceEventKind::AppActivated);
        assert_eq!(events[0].source, WorkspaceEventSource::Poll);
        assert_eq!(
            events[0].app.as_ref().map(|a| a.name.as_str()),
            Some("Mail")
        );
    }

    #[test]
    fn a_renamed_app_with_the_same_bundle_id_is_not_an_activation() {
        let before = snapshot(Some(app("Safari", Some("com.apple.Safari"))), true, 0);
        let after = snapshot(
            Some(app("Safari Technology Preview", Some("com.apple.Safari"))),
            true,
            250,
        );
        assert!(diff_snapshots(&before, &after).is_empty());
    }

    #[test]
    fn losing_the_console_reports_a_resigned_session() {
        let before = snapshot(None, true, 0);
        let after = snapshot(None, false, 250);
        let events = diff_snapshots(&before, &after);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, WorkspaceEventKind::SessionResignedActive);
    }

    #[test]
    fn regaining_the_console_reports_an_active_session() {
        let before = snapshot(None, false, 0);
        let after = snapshot(None, true, 250);
        let events = diff_snapshots(&before, &after);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, WorkspaceEventKind::SessionBecameActive);
    }

    /// PINV-36. The monotonic clock stops while a Mac sleeps; the wall
    /// clock does not. The gap between them is the sleep.
    #[test]
    fn a_wall_clock_jump_past_the_monotonic_clock_reports_a_wake() {
        let before = WorkspaceSnapshot {
            frontmost: None,
            on_console: true,
            monotonic_ms: 1_000,
            wall_ms: 1_000,
        };
        let after = WorkspaceSnapshot {
            frontmost: None,
            on_console: true,
            monotonic_ms: 1_250,
            wall_ms: 601_250,
        };
        let events = diff_snapshots(&before, &after);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, WorkspaceEventKind::DidWake);
        assert_eq!(events[0].source, WorkspaceEventSource::Poll);
    }

    #[test]
    fn a_small_clock_correction_is_not_a_wake() {
        let before = WorkspaceSnapshot {
            frontmost: None,
            on_console: true,
            monotonic_ms: 1_000,
            wall_ms: 1_000,
        };
        let after = WorkspaceSnapshot {
            frontmost: None,
            on_console: true,
            monotonic_ms: 1_250,
            wall_ms: 2_250,
        };
        assert!(diff_snapshots(&before, &after).is_empty());
        assert_eq!(WAKE_GAP_MS, 5_000);
    }

    #[test]
    fn a_wake_a_switch_and_an_activation_all_report_together() {
        let before = WorkspaceSnapshot {
            frontmost: Some(app("Safari", Some("com.apple.Safari"))),
            on_console: false,
            monotonic_ms: 0,
            wall_ms: 0,
        };
        let after = WorkspaceSnapshot {
            frontmost: Some(app("Mail", Some("com.apple.mail"))),
            on_console: true,
            monotonic_ms: 250,
            wall_ms: 600_250,
        };
        let kinds: Vec<_> = diff_snapshots(&before, &after)
            .into_iter()
            .map(|event| event.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                WorkspaceEventKind::DidWake,
                WorkspaceEventKind::SessionBecameActive,
                WorkspaceEventKind::AppActivated,
            ]
        );
    }

    #[test]
    fn a_poll_never_reports_a_will_sleep() {
        let before = WorkspaceSnapshot {
            frontmost: Some(app("Safari", None)),
            on_console: true,
            monotonic_ms: 0,
            wall_ms: 0,
        };
        let after = WorkspaceSnapshot {
            frontmost: None,
            on_console: false,
            monotonic_ms: 250,
            wall_ms: 900_250,
        };
        let events = diff_snapshots(&before, &after);
        assert!(
            !events
                .iter()
                .any(|event| event.kind == WorkspaceEventKind::WillSleep)
        );
    }

    // ---- fakes ------------------------------------------------------------

    struct FakeInspector {
        snapshots: RefCell<Vec<WorkspaceSnapshot>>,
        calls: RefCell<u32>,
        fail: bool,
    }

    impl FakeInspector {
        fn new(snapshots: Vec<WorkspaceSnapshot>) -> Self {
            Self {
                snapshots: RefCell::new(snapshots),
                calls: RefCell::new(0),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                snapshots: RefCell::new(Vec::new()),
                calls: RefCell::new(0),
                fail: true,
            }
        }
    }

    impl WorkspaceInspector for FakeInspector {
        fn snapshot(&self) -> Result<WorkspaceSnapshot, PolarizeError> {
            if self.fail {
                return Err(PolarizeError::Platform("no workspace".to_string()));
            }
            *self.calls.borrow_mut() += 1;
            let mut snapshots = self.snapshots.borrow_mut();
            Ok(if snapshots.len() > 1 {
                snapshots.remove(0)
            } else {
                snapshots[0].clone()
            })
        }
    }

    /// Reports the events it was given, one slice at a time, and
    /// records every budget it was handed.
    struct FakeWaiter {
        slices: RefCell<Vec<Vec<WorkspaceEvent>>>,
        budgets: RefCell<Vec<u64>>,
        fail: bool,
    }

    impl FakeWaiter {
        fn new(slices: Vec<Vec<WorkspaceEvent>>) -> Self {
            Self {
                slices: RefCell::new(slices),
                budgets: RefCell::new(Vec::new()),
                fail: false,
            }
        }

        fn silent() -> Self {
            Self::new(Vec::new())
        }

        fn failing() -> Self {
            Self {
                slices: RefCell::new(Vec::new()),
                budgets: RefCell::new(Vec::new()),
                fail: true,
            }
        }
    }

    impl WorkspaceNotificationWaiter for FakeWaiter {
        fn wait_for_workspace_notification(
            &self,
            budget: Duration,
        ) -> Result<Vec<WorkspaceEvent>, PolarizeError> {
            self.budgets.borrow_mut().push(budget.as_millis() as u64);
            if self.fail {
                return Err(PolarizeError::Platform(
                    "NSWorkspace observer thread refused to start".to_string(),
                ));
            }
            let mut slices = self.slices.borrow_mut();
            Ok(if slices.is_empty() {
                Vec::new()
            } else {
                slices.remove(0)
            })
        }
    }

    /// Advances by one poll interval each time the fake waiter is asked
    /// to wait, so no test sleeps.
    struct FakeClock {
        ms: RefCell<u64>,
        step: u64,
    }

    impl FakeClock {
        fn new(step: u64) -> Self {
            Self {
                ms: RefCell::new(0),
                step,
            }
        }
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            let now = *self.ms.borrow();
            *self.ms.borrow_mut() = now + self.step;
            now
        }
    }

    fn notified(kind: WorkspaceEventKind, app: Option<WorkspaceApp>) -> WorkspaceEvent {
        WorkspaceEvent {
            kind,
            source: WorkspaceEventSource::Notification,
            app,
        }
    }

    // ---- frontmost_app ----------------------------------------------------

    #[test]
    fn frontmost_app_reports_the_name_the_bundle_id_and_the_console() {
        let inspector = FakeInspector::new(vec![snapshot(
            Some(app("Safari", Some("com.apple.Safari"))),
            true,
            0,
        )]);
        let response = perform_frontmost_app(&inspector).expect("a response");
        assert_eq!(
            response.app.as_ref().map(|a| a.name.as_str()),
            Some("Safari")
        );
        assert_eq!(
            response.app.as_ref().and_then(|a| a.bundle_id.as_deref()),
            Some("com.apple.Safari")
        );
        assert!(response.on_console);
    }

    #[test]
    fn frontmost_app_reports_no_app_without_an_error() {
        let inspector = FakeInspector::new(vec![snapshot(None, true, 0)]);
        let response = perform_frontmost_app(&inspector).expect("a response");
        assert_eq!(response.app, None);
    }

    #[test]
    fn frontmost_app_passes_a_platform_failure_on() {
        let inspector = FakeInspector::failing();
        let err = perform_frontmost_app(&inspector).expect_err("an error");
        assert!(err.to_string().contains("no workspace"));
    }

    #[test]
    fn a_workspace_app_builds_an_identifier_that_prefers_the_bundle_id() {
        let with_bundle = app("Safari", Some("com.apple.Safari"));
        assert_eq!(
            with_bundle.identifier(),
            Some(AppIdentifier {
                bundle_id: Some("com.apple.Safari".to_string()),
                app_name: None,
            })
        );
        let without = app("Safari", None);
        assert_eq!(
            without.identifier(),
            Some(AppIdentifier {
                bundle_id: None,
                app_name: Some("Safari".to_string()),
            })
        );
        assert_eq!(app("", None).identifier(), None);
    }

    // ---- await_workspace_event --------------------------------------------

    fn any_request() -> AwaitWorkspaceEventRequest {
        AwaitWorkspaceEventRequest::default()
    }

    #[test]
    fn a_notification_ends_the_wait_at_once() {
        let inspector = FakeInspector::new(vec![snapshot(None, true, 0)]);
        let waiter = FakeWaiter::new(vec![vec![notified(WorkspaceEventKind::WillSleep, None)]]);
        let clock = FakeClock::new(0);
        let response = perform_await_workspace_event(&inspector, &waiter, &clock, &any_request())
            .expect("a response");
        assert_eq!(response.event.kind, WorkspaceEventKind::WillSleep);
        assert_eq!(response.event.source, WorkspaceEventSource::Notification);
        assert_eq!(response.polls, 1);
    }

    #[test]
    fn a_poll_finds_an_activation_the_notification_never_reported() {
        let inspector = FakeInspector::new(vec![
            snapshot(Some(app("Safari", Some("com.apple.Safari"))), true, 0),
            snapshot(Some(app("Mail", Some("com.apple.mail"))), true, 250),
        ]);
        let waiter = FakeWaiter::silent();
        let clock = FakeClock::new(0);
        let response = perform_await_workspace_event(&inspector, &waiter, &clock, &any_request())
            .expect("a response");
        assert_eq!(response.event.kind, WorkspaceEventKind::AppActivated);
        assert_eq!(response.event.source, WorkspaceEventSource::Poll);
        assert_eq!(
            response.event.app.as_ref().map(|a| a.name.as_str()),
            Some("Mail")
        );
    }

    #[test]
    fn a_notification_with_no_app_is_filled_in_from_the_snapshot() {
        let inspector = FakeInspector::new(vec![
            snapshot(Some(app("Safari", Some("com.apple.Safari"))), true, 0),
            snapshot(Some(app("Mail", Some("com.apple.mail"))), true, 250),
        ]);
        let waiter = FakeWaiter::new(vec![vec![notified(WorkspaceEventKind::AppActivated, None)]]);
        let clock = FakeClock::new(0);
        let response = perform_await_workspace_event(&inspector, &waiter, &clock, &any_request())
            .expect("a response");
        assert_eq!(response.event.source, WorkspaceEventSource::Notification);
        assert_eq!(
            response.event.app.as_ref().map(|a| a.name.as_str()),
            Some("Mail")
        );
    }

    #[test]
    fn a_kind_the_caller_did_not_ask_for_never_ends_the_wait() {
        let inspector = FakeInspector::new(vec![
            snapshot(Some(app("Safari", None)), true, 0),
            snapshot(Some(app("Mail", None)), true, 250),
        ]);
        let waiter = FakeWaiter::silent();
        let clock = FakeClock::new(250);
        let request = AwaitWorkspaceEventRequest {
            kinds: Some(vec![WorkspaceEventKind::DidWake]),
            timeout_ms: Some(500),
            ..any_request()
        };
        let err = perform_await_workspace_event(&inspector, &waiter, &clock, &request)
            .expect_err("a timeout");
        assert!(err.to_string().contains("DidWake"));
    }

    #[test]
    fn an_app_filter_waits_for_that_app_and_no_other() {
        let inspector = FakeInspector::new(vec![
            snapshot(Some(app("Safari", Some("com.apple.Safari"))), true, 0),
            snapshot(Some(app("Mail", Some("com.apple.mail"))), true, 250),
            snapshot(Some(app("Xcode", Some("com.apple.dt.Xcode"))), true, 500),
        ]);
        let waiter = FakeWaiter::silent();
        let clock = FakeClock::new(0);
        let request = AwaitWorkspaceEventRequest {
            app: Some(AppIdentifier {
                bundle_id: Some("com.apple.dt.Xcode".to_string()),
                app_name: None,
            }),
            ..any_request()
        };
        let response = perform_await_workspace_event(&inspector, &waiter, &clock, &request)
            .expect("a response");
        assert_eq!(
            response.event.app.as_ref().map(|a| a.name.as_str()),
            Some("Xcode")
        );
        assert_eq!(response.polls, 2);
    }

    #[test]
    fn an_app_filter_matches_a_display_name_without_case() {
        let inspector = FakeInspector::new(vec![
            snapshot(Some(app("Safari", None)), true, 0),
            snapshot(Some(app("TextEdit", None)), true, 250),
        ]);
        let waiter = FakeWaiter::silent();
        let clock = FakeClock::new(0);
        let request = AwaitWorkspaceEventRequest {
            app: Some(AppIdentifier {
                bundle_id: None,
                app_name: Some("textedit".to_string()),
            }),
            ..any_request()
        };
        let response = perform_await_workspace_event(&inspector, &waiter, &clock, &request)
            .expect("a response");
        assert_eq!(response.event.kind, WorkspaceEventKind::AppActivated);
    }

    /// An app filter narrows an activation. It must not hide a sleep.
    #[test]
    fn an_app_filter_leaves_the_other_kinds_alone() {
        let inspector = FakeInspector::new(vec![snapshot(Some(app("Safari", None)), true, 0)]);
        let waiter = FakeWaiter::new(vec![vec![notified(WorkspaceEventKind::WillSleep, None)]]);
        let clock = FakeClock::new(0);
        let request = AwaitWorkspaceEventRequest {
            app: Some(AppIdentifier {
                bundle_id: None,
                app_name: Some("Xcode".to_string()),
            }),
            ..any_request()
        };
        let response = perform_await_workspace_event(&inspector, &waiter, &clock, &request)
            .expect("a response");
        assert_eq!(response.event.kind, WorkspaceEventKind::WillSleep);
    }

    #[test]
    fn a_wait_hands_the_waiter_one_poll_interval_at_a_time() {
        let inspector = FakeInspector::new(vec![snapshot(None, true, 0)]);
        let waiter = FakeWaiter::silent();
        let clock = FakeClock::new(250);
        let request = AwaitWorkspaceEventRequest {
            timeout_ms: Some(600),
            poll_interval_ms: Some(250),
            ..any_request()
        };
        perform_await_workspace_event(&inspector, &waiter, &clock, &request)
            .expect_err("a timeout");
        assert_eq!(waiter.budgets.borrow().as_slice(), &[250, 250, 100]);
    }

    #[test]
    fn a_zero_timeout_still_reads_one_snapshot_and_waits_for_nothing() {
        let inspector = FakeInspector::new(vec![snapshot(None, true, 0)]);
        let waiter = FakeWaiter::silent();
        let clock = FakeClock::new(0);
        let request = AwaitWorkspaceEventRequest {
            timeout_ms: Some(0),
            ..any_request()
        };
        let err = perform_await_workspace_event(&inspector, &waiter, &clock, &request)
            .expect_err("a timeout");
        assert!(err.to_string().contains("0 ms"));
        assert!(waiter.budgets.borrow().is_empty());
        assert_eq!(*inspector.calls.borrow(), 1);
    }

    /// PINV-36. A waiter that cannot observe at all ends the wait with
    /// its own error. It never degrades into a loop with no delay.
    #[test]
    fn a_waiter_that_fails_ends_the_wait_with_that_error() {
        let inspector = FakeInspector::new(vec![snapshot(None, true, 0)]);
        let waiter = FakeWaiter::failing();
        let clock = FakeClock::new(0);
        let err = perform_await_workspace_event(&inspector, &waiter, &clock, &any_request())
            .expect_err("an error");
        assert!(err.to_string().contains("observer thread refused to start"));
        assert_eq!(waiter.budgets.borrow().len(), 1, "asked once, then gave up");
    }

    #[test]
    fn an_empty_kind_list_is_refused_before_any_wait() {
        let inspector = FakeInspector::new(vec![snapshot(None, true, 0)]);
        let waiter = FakeWaiter::silent();
        let clock = FakeClock::new(0);
        let request = AwaitWorkspaceEventRequest {
            kinds: Some(Vec::new()),
            ..any_request()
        };
        let err = perform_await_workspace_event(&inspector, &waiter, &clock, &request)
            .expect_err("a refusal");
        assert!(err.to_string().contains("at least one"));
        assert!(waiter.budgets.borrow().is_empty());
    }

    #[test]
    fn a_wait_reports_how_long_it_waited_and_what_is_frontmost_now() {
        let inspector = FakeInspector::new(vec![
            snapshot(Some(app("Safari", None)), true, 0),
            snapshot(Some(app("Mail", None)), true, 250),
        ]);
        let waiter = FakeWaiter::silent();
        let clock = FakeClock::new(250);
        let response = perform_await_workspace_event(&inspector, &waiter, &clock, &any_request())
            .expect("a response");
        assert_eq!(
            response.frontmost.as_ref().map(|a| a.name.as_str()),
            Some("Mail")
        );
        assert!(response.waited_ms > 0);
    }

    #[test]
    fn a_snapshot_failure_ends_the_wait() {
        let inspector = FakeInspector::failing();
        let waiter = FakeWaiter::silent();
        let clock = FakeClock::new(0);
        let err = perform_await_workspace_event(&inspector, &waiter, &clock, &any_request())
            .expect_err("an error");
        assert!(err.to_string().contains("no workspace"));
    }

    #[test]
    fn a_workspace_error_travels_as_a_platform_error() {
        let err: PolarizeError = WorkspaceEventError::EmptyKinds.into();
        assert!(matches!(err, PolarizeError::Platform(_)));
        assert!(err.to_string().contains("at least one"));
    }

    #[test]
    fn the_request_and_the_response_round_trip_through_json() {
        let request: AwaitWorkspaceEventRequest = serde_json::from_str("{}").expect("a request");
        assert_eq!(request, AwaitWorkspaceEventRequest::default());

        let response = AwaitWorkspaceEventResponse {
            event: notified(WorkspaceEventKind::DidWake, None),
            frontmost: Some(app("Safari", Some("com.apple.Safari"))),
            waited_ms: 12,
            polls: 1,
        };
        let json = serde_json::to_string(&response).expect("json");
        let back: AwaitWorkspaceEventResponse = serde_json::from_str(&json).expect("a response");
        assert_eq!(back, response);
        assert!(json.contains("did_wake"), "kinds serialize in snake_case");
    }
}
