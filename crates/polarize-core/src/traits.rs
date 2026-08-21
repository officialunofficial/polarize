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
use crate::schema::{AppIdentifier, Modifier, NamedKey, PostPath};

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
    ///
    /// `pid` is the target app's process id, when
    /// [`WindowManager::resolve_target_pid`] resolved one. An
    /// implementation may use `pid` to post through `SLEventPostToPid`
    /// instead of the global `CGEvent` stream. See PINV-47 in
    /// `docs/INVARIANTS.md`. A `Some` pid does not force that path: an
    /// implementation may still fall back, for instance when the
    /// symbol did not resolve. The returned [`PostPath`] must always
    /// name whichever path actually ran. A caller must never see a
    /// silent behavior change.
    fn click_at_pixel(
        &self,
        point: PixelPoint,
        click_count: u8,
        pid: Option<i32>,
    ) -> Result<PostPath, PolarizeError>;

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

    /// The process id of the app `target` names, if it names one. A
    /// `Screen` target names no particular app, so this returns `None`
    /// for it.
    ///
    /// [`crate::orchestrate::perform_tap`] passes this pid to
    /// [`InputSynthesizer::click_at_pixel`], as a candidate for the
    /// `SLEventPostToPid` path (PINV-47). A caller treats an `Err` here
    /// the same as `Ok(None)`. [`Self::resolve_target_rect`] already
    /// resolved this same app moments earlier, so a real failure here
    /// is rare. It must never block the click itself. Losing the pid
    /// only loses the pid-post optimization, not the click.
    fn resolve_target_pid(
        &self,
        target: &crate::schema::ScreenshotTarget,
    ) -> Result<Option<i32>, PolarizeError>;

    /// Activates `app` without raising its window or switching the
    /// current Space.
    ///
    /// Returns `false` when this path is unavailable. A caller then
    /// falls back to [`Self::activate_app`]. See PINV-48. `Err` means a
    /// real platform call failed, not that the path was merely
    /// unavailable.
    fn activate_app_without_raise(&self, app: &AppIdentifier) -> Result<bool, PolarizeError>;
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

/// Writes one accessibility attribute of one element. Implemented by
/// `polarize-macos` over `AXUIElementSetAttributeValue`.
///
/// The caller resolves `path` from a tree `AccessibilityInspector::describe`
/// returned, exactly as [`ActionPerformer`] does. See PINV-18 for why the
/// two walks must agree.
///
/// [`crate::set_value`] makes every decision before this call. It picks
/// the attribute, it checks the element, and it builds the typed value.
/// An implementation only performs the write. It must not choose a
/// different attribute, and it must not convert the value to another
/// type. See PINV-26.
pub trait ValueSetter {
    /// Writes `write` to the element at `path`. `app` is `None` to
    /// address the frontmost app. An empty `path` means the application
    /// element itself.
    ///
    /// An implementation calls `AXUIElementIsAttributeSettable` first.
    /// That call asks the live element, which is the only authority on
    /// whether this app accepts the write.
    ///
    /// `Ok(())` means the app accepted the write. It does not mean the
    /// app ran the handlers a real user edit runs. See PINV-27.
    fn set_value_at_path(
        &self,
        app: Option<&AppIdentifier>,
        path: &[usize],
        write: &crate::set_value::AttributeWrite,
    ) -> Result<(), PolarizeError>;
}

/// Reports the element that sits under one point. Implemented by
/// `polarize-macos` over `AXUIElementCopyElementAtPosition`.
///
/// The caller resolves the point first. [`crate::hit_test::perform_hit_test`]
/// converts a normalized fraction to a global display pixel point, the
/// same way [`crate::orchestrate::perform_tap`] does (PINV-32). An
/// implementation must not re-interpret `point` as anything else.
pub trait HitTester {
    /// The element on top at `point`, and the app the read addressed.
    ///
    /// `app` is `None` when the request names a whole screen. An
    /// implementation then picks the app itself, and reports which one
    /// it picked.
    ///
    /// `Ok((app, None))` means the platform reports no element at that
    /// point. That is a real answer, not a failure: a caller reads it as
    /// "a tap here reaches nothing".
    fn element_at_pixel(
        &self,
        app: Option<&AppIdentifier>,
        point: PixelPoint,
    ) -> Result<(ResolvedApp, Option<AxNode>), PolarizeError>;
}

/// Reads and writes the general pasteboard. Implemented by
/// `polarize-macos` over `NSPasteboard`.
///
/// A read returns the raw report, not a decision. macOS can refuse to
/// hand over the contents, and a refusal looks a lot like an empty
/// pasteboard. [`crate::clipboard::classify_read`] separates the two
/// (PINV-34), and it is pure, so real tests cover it.
pub trait ClipboardAccess {
    /// What the pasteboard reports for one content type.
    fn read_clipboard(
        &self,
        content_type: crate::clipboard::ClipboardContentType,
    ) -> Result<crate::clipboard::RawClipboardRead, PolarizeError>;

    /// Replaces the pasteboard contents with `text`, under one content
    /// type. macOS never refuses a write.
    fn write_clipboard(
        &self,
        content_type: crate::clipboard::ClipboardContentType,
        text: &str,
    ) -> Result<(), PolarizeError>;
}

/// One window of an app, as [`WindowController::list_windows`] read it.
///
/// Every field is a fresh read. `polarize-core` never caches one of
/// these across a write; see PINV-29.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    /// The window's `AXTitle`. `None` when it publishes none, or when
    /// the title is empty.
    pub title: Option<String>,
    /// The window's frame, in the global display coordinate space.
    pub rect: crate::coords::PixelRect,
    /// `AXMinimized`: the window is in the Dock.
    pub minimized: bool,
    /// `AXMain`: this is the app's main window.
    pub main: bool,
    /// `AXFocused`: this window takes keyboard input.
    pub focused: bool,
    /// `AXFullScreen`. `None` when the window publishes no such
    /// attribute. That attribute is undocumented, so `None` is a normal
    /// result — see [`crate::window_control`] and PINV-28.
    pub full_screen: Option<bool>,
}

/// One write against one window.
///
/// [`crate::window_control`] decides which writes a tool call needs, and
/// in which order. An implementation only carries them out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowWrite {
    /// Writes `AXPosition`, in the global display coordinate space.
    SetPosition(PixelPoint),
    /// Writes `AXSize`.
    SetSize(crate::coords::PixelSize),
    /// Writes `AXMinimized`.
    SetMinimized(bool),
    /// Writes `AXMain`.
    SetMain(bool),
    /// Writes `AXFullScreen`. An implementation must fail when the
    /// window publishes no such attribute, rather than report success.
    SetFullScreen(bool),
    /// Performs `AXRaise`.
    Raise,
    /// Presses the window's `AXCloseButton`.
    Close,
}

/// Reads and writes an app's windows. Implemented by `polarize-macos`
/// over `AXUIElement`'s `AXWindows` list.
///
/// The trait holds two calls only. Everything else a window tool does —
/// which window a request names, which writes it needs, in which order,
/// and what the response reports — is pure logic in
/// [`crate::window_control`], and is unit-tested there against a fake.
pub trait WindowController {
    /// Lists `app`'s windows, in `AXWindows` order, which macOS
    /// publishes front to back. `app` is `None` to address the frontmost
    /// app. Also returns the app the call resolved to, so a follow-up
    /// call can address that same app (PINV-18).
    fn list_windows(
        &self,
        app: Option<&AppIdentifier>,
    ) -> Result<(ResolvedApp, Vec<WindowInfo>), PolarizeError>;

    /// Applies every write in `writes` to the window at `index` in that
    /// same list, in order.
    ///
    /// An implementation resolves `index` **once**, then applies every
    /// write to that one element. It must not re-read the window list
    /// between writes. A write reorders the list: un-minimizing a window
    /// or raising it moves it to the front, so index `1` names a
    /// different window immediately afterwards. Re-resolving would send
    /// the rest of the plan to whatever moved into that slot. That is
    /// why the whole plan crosses the boundary in one call, rather than
    /// one write at a time. See PINV-28.
    ///
    /// An index that names no window is an error, not a silent stop. The
    /// app opened or closed a window between the two calls, so writing
    /// to another window would move something the caller never named.
    fn apply_window_writes(
        &self,
        app: Option<&AppIdentifier>,
        index: usize,
        writes: &[WindowWrite],
    ) -> Result<(), PolarizeError>;
}

/// Reads the text in an already-captured image. Implemented by
/// `polarize-macos` over Vision's `VNRecognizeTextRequest`.
///
/// This trait takes pixels, not a capture request. It never asks for a
/// screenshot itself. [`crate::find_text::perform_find_text`] captures
/// through [`ScreenCapture`] first, then hands the bytes here. Two
/// reasons make that split worth the extra step. The implementation
/// needs no ScreenCaptureKit dependency, and no permission of its own.
/// And every decision around the recognizer — the confidence floor, the
/// match test, the reading order, the bottom-left to top-left flip —
/// stays in `polarize-core`, where a fake implementation of this trait
/// covers it under `cargo test`.
///
/// An implementation is slow and synchronous. It blocks for 100 ms or
/// more per call, and for about 27 seconds on the first call after an
/// OS update, while macOS compiles the recognition model. The server
/// must call it through `tokio::task::spawn_blocking`.
pub trait TextRecognizer {
    /// Reads every line of text in `image`, which carries encoded PNG
    /// bytes plus the pixel size those bytes decode to.
    ///
    /// The returned boxes stay in Vision's own normalized space: origin
    /// bottom-left, `y` growing upward. `polarize-core` flips them
    /// (PINV-37). An implementation must not flip them itself.
    fn recognize_text(
        &self,
        image: &CapturedImage,
        options: &crate::find_text::RecognizeOptions,
    ) -> Result<Vec<crate::find_text::RecognizedLine>, PolarizeError>;
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
