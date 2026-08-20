//! Trait definitions `polarize-macos` implements with real macOS
//! framework calls.
//!
//! These traits carry no logic themselves — implementing them means
//! calling `ScreenCaptureKit`, `AXUIElement`, `CGEvent`, and AppKit APIs
//! that cannot run or be verified without a real macOS session with
//! Screen Recording and Accessibility permission granted (see the
//! "Testing harness" section of `docs/INVARIANTS.md`). What *is* tested
//! here, against fake implementations, is the orchestration logic in
//! [`crate::orchestrate`] that sits in front of them — coordinate
//! normalization, request-to-call dispatch, and response shaping.

use crate::ax::AxNode;
use crate::coords::PixelPoint;
use crate::error::PolarizeError;
use crate::schema::{AppIdentifier, Modifier, NamedKey};

/// A captured image, still in encoded PNG form.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedImage {
    pub png_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Captures pixels from a screen or a window. Implemented by
/// `polarize-macos` over `ScreenCaptureKit`.
pub trait ScreenCapture {
    /// Captures a whole display. `display_id` selects a specific display
    /// on a multi-monitor setup; `None` means the main display.
    fn capture_screen(&self, display_id: Option<u32>) -> Result<CapturedImage, PolarizeError>;

    /// Captures one window of `app`. `window_title` selects a specific
    /// window; `None` means the app's frontmost (or only) window.
    fn capture_window(
        &self,
        app: &AppIdentifier,
        window_title: Option<&str>,
    ) -> Result<CapturedImage, PolarizeError>;
}

/// The app an [`AccessibilityInspector::describe`] call actually read.
///
/// A request may name no app at all, or name one only by display name.
/// This reports what the platform resolved that to, so a follow-up call
/// can address the same app instead of resolving "frontmost" a second
/// time. See PINV-18.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedApp {
    /// The localized display name, e.g. `"TextEdit"`. Empty when the
    /// platform published none.
    pub name: String,
    /// The bundle id, e.g. `"com.apple.TextEdit"`. `None` for a raw
    /// binary, or an app bundle that declares no identifier.
    pub bundle_id: Option<String>,
}

impl ResolvedApp {
    /// The most precise identifier that addresses this app again.
    ///
    /// A bundle id wins, because it is unique and stable. A display name
    /// is the fallback, and it is only a good one: two processes can
    /// publish the same localized name, and `polarize-macos` resolves
    /// that to whichever the platform lists first. `None` means the
    /// platform published neither, so a caller has nothing better than
    /// "the frontmost app" to go on.
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

/// Walks the accessibility tree of an app. Implemented by
/// `polarize-macos` over `AXUIElement` (objc2-accessibility).
pub trait AccessibilityInspector {
    /// Returns the app the call resolved to, and its accessibility tree
    /// root. `app` is `None` to inspect the frontmost app.
    fn describe(&self, app: Option<&AppIdentifier>)
    -> Result<(ResolvedApp, AxNode), PolarizeError>;
}

/// Posts synthetic mouse and keyboard input. Implemented by
/// `polarize-macos` over `CGEvent` (objc2-core-graphics).
pub trait InputSynthesizer {
    /// Posts a mouse click at a pixel point already resolved from a
    /// normalized fraction — see [`crate::orchestrate::perform_tap`]
    /// (PINV-4). Implementations must not re-interpret `point` as
    /// anything other than raw pixels in the global display coordinate
    /// space.
    fn click_at_pixel(&self, point: PixelPoint, click_count: u8) -> Result<(), PolarizeError>;

    /// Types a literal string as a sequence of key-down/key-up events.
    fn type_text(&self, text: &str) -> Result<(), PolarizeError>;

    /// Presses one named key, holding the given modifiers.
    fn press_key(&self, key: NamedKey, modifiers: &[Modifier]) -> Result<(), PolarizeError>;
}

/// Enumerates apps/windows and resolves their pixel geometry.
/// Implemented by `polarize-macos` over AppKit (objc2-app-kit).
pub trait WindowManager {
    /// Brings `app` to the front. [`crate::orchestrate::perform_keyboard`]
    /// calls this first when a request names a `target` app, so typed
    /// text and key presses reach that app even when it did not already
    /// have focus.
    fn activate_app(&self, app: &AppIdentifier) -> Result<(), PolarizeError>;

    /// The pixel geometry — global-space origin plus size — of the
    /// screen or window a [`crate::schema::ScreenshotTarget`] refers to.
    /// [`crate::orchestrate::perform_tap`] normalizes a tap fraction
    /// against `size`, then adds `origin` to get a pixel point in the
    /// global display coordinate space [`InputSynthesizer::click_at_pixel`]
    /// requires (PINV-4 in `docs/INVARIANTS.md`).
    fn resolve_target_rect(
        &self,
        target: &crate::schema::ScreenshotTarget,
    ) -> Result<crate::coords::PixelRect, PolarizeError>;
}

/// Performs one accessibility action on one element. Implemented by
/// `polarize-macos` over `AXUIElementPerformAction`.
///
/// The caller resolves `path` from a tree `AccessibilityInspector::describe`
/// returned. An implementation walks the same child indices down a live
/// `AXUIElement` hierarchy. See PINV-18 for why the two walks must agree,
/// and `crate::action` for the race between them.
pub trait ActionPerformer {
    /// Performs `action`, e.g. `"AXPress"`, on the element at `path`.
    /// `app` is `None` to address the frontmost app. An empty `path`
    /// means the application element itself.
    fn perform_action_at_path(
        &self,
        app: Option<&AppIdentifier>,
        path: &[usize],
        action: &str,
    ) -> Result<(), PolarizeError>;
}

/// Blocks until an app's accessibility tree reports a change.
/// Implemented by `polarize-macos` over `AXObserver` and `CFRunLoop`.
///
/// This is the one part of a wait that only macOS can do. The waiting
/// policy that calls it — the timeout, the poll interval, the match test
/// — is pure logic in [`crate::wait`], and is unit-tested there against
/// a fake implementation of this trait.
pub trait UiChangeWaiter {
    /// Blocks until `app` signals an accessibility change, or until
    /// `budget` elapses. Returns `true` when a change arrived.
    ///
    /// A `false` result is not a failure. Some accessibility trees never
    /// post a notification, so [`crate::wait`] re-reads the tree after
    /// every wait either way (PINV-19). An implementation must not
    /// return early with `false`: [`crate::wait`] treats one call as one
    /// poll interval of elapsed time.
    fn wait_for_change(
        &self,
        app: Option<&AppIdentifier>,
        budget: std::time::Duration,
    ) -> Result<bool, PolarizeError>;
}

/// The raw result of one `osascript` run.
///
/// `polarize-macos` fills this in from a real subprocess.
/// [`crate::script::perform_run_applescript`] turns it into a response
/// or a structured error (PINV-21).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScriptOutcome {
    pub stdout: String,
    pub stderr: String,
    /// `None` when a signal killed the process, as it does on timeout.
    pub exit_code: Option<i32>,
    /// `true` when the runner killed the process at its deadline.
    pub timed_out: bool,
}

/// One app's scripting dictionary, as `sdef` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppSdef {
    pub app_name: String,
    /// The raw `sdef` XML. [`crate::script::scan_sdef`] reads the verb
    /// names out of it.
    pub xml: String,
}

/// Runs AppleScript source, and reads an app's scripting dictionary.
/// Implemented by `polarize-macos` over the `osascript` and `sdef`
/// subprocesses.
pub trait AppleScriptRunner {
    /// Runs `source` and returns its output, whatever the exit status.
    /// An implementation returns `Err` only when it cannot run the
    /// script at all — a script that runs and fails comes back as an
    /// `Ok` outcome with a non-zero `exit_code`, so
    /// [`crate::script::parse_osascript_error`] can classify it.
    ///
    /// `target_app` is the app name the caller named, if any. The
    /// implementation uses it for the Automation permission preflight;
    /// `source` already carries the `tell application` wrapper (see
    /// [`crate::script::wrap_in_tell`]).
    ///
    /// The implementation must kill the process after `timeout_ms` and
    /// report `timed_out: true`.
    fn run_script(
        &self,
        source: &str,
        target_app: Option<&str>,
        timeout_ms: u64,
    ) -> Result<ScriptOutcome, PolarizeError>;

    /// Returns `app`'s scripting dictionary as raw `sdef` XML.
    fn app_sdef(&self, app: &AppIdentifier) -> Result<AppSdef, PolarizeError>;
}

// ---- workspace tools: list_windows, app_launch, app_quit, list_displays ----

/// Reads the two window lists [`crate::workspace::merge_window_lists`]
/// joins. Implemented by `polarize-macos` over `kAXWindowsAttribute` and
/// `CGWindowListCopyWindowInfo`.
///
/// Neither call decides anything. Each one fetches a plain list of plain
/// structs, and the join that turns two lists into one record set is pure
/// logic in [`crate::workspace`] (PINV-30).
pub trait WindowLister {
    /// The process id of the app `app` names. `polarize-macos` resolves
    /// it the same way every other tool does, so PINV-5's "bundle id
    /// first, then name" rule still holds here.
    fn resolve_app_pid(&self, app: &AppIdentifier) -> Result<i32, PolarizeError>;

    /// Every window the accessibility tree publishes for `app`, or for
    /// every regular app when `app` is `None`.
    fn accessibility_windows(
        &self,
        app: Option<&AppIdentifier>,
    ) -> Result<Vec<crate::workspace::AxWindow>, PolarizeError>;

    /// Every window the window server publishes, across all apps.
    fn window_server_windows(&self) -> Result<Vec<crate::workspace::ServerWindow>, PolarizeError>;

    /// Every running app, reduced to the fields a window record names.
    fn running_apps(&self) -> Result<Vec<crate::workspace::RunningApp>, PolarizeError>;
}

/// Starts and stops apps. Implemented by `polarize-macos` over
/// `NSWorkspace` and `NSRunningApplication`.
///
/// The policy that calls these — wait for a launch to appear, ask
/// politely before forcing, give up at a deadline — is pure logic in
/// [`crate::workspace`] (PINV-31).
pub trait AppLifecycle {
    /// The running app `app` names, or `None` when it is not running.
    fn find_running_app(
        &self,
        app: &AppIdentifier,
    ) -> Result<Option<crate::workspace::RunningApp>, PolarizeError>;

    /// Asks macOS to open the app `app` names.
    ///
    /// Returns the running app when the platform hands one back at once.
    /// Returns `None` when the launch is still in flight;
    /// [`crate::workspace::perform_app_launch`] then polls
    /// [`Self::find_running_app`] until the app appears.
    fn open_app(
        &self,
        app: &AppIdentifier,
    ) -> Result<Option<crate::workspace::RunningApp>, PolarizeError>;

    /// Brings the app with this process id to the front. Returns what
    /// the platform reported.
    fn activate_app_by_pid(&self, pid: i32) -> Result<bool, PolarizeError>;

    /// Asks the app with this process id to quit. `force` selects
    /// `forceTerminate()` over `terminate()`. Returns whether the
    /// platform accepted the request, which is not the same as the app
    /// having exited. See PINV-31.
    fn request_terminate(&self, pid: i32, force: bool) -> Result<bool, PolarizeError>;

    /// Blocks for at most `budget`, and reports whether the process has
    /// exited.
    ///
    /// With `pid` set, an implementation reports `true` once that
    /// process is gone. With `pid` as `None` it sleeps out the budget
    /// and reports `false`. This is the one time-consuming primitive
    /// `app_launch` and `app_quit` need. The policy that calls it lives
    /// in [`crate::workspace`], where a fake implementation covers it.
    fn sleep_until_exit(
        &self,
        pid: Option<i32>,
        budget: std::time::Duration,
    ) -> Result<bool, PolarizeError>;
}

/// Lists the attached displays. Implemented by `polarize-macos` over
/// `CGGetActiveDisplayList` and `CGDisplayBounds`.
pub trait DisplayLister {
    /// Every active display, with its bounds in the same global pixel
    /// space [`WindowManager::resolve_target_rect`] returns.
    fn displays(&self) -> Result<Vec<crate::workspace::DisplayInfo>, PolarizeError>;
}
