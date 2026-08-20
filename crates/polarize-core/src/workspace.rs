//! The four workspace tools: `list_windows`, `app_launch`, `app_quit`,
//! and `list_displays`.
//!
//! ## Why `list_windows` joins two lists
//!
//! macOS publishes windows through two APIs, and neither one is enough
//! on its own.
//!
//! The accessibility tree (`kAXWindowsAttribute`) knows an app's own view
//! of its windows: the title the app set, the frame, whether a window is
//! the main one, whether it is minimized. It has no durable window id,
//! and it says nothing about Spaces.
//!
//! `CGWindowListCopyWindowInfo` knows the window server's view: a durable
//! `kCGWindowNumber`, the owner process id, the compositor's bounds, the
//! window layer, and `kCGWindowIsOnscreen`. That last flag is the most
//! public Spaces awareness macOS gives. It reports whether a window sits
//! on the Space the user is looking at right now. macOS publishes no
//! supported Space id at all.
//!
//! A caller needs both halves at once, so this module joins them. The
//! join is pure logic, and [`merge_window_lists`] holds all of it
//! (PINV-30). `polarize-macos` only fetches the two lists.
//!
//! ## Permissions
//!
//! These four tools do not need the same grants, and
//! [`crate::permission::workspace_tool_permission`] says which needs
//! what. `list_windows` needs Accessibility, for its accessibility half
//! only. `app_launch`, `app_quit`, and `list_displays` need no macOS
//! permission at all.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::coords::{PixelPoint, PixelRect, PixelSize};
use crate::error::PolarizeError;
use crate::schema::AppIdentifier;
use crate::traits::{AppLifecycle, DisplayLister, WindowLister};
use crate::wait::Clock;

// ---- constants ----------------------------------------------------------

/// How far apart two frames may be and still name the same window, in
/// pixels, per edge.
///
/// The accessibility tree and the window server round a window's frame
/// independently, so the same window can differ by a fraction of a pixel
/// between the two lists. One pixel absorbs that without letting two
/// genuinely different windows pair up.
pub const FRAME_TOLERANCE_PX: f64 = 1.0;

/// The window layer an ordinary application window sits on. The menu
/// bar, the Dock, and other system furniture use other layers.
pub const NORMAL_WINDOW_LAYER: i32 = 0;

/// How long [`perform_app_launch`] waits for a launched app to appear,
/// in milliseconds, when the caller names no timeout.
pub const DEFAULT_LAUNCH_TIMEOUT_MS: u64 = 10_000;

/// How long [`perform_app_quit`] waits for an app to exit, in
/// milliseconds, when the caller names no timeout.
pub const DEFAULT_QUIT_TIMEOUT_MS: u64 = 5_000;

/// The largest timeout [`perform_app_launch`] and [`perform_app_quit`]
/// accept, in milliseconds. A larger request is clamped to this.
pub const MAX_LIFECYCLE_TIMEOUT_MS: u64 = 120_000;

/// How often [`perform_app_launch`] and [`perform_app_quit`] re-check the
/// app, in milliseconds.
pub const LIFECYCLE_POLL_INTERVAL_MS: u64 = 100;

// ---- shared geometry ----------------------------------------------------

/// A rectangle in the global display pixel space, flattened for JSON.
///
/// This carries the same rectangle as [`crate::coords::PixelRect`], in
/// the same space: pixels, origin at the top left of the main display,
/// exactly what [`crate::traits::WindowManager::resolve_target_rect`]
/// returns. It exists as a separate type only because a tool response
/// needs flat fields and a `JsonSchema` derive.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct PixelFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl PixelFrame {
    /// Builds a frame from its four numbers.
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The same rectangle as a [`PixelRect`].
    pub fn to_pixel_rect(self) -> PixelRect {
        PixelRect {
            origin: PixelPoint {
                x: self.x,
                y: self.y,
            },
            size: PixelSize {
                width: self.width,
                height: self.height,
            },
        }
    }

    /// Whether `self` and `other` name the same rectangle, within
    /// [`FRAME_TOLERANCE_PX`] on every edge.
    pub fn matches(self, other: Self) -> bool {
        let close = |a: f64, b: f64| (a - b).abs() <= FRAME_TOLERANCE_PX;
        close(self.x, other.x)
            && close(self.y, other.y)
            && close(self.width, other.width)
            && close(self.height, other.height)
    }
}

impl From<PixelRect> for PixelFrame {
    fn from(rect: PixelRect) -> Self {
        Self {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        }
    }
}

// ---- what the platform hands over ---------------------------------------

/// One window, as `kAXWindowsAttribute` reports it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AxWindow {
    pub owner_pid: i32,
    /// `AXTitle`. `None` when the app publishes none.
    pub title: Option<String>,
    /// `AXPosition` and `AXSize`, in global pixels.
    pub frame: PixelFrame,
    /// `AXMain` — the app's main window.
    pub main: bool,
    /// `AXMinimized`.
    pub minimized: bool,
}

/// One window, as `CGWindowListCopyWindowInfo` reports it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServerWindow {
    /// `kCGWindowNumber`. This is the durable window id, stable for the
    /// life of the window.
    pub window_id: u32,
    /// `kCGWindowOwnerPID`.
    pub owner_pid: i32,
    /// `kCGWindowOwnerName`.
    pub owner_name: Option<String>,
    /// `kCGWindowName`. macOS hides this from a process without Screen
    /// Recording permission, so it is often `None` even for a titled
    /// window.
    pub title: Option<String>,
    /// `kCGWindowBounds`.
    pub frame: PixelFrame,
    /// `kCGWindowIsOnscreen` — the window sits on the Space the user is
    /// looking at now.
    pub on_screen: bool,
    /// `kCGWindowLayer`. [`NORMAL_WINDOW_LAYER`] is an ordinary app
    /// window.
    pub layer: i32,
}

/// One running app, reduced to the fields a window record names.
///
/// This is [`crate::traits::ResolvedApp`] plus the process id, which is
/// what joins an app to its windows.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct RunningApp {
    pub pid: i32,
    /// The localized display name. Empty when the platform published
    /// none.
    pub name: String,
    /// The bundle id. `None` for a raw binary, or an app bundle that
    /// declares no identifier.
    pub bundle_id: Option<String>,
}

/// One display, as `CGGetActiveDisplayList` and `CGDisplayBounds` report
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DisplayInfo {
    /// The `CGDirectDisplayID`. `screenshot` and `tap` both accept this
    /// as their `display_id`.
    pub display_id: u32,
    /// `CGDisplayBounds`, in the global pixel space.
    pub frame: PixelFrame,
    /// The backing scale factor: 2.0 on a Retina display, 1.0 on a
    /// standard one.
    pub scale_factor: f64,
    /// Whether this is the main display, the one that holds the menu
    /// bar.
    pub is_main: bool,
}

// ---- the joined record --------------------------------------------------

/// Which list reported a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowSource {
    /// Both lists reported it. This record carries every field.
    Both,
    /// Only the accessibility tree reported it. It has no window id and
    /// no Space fact.
    AccessibilityOnly,
    /// Only the window server reported it. It has no `main` or
    /// `minimized` fact.
    WindowServerOnly,
}

/// One window, after the join.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WindowRecord {
    /// `kCGWindowNumber`, the durable window id. `None` when only the
    /// accessibility tree reported this window.
    pub window_id: Option<u32>,
    pub owner_pid: i32,
    /// The owning app's localized name, from the running-app list, or
    /// from `kCGWindowOwnerName` when no running app matched the pid.
    pub app_name: Option<String>,
    /// The owning app's bundle id. Prefer this over `app_name` to
    /// address the app again — see PINV-18.
    pub bundle_id: Option<String>,
    pub title: Option<String>,
    pub frame: PixelFrame,
    /// `kCGWindowIsOnscreen`: the window is on the Space the user is
    /// looking at now. `None` when only the accessibility tree reported
    /// this window, so the fact is unknown rather than false.
    pub on_current_space: Option<bool>,
    /// `AXMinimized`. `None` when only the window server reported it.
    pub minimized: Option<bool>,
    /// `AXMain`. `None` when only the window server reported it.
    pub main: Option<bool>,
    /// `kCGWindowLayer`. `None` when only the accessibility tree
    /// reported it.
    pub layer: Option<i32>,
    pub source: WindowSource,
}

/// # PINV-30: the window join matches on one app, then title, then frame, and never invents a match
///
/// - Always: [`merge_window_lists`] pairs an [`AxWindow`] with a
///   [`ServerWindow`] only when both report the same `owner_pid`. Within
///   one app it makes three passes in order: same title and same frame,
///   then same title, then same frame. Each server window is claimed at
///   most once. A window that no pass pairs stays in the result on its
///   own, marked [`WindowSource::AccessibilityOnly`] or
///   [`WindowSource::WindowServerOnly`], with the fields the missing
///   list would have supplied left as `None`.
/// - Because: the two lists disagree by design. A window can be missing
///   from the accessibility half because the app publishes nothing for
///   it, and missing from the window-server half because it is on
///   another Space and the list excludes it. A caller must still see
///   both. The three passes exist because neither key alone is unique: a
///   document app happily shows several windows titled "Untitled", and
///   macOS hides `kCGWindowName` from a process without Screen Recording
///   permission, which leaves title matching with nothing to work on.
///   Dropping an unpaired window instead would silently hide real
///   windows, and pairing on title alone would hand a caller the wrong
///   `window_id` for a `screenshot` or `tap` call that follows.
/// - If violated: `list_windows` reports a durable `window_id` that
///   belongs to a different window, or drops the very window the caller
///   was looking for and reports success.
pub fn merge_window_lists(
    ax_windows: &[AxWindow],
    server_windows: &[ServerWindow],
    running_apps: &[RunningApp],
) -> Vec<WindowRecord> {
    type MatchRule = fn(&AxWindow, &ServerWindow) -> bool;
    const RULES: [MatchRule; 3] = [
        |ax, server| same_title(ax, server) && ax.frame.matches(server.frame),
        same_title,
        |ax, server| ax.frame.matches(server.frame),
    ];

    let mut claimed = vec![false; server_windows.len()];
    let mut paired: Vec<Option<usize>> = vec![None; ax_windows.len()];

    for rule in RULES {
        for (ax_index, ax_window) in ax_windows.iter().enumerate() {
            if paired[ax_index].is_some() {
                continue;
            }
            let found = server_windows
                .iter()
                .enumerate()
                .position(|(index, server)| {
                    !claimed[index]
                        && server.owner_pid == ax_window.owner_pid
                        && rule(ax_window, server)
                });
            if let Some(server_index) = found {
                claimed[server_index] = true;
                paired[ax_index] = Some(server_index);
            }
        }
    }

    let mut records: Vec<WindowRecord> = Vec::with_capacity(ax_windows.len());
    for (ax_index, ax_window) in ax_windows.iter().enumerate() {
        let server = paired[ax_index].map(|index| &server_windows[index]);
        records.push(join_one(ax_window, server, running_apps));
    }
    for (index, server) in server_windows.iter().enumerate() {
        if !claimed[index] {
            records.push(server_only(server, running_apps));
        }
    }
    records
}

/// Whether two windows publish the same title. An absent title and an
/// empty title mean the same thing, so both read as "no title", and two
/// untitled windows do match.
fn same_title(ax_window: &AxWindow, server: &ServerWindow) -> bool {
    title_key(&ax_window.title) == title_key(&server.title)
}

/// Reads a title down to what a comparison should use: `None` for both
/// an absent title and an empty one.
fn title_key(title: &Option<String>) -> Option<&str> {
    title.as_deref().filter(|value| !value.is_empty())
}

/// Looks an app up by process id in the running-app list.
fn app_for_pid(running_apps: &[RunningApp], pid: i32) -> Option<&RunningApp> {
    running_apps.iter().find(|app| app.pid == pid)
}

/// Builds a record for a window the accessibility tree reported, with or
/// without a window-server partner.
fn join_one(
    ax_window: &AxWindow,
    server: Option<&ServerWindow>,
    running_apps: &[RunningApp],
) -> WindowRecord {
    let app = app_for_pid(running_apps, ax_window.owner_pid);
    let title = title_key(&ax_window.title)
        .map(str::to_string)
        .or_else(|| server.and_then(|s| title_key(&s.title).map(str::to_string)));
    WindowRecord {
        window_id: server.map(|s| s.window_id),
        owner_pid: ax_window.owner_pid,
        app_name: app
            .map(|a| a.name.clone())
            .or_else(|| server.and_then(|s| s.owner_name.clone())),
        bundle_id: app.and_then(|a| a.bundle_id.clone()),
        title,
        // The window server's bounds win, because that is the rectangle
        // the compositor really draws, and it is the same rectangle
        // `resolve_target_rect` returns for a `Window` target.
        frame: server.map(|s| s.frame).unwrap_or(ax_window.frame),
        on_current_space: server.map(|s| s.on_screen),
        minimized: Some(ax_window.minimized),
        main: Some(ax_window.main),
        layer: server.map(|s| s.layer),
        source: match server {
            Some(_) => WindowSource::Both,
            None => WindowSource::AccessibilityOnly,
        },
    }
}

/// Builds a record for a window only the window server reported.
fn server_only(server: &ServerWindow, running_apps: &[RunningApp]) -> WindowRecord {
    let app = app_for_pid(running_apps, server.owner_pid);
    WindowRecord {
        window_id: Some(server.window_id),
        owner_pid: server.owner_pid,
        app_name: app
            .map(|a| a.name.clone())
            .or_else(|| server.owner_name.clone()),
        bundle_id: app.and_then(|a| a.bundle_id.clone()),
        title: title_key(&server.title).map(str::to_string),
        frame: server.frame,
        on_current_space: Some(server.on_screen),
        minimized: None,
        main: None,
        layer: Some(server.layer),
        source: WindowSource::WindowServerOnly,
    }
}

// ---- list_windows -------------------------------------------------------

/// Lists the windows of one app, or of every app.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListWindowsRequest {
    /// Limit the list to one app. `None` lists every app's windows.
    #[serde(default)]
    pub app: Option<AppIdentifier>,
    /// Keep only the windows on the Space the user is looking at now.
    /// Defaults to `false`.
    #[serde(default)]
    pub on_current_space_only: Option<bool>,
    /// Keep the menu bar, the Dock, and other system furniture, which
    /// sit on a window layer other than [`NORMAL_WINDOW_LAYER`].
    /// Defaults to `false`.
    #[serde(default)]
    pub include_system_windows: Option<bool>,
}

/// The windows [`perform_list_windows`] found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListWindowsResponse {
    /// How many windows the response carries.
    pub count: usize,
    pub windows: Vec<WindowRecord>,
}

/// Reads both window lists, joins them (PINV-30), and applies the
/// request's filters.
pub fn perform_list_windows<L>(
    lister: &L,
    request: &ListWindowsRequest,
) -> Result<ListWindowsResponse, PolarizeError>
where
    L: WindowLister,
{
    // Resolving the pid first is what lets the window-server half be
    // filtered to one app. That list is global, and carries a process id
    // rather than a bundle id or a name.
    let pid_filter = match request.app.as_ref() {
        Some(app) => Some(lister.resolve_app_pid(app)?),
        None => None,
    };
    let ax_windows = lister.accessibility_windows(request.app.as_ref())?;
    let server_windows = lister.window_server_windows()?;
    let running_apps = lister.running_apps()?;

    let mut windows = merge_window_lists(&ax_windows, &server_windows, &running_apps);
    if let Some(pid) = pid_filter {
        windows.retain(|window| window.owner_pid == pid);
    }
    if !request.include_system_windows.unwrap_or(false) {
        // An accessibility-only window has no layer at all. It is a real
        // app window either way, so it stays.
        windows.retain(|window| {
            window
                .layer
                .is_none_or(|layer| layer == NORMAL_WINDOW_LAYER)
        });
    }
    if request.on_current_space_only.unwrap_or(false) {
        windows.retain(|window| window.on_current_space == Some(true));
    }
    Ok(ListWindowsResponse {
        count: windows.len(),
        windows,
    })
}

// ---- app_launch ---------------------------------------------------------

/// Starts an app, or brings one that already runs to the front.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct AppLaunchRequest {
    /// The app to open. `polarize-macos` tries `bundle_id` first, then
    /// `app_name` — see PINV-5.
    pub app: AppIdentifier,
    /// Bring the app to the front once it runs. Defaults to `true`.
    #[serde(default)]
    pub activate: Option<bool>,
    /// Give up after this many milliseconds if the app never appears.
    /// Defaults to [`DEFAULT_LAUNCH_TIMEOUT_MS`], clamped to
    /// [`MAX_LIFECYCLE_TIMEOUT_MS`].
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// The app [`perform_app_launch`] opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppLaunchResponse {
    pub pid: i32,
    pub app_name: String,
    pub bundle_id: Option<String>,
    /// `true` when this call started the app.
    pub launched: bool,
    /// `true` when the app was already running, so this call started
    /// nothing.
    pub already_running: bool,
    /// `true` when this call brought the app to the front.
    pub activated: bool,
}

/// Opens the app a request names, and reports the running app.
///
/// An app that already runs is never opened a second time. Opening it
/// again can create a second instance of some apps, and a caller that
/// asks for "launch" almost always means "make sure it runs".
pub fn perform_app_launch<L, C>(
    lifecycle: &L,
    clock: &C,
    request: &AppLaunchRequest,
) -> Result<AppLaunchResponse, PolarizeError>
where
    L: AppLifecycle,
    C: Clock,
{
    let already = lifecycle.find_running_app(&request.app)?;
    let (app, already_running) = match already {
        Some(app) => (app, true),
        None => (wait_for_launch(lifecycle, clock, request)?, false),
    };

    let activated = if request.activate.unwrap_or(true) {
        lifecycle.activate_app_by_pid(app.pid)?
    } else {
        false
    };

    Ok(AppLaunchResponse {
        pid: app.pid,
        app_name: app.name,
        bundle_id: app.bundle_id,
        launched: !already_running,
        already_running,
        activated,
    })
}

/// Opens the app, then polls until macOS reports it running.
///
/// The poll checks at least once, whatever the timeout, so a
/// `timeout_ms` of `0` still reads the running-app list one time.
fn wait_for_launch<L, C>(
    lifecycle: &L,
    clock: &C,
    request: &AppLaunchRequest,
) -> Result<RunningApp, PolarizeError>
where
    L: AppLifecycle,
    C: Clock,
{
    let timeout_ms = request
        .timeout_ms
        .unwrap_or(DEFAULT_LAUNCH_TIMEOUT_MS)
        .min(MAX_LIFECYCLE_TIMEOUT_MS);
    let mut found = lifecycle.open_app(&request.app)?;
    let start_ms = clock.now_ms();
    while found.is_none() {
        found = lifecycle.find_running_app(&request.app)?;
        if found.is_some() {
            break;
        }
        let elapsed_ms = clock.now_ms().saturating_sub(start_ms);
        if elapsed_ms >= timeout_ms {
            break;
        }
        let budget_ms = (timeout_ms - elapsed_ms).min(LIFECYCLE_POLL_INTERVAL_MS);
        lifecycle.sleep_until_exit(None, Duration::from_millis(budget_ms))?;
    }
    found.ok_or_else(|| PolarizeError::AppNotFound(describe_identifier(&request.app)))
}

// ---- app_quit -----------------------------------------------------------

/// Quits an app.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct AppQuitRequest {
    /// The app to quit. `polarize-macos` tries `bundle_id` first, then
    /// `app_name` — see PINV-5.
    pub app: AppIdentifier,
    /// Kill the app instead of asking it to quit. Defaults to `false`.
    /// See PINV-31.
    #[serde(default)]
    pub force: Option<bool>,
    /// Wait this many milliseconds for the app to exit. Defaults to
    /// [`DEFAULT_QUIT_TIMEOUT_MS`], clamped to
    /// [`MAX_LIFECYCLE_TIMEOUT_MS`].
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// What [`perform_app_quit`] asked for, and what happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppQuitResponse {
    pub pid: i32,
    pub app_name: String,
    pub bundle_id: Option<String>,
    /// `true` when the app had exited before this call returned.
    /// `false` means the app is still running — a modal save dialog is
    /// the usual reason.
    pub exited: bool,
    /// `true` when this call used `forceTerminate()`.
    pub forced: bool,
    /// Whether the platform accepted the termination request. This is
    /// not the same as the app having exited.
    pub requested: bool,
    /// How long the call waited for the exit, in milliseconds.
    pub waited_ms: u64,
    /// How many times the call checked whether the app had exited.
    pub polls: u32,
}

/// # PINV-31: `app_quit` asks politely unless the caller asks to force
///
/// - Always: [`perform_app_quit`] calls
///   [`AppLifecycle::request_terminate`] with `force: false` unless the
///   request sets `force: true`. An absent `force` field is `false`. The
///   call never escalates to a force on its own, not after a timeout and
///   not after a refused request. It reports `exited` from what the
///   platform observed, never from the fact that it asked.
/// - Because: `terminate()` sends a quit Apple Event, so the app runs
///   its own quit path: it can save open documents, and it can put up a
///   "save changes?" dialog and stay running. `forceTerminate()` is
///   `SIGKILL` with extra steps — unsaved work is gone, with no dialog
///   and no undo. An automation tool that escalates by itself destroys a
///   user's work on a schedule the user never agreed to. Reporting
///   "quit" when the app is still showing a save dialog is just as bad:
///   the caller moves on and the app is still there.
/// - If violated: an `app_quit` call silently discards unsaved documents,
///   or a caller believes an app exited while a modal dialog holds it
///   open, and every step that follows acts on the wrong app state.
pub fn perform_app_quit<L, C>(
    lifecycle: &L,
    clock: &C,
    request: &AppQuitRequest,
) -> Result<AppQuitResponse, PolarizeError>
where
    L: AppLifecycle,
    C: Clock,
{
    let Some(app) = lifecycle.find_running_app(&request.app)? else {
        return Err(PolarizeError::AppNotFound(describe_identifier(
            &request.app,
        )));
    };
    let forced = request.force.unwrap_or(false);
    let timeout_ms = request
        .timeout_ms
        .unwrap_or(DEFAULT_QUIT_TIMEOUT_MS)
        .min(MAX_LIFECYCLE_TIMEOUT_MS);

    let requested = lifecycle.request_terminate(app.pid, forced)?;

    let start_ms = clock.now_ms();
    let mut polls = 0u32;
    let mut exited = false;
    loop {
        let elapsed_ms = clock.now_ms().saturating_sub(start_ms);
        let budget_ms = timeout_ms
            .saturating_sub(elapsed_ms)
            .min(LIFECYCLE_POLL_INTERVAL_MS);
        polls += 1;
        if lifecycle.sleep_until_exit(Some(app.pid), Duration::from_millis(budget_ms))? {
            exited = true;
            break;
        }
        if clock.now_ms().saturating_sub(start_ms) >= timeout_ms {
            break;
        }
    }

    Ok(AppQuitResponse {
        pid: app.pid,
        app_name: app.name,
        bundle_id: app.bundle_id,
        exited,
        forced,
        requested,
        waited_ms: clock.now_ms().saturating_sub(start_ms),
        polls,
    })
}

// ---- list_displays ------------------------------------------------------

/// Lists the attached displays. The tool takes no arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListDisplaysRequest {}

/// One display, as `list_displays` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DisplayRecord {
    /// Pass this back as a `screenshot` or `tap` `display_id`.
    pub display_id: u32,
    /// The display that holds the menu bar.
    pub is_main: bool,
    /// The display's rectangle in the global pixel space — the same
    /// space a `tap` point uses.
    pub frame: PixelFrame,
    /// 2.0 on a Retina display, 1.0 on a standard one.
    pub scale_factor: f64,
    /// The backing-store width in real pixels: `frame.width` times
    /// `scale_factor`. A screenshot of this display is this wide.
    pub pixel_width: f64,
    /// The backing-store height in real pixels.
    pub pixel_height: f64,
}

/// The displays [`perform_list_displays`] found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListDisplaysResponse {
    pub count: usize,
    pub displays: Vec<DisplayRecord>,
}

/// Reads a scale factor the platform reported, and repairs a useless
/// one.
///
/// A non-positive or non-finite scale would turn `pixel_width` into
/// garbage, and `1.0` is the honest fallback: it reports the frame
/// unchanged rather than a made-up multiple of it.
pub fn normalize_scale_factor(raw: f64) -> f64 {
    if raw.is_finite() && raw > 0.0 {
        raw
    } else {
        1.0
    }
}

/// Lists every active display, main display first.
///
/// The main display leads because it defines the global coordinate
/// origin. Every other display keeps the order the platform gave.
pub fn perform_list_displays<D>(
    lister: &D,
    _request: &ListDisplaysRequest,
) -> Result<ListDisplaysResponse, PolarizeError>
where
    D: DisplayLister,
{
    let displays = lister.displays()?;
    let ordered = displays
        .iter()
        .filter(|display| display.is_main)
        .chain(displays.iter().filter(|display| !display.is_main));
    let displays: Vec<DisplayRecord> = ordered
        .map(|display| {
            let scale_factor = normalize_scale_factor(display.scale_factor);
            DisplayRecord {
                display_id: display.display_id,
                is_main: display.is_main,
                frame: display.frame,
                scale_factor,
                pixel_width: display.frame.width * scale_factor,
                pixel_height: display.frame.height * scale_factor,
            }
        })
        .collect();
    Ok(ListDisplaysResponse {
        count: displays.len(),
        displays,
    })
}

// ---- shared helpers -----------------------------------------------------

/// Names an app identifier for an error message: its bundle id, then its
/// name, then a placeholder.
pub fn describe_identifier(app: &AppIdentifier) -> String {
    app.bundle_id
        .as_deref()
        .or(app.app_name.as_deref())
        .unwrap_or("<empty AppIdentifier>")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    // ---- fakes ----------------------------------------------------------

    /// A clock the test moves by hand, so no test ever sleeps.
    ///
    /// [`FakeLifecycle`] holds a clone of the same counter, and advances
    /// it on every wait. That is what makes a deadline test real: the
    /// time a wait consumes is the time the code under test then sees.
    #[derive(Debug, Default, Clone)]
    struct FakeClock {
        now_ms: Rc<Cell<u64>>,
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.now_ms.get()
        }
    }

    #[derive(Debug, Default)]
    struct FakeWindowLister {
        ax_windows: Vec<AxWindow>,
        server_windows: Vec<ServerWindow>,
        running_apps: Vec<RunningApp>,
        pid: i32,
        resolve_calls: RefCell<Vec<AppIdentifier>>,
        ax_calls: RefCell<Vec<Option<AppIdentifier>>>,
    }

    impl WindowLister for FakeWindowLister {
        fn resolve_app_pid(&self, app: &AppIdentifier) -> Result<i32, PolarizeError> {
            self.resolve_calls.borrow_mut().push(app.clone());
            Ok(self.pid)
        }

        fn accessibility_windows(
            &self,
            app: Option<&AppIdentifier>,
        ) -> Result<Vec<AxWindow>, PolarizeError> {
            self.ax_calls.borrow_mut().push(app.cloned());
            Ok(self.ax_windows.clone())
        }

        fn window_server_windows(&self) -> Result<Vec<ServerWindow>, PolarizeError> {
            Ok(self.server_windows.clone())
        }

        fn running_apps(&self) -> Result<Vec<RunningApp>, PolarizeError> {
            Ok(self.running_apps.clone())
        }
    }

    /// Records every lifecycle call, and answers from a script.
    #[derive(Debug, Default)]
    struct FakeLifecycle {
        /// Answers to successive `find_running_app` calls. The last one
        /// repeats once the script runs out.
        find_results: RefCell<Vec<Option<RunningApp>>>,
        open_result: Option<RunningApp>,
        /// Answers to successive `sleep_until_exit` calls. `false`
        /// repeats once the script runs out.
        exit_results: RefCell<Vec<bool>>,
        terminate_accepts: bool,
        activate_result: bool,
        opened: RefCell<u32>,
        terminate_calls: RefCell<Vec<(i32, bool)>>,
        activate_calls: RefCell<Vec<i32>>,
        budgets_ms: RefCell<Vec<u64>>,
        /// A clone of the test's [`FakeClock`], when the test wants a
        /// wait to move time.
        clock: Option<FakeClock>,
        /// Milliseconds the fake clock advances per `sleep_until_exit`.
        tick_ms: u64,
    }

    impl FakeLifecycle {
        fn running(app: RunningApp) -> Self {
            Self {
                find_results: RefCell::new(vec![Some(app)]),
                terminate_accepts: true,
                activate_result: true,
                ..Self::default()
            }
        }

        fn absent() -> Self {
            Self {
                find_results: RefCell::new(vec![None]),
                terminate_accepts: true,
                activate_result: true,
                ..Self::default()
            }
        }

        fn next_find(&self) -> Option<RunningApp> {
            let mut results = self.find_results.borrow_mut();
            if results.len() > 1 {
                results.remove(0)
            } else {
                results.first().cloned().flatten()
            }
        }
    }

    impl AppLifecycle for FakeLifecycle {
        fn find_running_app(
            &self,
            _app: &AppIdentifier,
        ) -> Result<Option<RunningApp>, PolarizeError> {
            Ok(self.next_find())
        }

        fn open_app(&self, _app: &AppIdentifier) -> Result<Option<RunningApp>, PolarizeError> {
            *self.opened.borrow_mut() += 1;
            Ok(self.open_result.clone())
        }

        fn activate_app_by_pid(&self, pid: i32) -> Result<bool, PolarizeError> {
            self.activate_calls.borrow_mut().push(pid);
            Ok(self.activate_result)
        }

        fn request_terminate(&self, pid: i32, force: bool) -> Result<bool, PolarizeError> {
            self.terminate_calls.borrow_mut().push((pid, force));
            Ok(self.terminate_accepts)
        }

        fn sleep_until_exit(
            &self,
            _pid: Option<i32>,
            budget: Duration,
        ) -> Result<bool, PolarizeError> {
            self.budgets_ms.borrow_mut().push(budget.as_millis() as u64);
            if let Some(clock) = self.clock.as_ref() {
                clock.now_ms.set(clock.now_ms.get() + self.tick_ms);
            }
            let mut results = self.exit_results.borrow_mut();
            if results.is_empty() {
                Ok(false)
            } else {
                Ok(results.remove(0))
            }
        }
    }

    #[derive(Debug, Default)]
    struct FakeDisplayLister {
        displays: Vec<DisplayInfo>,
    }

    impl DisplayLister for FakeDisplayLister {
        fn displays(&self) -> Result<Vec<DisplayInfo>, PolarizeError> {
            Ok(self.displays.clone())
        }
    }

    // ---- builders -------------------------------------------------------

    fn ax(pid: i32, title: Option<&str>, frame: PixelFrame) -> AxWindow {
        AxWindow {
            owner_pid: pid,
            title: title.map(str::to_string),
            frame,
            main: false,
            minimized: false,
        }
    }

    fn server(id: u32, pid: i32, title: Option<&str>, frame: PixelFrame) -> ServerWindow {
        ServerWindow {
            window_id: id,
            owner_pid: pid,
            owner_name: Some("Owner".to_string()),
            title: title.map(str::to_string),
            frame,
            on_screen: true,
            layer: NORMAL_WINDOW_LAYER,
        }
    }

    fn frame(x: f64, y: f64) -> PixelFrame {
        PixelFrame::new(x, y, 400.0, 300.0)
    }

    fn text_edit() -> RunningApp {
        RunningApp {
            pid: 501,
            name: "TextEdit".to_string(),
            bundle_id: Some("com.apple.TextEdit".to_string()),
        }
    }

    fn by_name(name: &str) -> AppIdentifier {
        AppIdentifier {
            bundle_id: None,
            app_name: Some(name.to_string()),
        }
    }

    // ---- merge_window_lists (PINV-30) -----------------------------------

    #[test]
    fn merge_pairs_a_window_both_lists_report() {
        let ax_windows = [ax(501, Some("Notes"), frame(10.0, 20.0))];
        let server_windows = [server(77, 501, Some("Notes"), frame(10.0, 20.0))];
        let records = merge_window_lists(&ax_windows, &server_windows, &[text_edit()]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, WindowSource::Both);
        assert_eq!(records[0].window_id, Some(77));
        assert_eq!(records[0].title.as_deref(), Some("Notes"));
    }

    #[test]
    fn merge_keeps_an_accessibility_only_window_without_a_window_id() {
        let ax_windows = [ax(501, Some("Notes"), frame(10.0, 20.0))];
        let records = merge_window_lists(&ax_windows, &[], &[text_edit()]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, WindowSource::AccessibilityOnly);
        assert_eq!(records[0].window_id, None);
        assert_eq!(records[0].layer, None);
    }

    #[test]
    fn merge_reports_no_space_fact_for_an_accessibility_only_window() {
        let ax_windows = [ax(501, Some("Notes"), frame(10.0, 20.0))];
        let records = merge_window_lists(&ax_windows, &[], &[]);
        assert_eq!(records[0].on_current_space, None);
    }

    #[test]
    fn merge_keeps_a_window_server_only_window() {
        let server_windows = [server(77, 501, Some("Notes"), frame(10.0, 20.0))];
        let records = merge_window_lists(&[], &server_windows, &[text_edit()]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, WindowSource::WindowServerOnly);
        assert_eq!(records[0].window_id, Some(77));
        assert_eq!(records[0].main, None);
        assert_eq!(records[0].minimized, None);
    }

    #[test]
    fn merge_of_two_empty_lists_is_empty() {
        assert!(merge_window_lists(&[], &[], &[]).is_empty());
    }

    #[test]
    fn merge_never_pairs_windows_of_different_apps() {
        let ax_windows = [ax(501, Some("Notes"), frame(10.0, 20.0))];
        let server_windows = [server(77, 999, Some("Notes"), frame(10.0, 20.0))];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].source, WindowSource::AccessibilityOnly);
        assert_eq!(records[1].source, WindowSource::WindowServerOnly);
    }

    #[test]
    fn merge_pairs_same_title_windows_by_frame_first() {
        // Two windows share a title. The lists disagree on order, so a
        // pure list-order join would cross them over.
        let ax_windows = [
            ax(501, Some("Untitled"), frame(0.0, 0.0)),
            ax(501, Some("Untitled"), frame(500.0, 0.0)),
        ];
        let server_windows = [
            server(2, 501, Some("Untitled"), frame(500.0, 0.0)),
            server(1, 501, Some("Untitled"), frame(0.0, 0.0)),
        ];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].window_id, Some(1));
        assert_eq!(records[1].window_id, Some(2));
    }

    #[test]
    fn merge_falls_back_to_the_title_when_the_frames_differ() {
        let ax_windows = [ax(501, Some("Notes"), frame(10.0, 20.0))];
        let server_windows = [server(77, 501, Some("Notes"), frame(900.0, 900.0))];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].window_id, Some(77));
    }

    #[test]
    fn merge_falls_back_to_the_frame_when_the_titles_differ() {
        // macOS hides `kCGWindowName` without Screen Recording
        // permission, so this is the common case, not an edge case.
        let ax_windows = [ax(501, Some("Notes"), frame(10.0, 20.0))];
        let server_windows = [server(77, 501, None, frame(10.0, 20.0))];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].window_id, Some(77));
        assert_eq!(records[0].title.as_deref(), Some("Notes"));
    }

    #[test]
    fn merge_claims_each_server_window_at_most_once() {
        let ax_windows = [
            ax(501, None, frame(0.0, 0.0)),
            ax(501, None, frame(0.0, 0.0)),
            ax(501, None, frame(0.0, 0.0)),
        ];
        let server_windows = [
            server(1, 501, None, frame(0.0, 0.0)),
            server(2, 501, None, frame(0.0, 0.0)),
        ];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].window_id, Some(1));
        assert_eq!(records[1].window_id, Some(2));
        assert_eq!(records[2].window_id, None);
        assert_eq!(records[2].source, WindowSource::AccessibilityOnly);
    }

    #[test]
    fn merge_tolerates_a_sub_pixel_frame_difference() {
        let ax_windows = [ax(501, None, PixelFrame::new(10.0, 20.0, 400.0, 300.0))];
        let server_windows = [server(
            77,
            501,
            None,
            PixelFrame::new(10.5, 20.5, 400.5, 299.5),
        )];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].window_id, Some(77));
    }

    #[test]
    fn merge_does_not_pair_frames_further_apart_than_the_tolerance() {
        let ax_windows = [ax(
            501,
            Some("A"),
            PixelFrame::new(10.0, 20.0, 400.0, 300.0),
        )];
        let server_windows = [server(
            77,
            501,
            Some("B"),
            PixelFrame::new(13.0, 20.0, 400.0, 300.0),
        )];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn merge_reads_the_app_name_and_bundle_id_from_the_running_app_list() {
        let ax_windows = [ax(501, Some("Notes"), frame(0.0, 0.0))];
        let records = merge_window_lists(&ax_windows, &[], &[text_edit()]);
        assert_eq!(records[0].app_name.as_deref(), Some("TextEdit"));
        assert_eq!(records[0].bundle_id.as_deref(), Some("com.apple.TextEdit"));
    }

    #[test]
    fn merge_falls_back_to_the_owner_name_when_no_running_app_matches() {
        let server_windows = [server(77, 501, Some("Notes"), frame(0.0, 0.0))];
        let records = merge_window_lists(&[], &server_windows, &[]);
        assert_eq!(records[0].app_name.as_deref(), Some("Owner"));
        assert_eq!(records[0].bundle_id, None);
    }

    #[test]
    fn merge_prefers_the_accessibility_title_over_an_empty_server_title() {
        let ax_windows = [ax(501, Some("Notes"), frame(0.0, 0.0))];
        let server_windows = [server(77, 501, Some(""), frame(0.0, 0.0))];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        assert_eq!(records[0].title.as_deref(), Some("Notes"));
    }

    #[test]
    fn merge_treats_an_empty_title_as_no_title() {
        let ax_windows = [ax(501, Some(""), frame(0.0, 0.0))];
        let server_windows = [server(77, 501, None, frame(900.0, 900.0))];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        assert_eq!(records.len(), 1, "an empty title must match an absent one");
        assert_eq!(records[0].title, None);
    }

    #[test]
    fn merge_reports_the_space_flag_from_the_window_server() {
        let ax_windows = [ax(501, Some("Notes"), frame(0.0, 0.0))];
        let mut hidden = server(77, 501, Some("Notes"), frame(0.0, 0.0));
        hidden.on_screen = false;
        let records = merge_window_lists(&ax_windows, &[hidden], &[]);
        assert_eq!(records[0].on_current_space, Some(false));
    }

    #[test]
    fn merge_keeps_the_accessibility_order_and_appends_the_rest() {
        let ax_windows = [
            ax(501, Some("First"), frame(0.0, 0.0)),
            ax(501, Some("Second"), frame(100.0, 0.0)),
        ];
        let server_windows = [
            server(3, 501, Some("Third"), frame(700.0, 0.0)),
            server(2, 501, Some("Second"), frame(100.0, 0.0)),
        ];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        let titles: Vec<Option<&str>> = records.iter().map(|r| r.title.as_deref()).collect();
        assert_eq!(
            titles,
            vec![Some("First"), Some("Second"), Some("Third")],
            "accessibility windows keep their order, then the leftovers"
        );
    }

    #[test]
    fn merge_uses_the_window_server_frame_when_both_lists_report_one() {
        let ax_windows = [ax(
            501,
            Some("Notes"),
            PixelFrame::new(10.0, 20.0, 400.0, 300.0),
        )];
        let server_windows = [server(
            77,
            501,
            Some("Notes"),
            PixelFrame::new(10.4, 20.4, 400.0, 300.0),
        )];
        let records = merge_window_lists(&ax_windows, &server_windows, &[]);
        assert_eq!(records[0].frame.x, 10.4);
    }

    // ---- perform_list_windows -------------------------------------------

    fn lister_with_two_apps() -> FakeWindowLister {
        FakeWindowLister {
            ax_windows: vec![
                ax(501, Some("Notes"), frame(0.0, 0.0)),
                ax(777, Some("Other"), frame(100.0, 0.0)),
            ],
            server_windows: vec![
                server(1, 501, Some("Notes"), frame(0.0, 0.0)),
                server(2, 777, Some("Other"), frame(100.0, 0.0)),
            ],
            running_apps: vec![text_edit()],
            pid: 501,
            ..FakeWindowLister::default()
        }
    }

    #[test]
    fn list_windows_filters_to_the_resolved_app_pid() {
        let lister = lister_with_two_apps();
        let request = ListWindowsRequest {
            app: Some(by_name("TextEdit")),
            ..ListWindowsRequest::default()
        };
        let response = perform_list_windows(&lister, &request).unwrap();
        assert_eq!(response.count, 1);
        assert_eq!(response.windows[0].owner_pid, 501);
        assert_eq!(lister.resolve_calls.borrow().len(), 1);
    }

    #[test]
    fn list_windows_without_an_app_never_resolves_a_pid() {
        let lister = lister_with_two_apps();
        let response = perform_list_windows(&lister, &ListWindowsRequest::default()).unwrap();
        assert_eq!(response.count, 2);
        assert!(lister.resolve_calls.borrow().is_empty());
        assert_eq!(lister.ax_calls.borrow().as_slice(), &[None]);
    }

    #[test]
    fn list_windows_drops_system_layer_windows_by_default() {
        let mut menu_bar = server(9, 200, Some("Menubar"), frame(0.0, 0.0));
        menu_bar.layer = 25;
        let lister = FakeWindowLister {
            server_windows: vec![menu_bar, server(1, 501, Some("Notes"), frame(0.0, 0.0))],
            ..FakeWindowLister::default()
        };
        let response = perform_list_windows(&lister, &ListWindowsRequest::default()).unwrap();
        assert_eq!(response.count, 1);
        assert_eq!(response.windows[0].window_id, Some(1));
    }

    #[test]
    fn list_windows_keeps_system_windows_when_asked() {
        let mut menu_bar = server(9, 200, Some("Menubar"), frame(0.0, 0.0));
        menu_bar.layer = 25;
        let lister = FakeWindowLister {
            server_windows: vec![menu_bar],
            ..FakeWindowLister::default()
        };
        let request = ListWindowsRequest {
            include_system_windows: Some(true),
            ..ListWindowsRequest::default()
        };
        let response = perform_list_windows(&lister, &request).unwrap();
        assert_eq!(response.count, 1);
    }

    #[test]
    fn list_windows_keeps_an_accessibility_only_window_which_has_no_layer() {
        let lister = FakeWindowLister {
            ax_windows: vec![ax(501, Some("Notes"), frame(0.0, 0.0))],
            ..FakeWindowLister::default()
        };
        let response = perform_list_windows(&lister, &ListWindowsRequest::default()).unwrap();
        assert_eq!(response.count, 1);
    }

    #[test]
    fn list_windows_on_current_space_only_drops_the_other_spaces() {
        let mut elsewhere = server(2, 501, Some("Elsewhere"), frame(300.0, 0.0));
        elsewhere.on_screen = false;
        let lister = FakeWindowLister {
            server_windows: vec![server(1, 501, Some("Here"), frame(0.0, 0.0)), elsewhere],
            ..FakeWindowLister::default()
        };
        let request = ListWindowsRequest {
            on_current_space_only: Some(true),
            ..ListWindowsRequest::default()
        };
        let response = perform_list_windows(&lister, &request).unwrap();
        assert_eq!(response.count, 1);
        assert_eq!(response.windows[0].title.as_deref(), Some("Here"));
    }

    #[test]
    fn list_windows_on_current_space_only_drops_an_accessibility_only_window() {
        // The fact is unknown, not true, so a filter that asks for
        // "on the current Space" must not claim it.
        let lister = FakeWindowLister {
            ax_windows: vec![ax(501, Some("Notes"), frame(0.0, 0.0))],
            ..FakeWindowLister::default()
        };
        let request = ListWindowsRequest {
            on_current_space_only: Some(true),
            ..ListWindowsRequest::default()
        };
        let response = perform_list_windows(&lister, &request).unwrap();
        assert_eq!(response.count, 0);
    }

    #[test]
    fn list_windows_count_matches_the_window_list_length() {
        let lister = lister_with_two_apps();
        let response = perform_list_windows(&lister, &ListWindowsRequest::default()).unwrap();
        assert_eq!(response.count, response.windows.len());
    }

    #[test]
    fn list_windows_passes_the_app_through_to_the_accessibility_half() {
        let lister = lister_with_two_apps();
        let request = ListWindowsRequest {
            app: Some(by_name("TextEdit")),
            ..ListWindowsRequest::default()
        };
        perform_list_windows(&lister, &request).unwrap();
        assert_eq!(
            lister.ax_calls.borrow().as_slice(),
            &[Some(by_name("TextEdit"))]
        );
    }

    // ---- perform_app_launch ---------------------------------------------

    #[test]
    fn app_launch_opens_an_app_that_is_not_running() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            open_result: Some(text_edit()),
            ..FakeLifecycle::absent()
        };
        let request = AppLaunchRequest {
            app: by_name("TextEdit"),
            ..AppLaunchRequest::default()
        };
        let response = perform_app_launch(&lifecycle, &clock, &request).unwrap();
        assert!(response.launched);
        assert!(!response.already_running);
        assert_eq!(*lifecycle.opened.borrow(), 1);
        assert_eq!(response.pid, 501);
        assert_eq!(response.bundle_id.as_deref(), Some("com.apple.TextEdit"));
    }

    #[test]
    fn app_launch_never_opens_an_app_that_already_runs() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle::running(text_edit());
        let request = AppLaunchRequest {
            app: by_name("TextEdit"),
            ..AppLaunchRequest::default()
        };
        let response = perform_app_launch(&lifecycle, &clock, &request).unwrap();
        assert!(response.already_running);
        assert!(!response.launched);
        assert_eq!(*lifecycle.opened.borrow(), 0);
    }

    #[test]
    fn app_launch_activates_by_default() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle::running(text_edit());
        let request = AppLaunchRequest {
            app: by_name("TextEdit"),
            ..AppLaunchRequest::default()
        };
        let response = perform_app_launch(&lifecycle, &clock, &request).unwrap();
        assert!(response.activated);
        assert_eq!(lifecycle.activate_calls.borrow().as_slice(), &[501]);
    }

    #[test]
    fn app_launch_skips_activation_when_asked() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle::running(text_edit());
        let request = AppLaunchRequest {
            app: by_name("TextEdit"),
            activate: Some(false),
            ..AppLaunchRequest::default()
        };
        let response = perform_app_launch(&lifecycle, &clock, &request).unwrap();
        assert!(!response.activated);
        assert!(lifecycle.activate_calls.borrow().is_empty());
    }

    #[test]
    fn app_launch_waits_for_the_app_to_appear() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            // Not running, not running, then running.
            find_results: RefCell::new(vec![None, None, Some(text_edit())]),
            open_result: None,
            terminate_accepts: true,
            activate_result: true,
            clock: Some(clock.clone()),
            tick_ms: LIFECYCLE_POLL_INTERVAL_MS,
            ..FakeLifecycle::default()
        };
        let request = AppLaunchRequest {
            app: by_name("TextEdit"),
            ..AppLaunchRequest::default()
        };
        let response = perform_app_launch(&lifecycle, &clock, &request).unwrap();
        assert!(response.launched);
        assert_eq!(response.pid, 501);
        assert_eq!(lifecycle.budgets_ms.borrow().as_slice(), &[100]);
    }

    #[test]
    fn app_launch_reports_app_not_found_when_it_never_appears() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            clock: Some(clock.clone()),
            tick_ms: LIFECYCLE_POLL_INTERVAL_MS,
            ..FakeLifecycle::absent()
        };
        let request = AppLaunchRequest {
            app: by_name("Nope"),
            timeout_ms: Some(250),
            ..AppLaunchRequest::default()
        };
        let err = perform_app_launch(&lifecycle, &clock, &request).unwrap_err();
        assert!(matches!(err, PolarizeError::AppNotFound(_)));
        assert_eq!(err.to_string(), "app not found: Nope");
        assert_eq!(lifecycle.budgets_ms.borrow().as_slice(), &[100, 100, 50]);
    }

    #[test]
    fn app_launch_checks_once_with_a_zero_timeout() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle::absent();
        let request = AppLaunchRequest {
            app: by_name("Nope"),
            timeout_ms: Some(0),
            ..AppLaunchRequest::default()
        };
        let err = perform_app_launch(&lifecycle, &clock, &request).unwrap_err();
        assert!(matches!(err, PolarizeError::AppNotFound(_)));
        assert!(
            lifecycle.budgets_ms.borrow().is_empty(),
            "a zero timeout must not wait at all"
        );
        assert_eq!(*lifecycle.opened.borrow(), 1);
    }

    #[test]
    fn app_launch_clamps_a_huge_timeout() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            clock: Some(clock.clone()),
            tick_ms: MAX_LIFECYCLE_TIMEOUT_MS,
            ..FakeLifecycle::absent()
        };
        let request = AppLaunchRequest {
            app: by_name("Nope"),
            timeout_ms: Some(u64::MAX),
            ..AppLaunchRequest::default()
        };
        let err = perform_app_launch(&lifecycle, &clock, &request).unwrap_err();
        assert!(matches!(err, PolarizeError::AppNotFound(_)));
        // One wait of the poll interval, then the clamped deadline ends
        // the loop.
        assert_eq!(lifecycle.budgets_ms.borrow().len(), 1);
    }

    #[test]
    fn app_launch_names_the_bundle_id_in_a_not_found_error() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle::absent();
        let request = AppLaunchRequest {
            app: AppIdentifier {
                bundle_id: Some("com.example.Nope".to_string()),
                app_name: Some("Nope".to_string()),
            },
            timeout_ms: Some(0),
            ..AppLaunchRequest::default()
        };
        let err = perform_app_launch(&lifecycle, &clock, &request).unwrap_err();
        assert_eq!(err.to_string(), "app not found: com.example.Nope");
    }

    // ---- perform_app_quit (PINV-31) -------------------------------------

    #[test]
    fn app_quit_asks_politely_by_default() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            exit_results: RefCell::new(vec![true]),
            ..FakeLifecycle::running(text_edit())
        };
        let request = AppQuitRequest {
            app: by_name("TextEdit"),
            ..AppQuitRequest::default()
        };
        let response = perform_app_quit(&lifecycle, &clock, &request).unwrap();
        assert_eq!(
            lifecycle.terminate_calls.borrow().as_slice(),
            &[(501, false)]
        );
        assert!(!response.forced);
    }

    #[test]
    fn app_quit_forces_only_when_asked() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            exit_results: RefCell::new(vec![true]),
            ..FakeLifecycle::running(text_edit())
        };
        let request = AppQuitRequest {
            app: by_name("TextEdit"),
            force: Some(true),
            ..AppQuitRequest::default()
        };
        let response = perform_app_quit(&lifecycle, &clock, &request).unwrap();
        assert_eq!(
            lifecycle.terminate_calls.borrow().as_slice(),
            &[(501, true)]
        );
        assert!(response.forced);
    }

    #[test]
    fn app_quit_never_escalates_to_a_force_after_a_timeout() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            clock: Some(clock.clone()),
            tick_ms: LIFECYCLE_POLL_INTERVAL_MS,
            ..FakeLifecycle::running(text_edit())
        };
        let request = AppQuitRequest {
            app: by_name("TextEdit"),
            timeout_ms: Some(300),
            ..AppQuitRequest::default()
        };
        let response = perform_app_quit(&lifecycle, &clock, &request).unwrap();
        assert!(!response.exited);
        assert_eq!(
            lifecycle.terminate_calls.borrow().as_slice(),
            &[(501, false)],
            "one polite request, and no second forced one"
        );
    }

    #[test]
    fn app_quit_reports_the_app_exited() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            exit_results: RefCell::new(vec![false, true]),
            clock: Some(clock.clone()),
            tick_ms: LIFECYCLE_POLL_INTERVAL_MS,
            ..FakeLifecycle::running(text_edit())
        };
        let request = AppQuitRequest {
            app: by_name("TextEdit"),
            ..AppQuitRequest::default()
        };
        let response = perform_app_quit(&lifecycle, &clock, &request).unwrap();
        assert!(response.exited);
        assert!(response.requested);
        assert_eq!(response.polls, 2);
        assert_eq!(response.waited_ms, 200);
    }

    #[test]
    fn app_quit_reports_an_app_that_did_not_exit() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            clock: Some(clock.clone()),
            tick_ms: LIFECYCLE_POLL_INTERVAL_MS,
            ..FakeLifecycle::running(text_edit())
        };
        let request = AppQuitRequest {
            app: by_name("TextEdit"),
            timeout_ms: Some(200),
            ..AppQuitRequest::default()
        };
        let response = perform_app_quit(&lifecycle, &clock, &request).unwrap();
        assert!(!response.exited);
        assert_eq!(response.polls, 2);
    }

    #[test]
    fn app_quit_fails_when_the_app_is_not_running() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle::absent();
        let request = AppQuitRequest {
            app: by_name("Nope"),
            ..AppQuitRequest::default()
        };
        let err = perform_app_quit(&lifecycle, &clock, &request).unwrap_err();
        assert_eq!(err.to_string(), "app not found: Nope");
        assert!(lifecycle.terminate_calls.borrow().is_empty());
    }

    #[test]
    fn app_quit_never_waits_past_its_timeout() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            clock: Some(clock.clone()),
            tick_ms: LIFECYCLE_POLL_INTERVAL_MS,
            ..FakeLifecycle::running(text_edit())
        };
        let request = AppQuitRequest {
            app: by_name("TextEdit"),
            timeout_ms: Some(250),
            ..AppQuitRequest::default()
        };
        let response = perform_app_quit(&lifecycle, &clock, &request).unwrap();
        assert_eq!(lifecycle.budgets_ms.borrow().as_slice(), &[100, 100, 50]);
        assert!(response.waited_ms <= 300);
    }

    #[test]
    fn app_quit_checks_once_with_a_zero_timeout() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            exit_results: RefCell::new(vec![true]),
            ..FakeLifecycle::running(text_edit())
        };
        let request = AppQuitRequest {
            app: by_name("TextEdit"),
            timeout_ms: Some(0),
            ..AppQuitRequest::default()
        };
        let response = perform_app_quit(&lifecycle, &clock, &request).unwrap();
        assert_eq!(response.polls, 1);
        assert!(response.exited);
        assert_eq!(lifecycle.budgets_ms.borrow().as_slice(), &[0]);
    }

    #[test]
    fn app_quit_clamps_a_huge_timeout() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            clock: Some(clock.clone()),
            tick_ms: MAX_LIFECYCLE_TIMEOUT_MS,
            ..FakeLifecycle::running(text_edit())
        };
        let request = AppQuitRequest {
            app: by_name("TextEdit"),
            timeout_ms: Some(u64::MAX),
            ..AppQuitRequest::default()
        };
        let response = perform_app_quit(&lifecycle, &clock, &request).unwrap();
        assert_eq!(response.polls, 1);
        assert_eq!(response.waited_ms, MAX_LIFECYCLE_TIMEOUT_MS);
    }

    #[test]
    fn app_quit_reports_a_refused_request() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle {
            terminate_accepts: false,
            ..FakeLifecycle::running(text_edit())
        };
        let request = AppQuitRequest {
            app: by_name("TextEdit"),
            timeout_ms: Some(0),
            ..AppQuitRequest::default()
        };
        let response = perform_app_quit(&lifecycle, &clock, &request).unwrap();
        assert!(!response.requested);
        assert!(!response.exited);
    }

    // ---- perform_list_displays ------------------------------------------

    fn display(id: u32, x: f64, scale: f64, is_main: bool) -> DisplayInfo {
        DisplayInfo {
            display_id: id,
            frame: PixelFrame::new(x, 0.0, 1440.0, 900.0),
            scale_factor: scale,
            is_main,
        }
    }

    #[test]
    fn list_displays_reports_the_main_display_first() {
        let lister = FakeDisplayLister {
            displays: vec![
                display(2, 1440.0, 1.0, false),
                display(1, 0.0, 2.0, true),
                display(3, 2880.0, 1.0, false),
            ],
        };
        let response = perform_list_displays(&lister, &ListDisplaysRequest {}).unwrap();
        let ids: Vec<u32> = response.displays.iter().map(|d| d.display_id).collect();
        assert_eq!(ids, vec![1, 2, 3], "main first, then the platform order");
    }

    #[test]
    fn list_displays_multiplies_the_frame_by_the_scale_factor() {
        let lister = FakeDisplayLister {
            displays: vec![display(1, 0.0, 2.0, true)],
        };
        let response = perform_list_displays(&lister, &ListDisplaysRequest {}).unwrap();
        let record = response.displays[0];
        assert_eq!(record.frame.width, 1440.0);
        assert_eq!(record.pixel_width, 2880.0);
        assert_eq!(record.pixel_height, 1800.0);
    }

    #[test]
    fn list_displays_replaces_a_non_positive_scale_factor_with_one() {
        let lister = FakeDisplayLister {
            displays: vec![display(1, 0.0, 0.0, true)],
        };
        let response = perform_list_displays(&lister, &ListDisplaysRequest {}).unwrap();
        assert_eq!(response.displays[0].scale_factor, 1.0);
        assert_eq!(response.displays[0].pixel_width, 1440.0);
    }

    #[test]
    fn list_displays_replaces_a_non_finite_scale_factor_with_one() {
        assert_eq!(normalize_scale_factor(f64::NAN), 1.0);
        assert_eq!(normalize_scale_factor(f64::INFINITY), 1.0);
        assert_eq!(normalize_scale_factor(-2.0), 1.0);
        assert_eq!(normalize_scale_factor(2.0), 2.0);
    }

    #[test]
    fn list_displays_of_an_empty_list_is_empty() {
        let lister = FakeDisplayLister::default();
        let response = perform_list_displays(&lister, &ListDisplaysRequest {}).unwrap();
        assert_eq!(response.count, 0);
        assert!(response.displays.is_empty());
    }

    #[test]
    fn list_displays_count_matches_the_display_list_length() {
        let lister = FakeDisplayLister {
            displays: vec![display(1, 0.0, 2.0, true), display(2, 1440.0, 1.0, false)],
        };
        let response = perform_list_displays(&lister, &ListDisplaysRequest {}).unwrap();
        assert_eq!(response.count, 2);
        assert_eq!(response.count, response.displays.len());
    }

    // ---- geometry and serde ---------------------------------------------

    #[test]
    fn a_pixel_frame_round_trips_through_a_pixel_rect() {
        let frame = PixelFrame::new(10.0, 20.0, 400.0, 300.0);
        assert_eq!(PixelFrame::from(frame.to_pixel_rect()), frame);
    }

    #[test]
    fn list_windows_response_round_trips_through_json() {
        let lister = lister_with_two_apps();
        let response = perform_list_windows(&lister, &ListWindowsRequest::default()).unwrap();
        let json = serde_json::to_string(&response).unwrap();
        let back: ListWindowsResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back, response);
    }

    #[test]
    fn list_windows_request_deserializes_from_an_empty_object() {
        let request: ListWindowsRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(request, ListWindowsRequest::default());
    }

    #[test]
    fn app_quit_request_leaves_force_absent_when_the_caller_omits_it() {
        let request: AppQuitRequest =
            serde_json::from_str(r#"{"app":{"app_name":"TextEdit"}}"#).unwrap();
        assert_eq!(request.force, None);
        assert_eq!(request.timeout_ms, None);
    }

    #[test]
    fn a_window_source_serializes_in_snake_case() {
        let json = serde_json::to_string(&WindowSource::WindowServerOnly).unwrap();
        assert_eq!(json, r#""window_server_only""#);
    }

    #[test]
    fn list_displays_response_round_trips_through_json() {
        let lister = FakeDisplayLister {
            displays: vec![display(1, 0.0, 2.0, true)],
        };
        let response = perform_list_displays(&lister, &ListDisplaysRequest {}).unwrap();
        let json = serde_json::to_string(&response).unwrap();
        let back: ListDisplaysResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back, response);
    }

    #[test]
    fn app_launch_response_round_trips_through_json() {
        let clock = FakeClock::default();
        let lifecycle = FakeLifecycle::running(text_edit());
        let request = AppLaunchRequest {
            app: by_name("TextEdit"),
            ..AppLaunchRequest::default()
        };
        let response = perform_app_launch(&lifecycle, &clock, &request).unwrap();
        let json = serde_json::to_string(&response).unwrap();
        let back: AppLaunchResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back, response);
    }

    #[test]
    fn describe_identifier_falls_back_to_a_placeholder() {
        assert_eq!(
            describe_identifier(&AppIdentifier::default()),
            "<empty AppIdentifier>"
        );
    }
}
