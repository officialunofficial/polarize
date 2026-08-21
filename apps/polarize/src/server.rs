//! The `polarize` MCP server: wires all nine MCP tools (`screenshot`,
//! `describe`, `tap`, `keyboard`, `perform_action`, `await_ui_element`,
//! `await_screen_idle`, `run_applescript`, `script_dictionary`) to
//! `polarize-core`'s orchestration functions, backed by
//! `polarize-macos`'s real macOS framework bindings.
//!
//! ## Why some tools are `async` and the rest are not
//!
//! `rmcp` runs each tool call on the `tokio` runtime. A blocking call
//! there parks a worker thread, and enough concurrent ones starve the
//! stdio transport itself. So every tool that can run long goes through
//! [`blocking`], which moves the work to `tokio`'s blocking pool:
//!
//! - `await_ui_element` and `await_screen_idle` block for as long as
//!   their timeout allows — five seconds by default, and more on
//!   request.
//! - `run_applescript` runs a subprocess, for up to two minutes.
//! - `script_dictionary` runs `sdef`, which reads an app bundle.
//! - `clipboard_read` can raise a macOS consent prompt, which blocks
//!   until the user answers it.
//! - `record_flow` runs a `CFRunLoop` for the whole recording window,
//!   which is up to a minute.
//! - `perform_action` walks a whole accessibility tree, then makes a
//!   synchronous `AXUIElementPerformAction` call that some apps answer
//!   slowly.
//! - `describe` walks that same tree. It reads eleven attributes per
//!   node, each a separate `AXUIElementCopyAttributeValue` round-trip to
//!   the target app, so a large app costs seconds.
//!
//! `screenshot`, `tap`, and `keyboard` stay synchronous. Each returns in
//! milliseconds.
//!
//! The `polarize-macos` types are all zero-sized unit structs, so a
//! `blocking` closure constructs fresh ones rather than borrowing `self`
//! across the await point.
//!
//! This module carries no logic of its own beyond argument/result
//! plumbing: every tool method deserializes its MCP call into a
//! `polarize-core` request type (via `rmcp`'s `Parameters<T>`
//! extractor), calls the matching `polarize_core::orchestrate::perform_*`
//! function, and serializes the result back out (via `rmcp`'s `Json<T>`
//! structured-output wrapper) or maps a [`PolarizeError`] to an
//! MCP [`ErrorData`]. The real work — coordinate normalization, request
//! dispatch, and the native `ScreenCaptureKit`/`AXUIElement`/`CGEvent`/
//! AppKit calls — lives in `polarize-core` and `polarize-macos`.
//!
//! ## Permission errors surface, but are not pre-flighted here
//!
//! Every `polarize-macos` implementation checks its real TCC
//! permission before making a native call (`AXIsProcessTrusted` for
//! `describe`/`tap`/`keyboard`/`perform_action`/the `await` tools,
//! `CGPreflightScreenCaptureAccess` for `screenshot`, and
//! `AEDeterminePermissionToAutomateTarget` for the AppleScript tools). Each returns `PolarizeError::Permission` when its
//! permission is not granted. That error flows through `to_error_data`
//! below like any other.
//!
//! This server does not add its own permission pre-check on top.
//! `polarize-core`'s `permission` module only ever *decides* whether a
//! status list satisfies a tool's requirement (PINV-2). Native TCC
//! queries belong in `polarize-macos`, not in this thin server.

use polarize_core::action::{self, PerformActionRequest, PerformActionResponse};
use polarize_core::clipboard::{
    self, ClipboardReadRequest, ClipboardReadResponse, ClipboardWriteRequest,
    ClipboardWriteResponse,
};
use polarize_core::error::PolarizeError;
use polarize_core::find_text::{self, FindTextRequest, FindTextResponse};
use polarize_core::hit_test::{self, HitTestRequest, HitTestResponse};
use polarize_core::notifications::{
    DescribeNotificationsRequest, DescribeNotificationsResponse, DismissNotificationRequest,
    DismissNotificationResponse,
};
use polarize_core::orchestrate;
use polarize_core::permission::PermissionError;
use polarize_core::recording::{self, RecordFlowRequest, RecordFlowResponse};
use polarize_core::schema::{
    DescribeRequest, DescribeResponse, KeyboardRequest, KeyboardResponse, ScreenshotRequest,
    ScreenshotResponse, TapRequest, TapResponse,
};
use polarize_core::script::{
    self, RequestAutomationPermissionRequest, RequestAutomationPermissionResponse,
    RunAppleScriptRequest, RunAppleScriptResponse, ScriptDictionaryRequest,
    ScriptDictionaryResponse,
};
use polarize_core::set_value::{self, SetValueRequest, SetValueResponse};
use polarize_core::wait::{
    self, AwaitScreenIdleRequest, AwaitScreenIdleResponse, AwaitUiElementRequest,
    AwaitUiElementResponse, SystemClock,
};
use polarize_core::window_control::{
    self, SetWindowFrameRequest, SetWindowFrameResponse, WindowActionRequest, WindowActionResponse,
};
use polarize_core::workspace::{
    self, AppLaunchRequest, AppLaunchResponse, AppQuitRequest, AppQuitResponse,
    ListDisplaysRequest, ListDisplaysResponse, ListWindowsRequest, ListWindowsResponse,
};
use polarize_core::workspace_events::{
    self, AwaitWorkspaceEventRequest, AwaitWorkspaceEventResponse, FrontmostAppResponse,
};
use polarize_macos::accessibility::MacAccessibilityInspector;
use polarize_macos::action::MacActionPerformer;
use polarize_macos::applescript::MacAppleScriptRunner;
use polarize_macos::capture::MacScreenCapture;
use polarize_macos::clipboard::MacClipboard;
use polarize_macos::event_tap::MacFlowRecorder;
use polarize_macos::hit_test::MacHitTester;
use polarize_macos::input::MacInputSynthesizer;
use polarize_macos::notifications::MacNotificationCenter;
use polarize_macos::observer::MacUiChangeWaiter;
use polarize_macos::set_value::MacValueSetter;
use polarize_macos::vision::MacTextRecognizer;
use polarize_macos::window::MacWindowManager;
use polarize_macos::window_control::MacWindowController;
use polarize_macos::workspace::MacWorkspace;
use polarize_macos::workspace_events::{MacWorkspaceInspector, MacWorkspaceNotificationWaiter};
use rmcp::ErrorData;
use rmcp::ServerHandler;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::JsonObject;
use rmcp::{tool, tool_handler, tool_router};
use std::sync::Arc;

/// The `polarize` MCP server. Each field is a real `polarize-macos`
/// implementation of one `polarize-core` trait; every one is zero-sized
/// (`#[derive(Default)]` unit structs), so constructing this server has
/// no runtime cost of its own.
///
/// Only the synchronous tools hold their implementation here. A tool
/// that runs through [`blocking`] constructs its own inside the closure,
/// because the closure must own everything it touches.
#[derive(Debug, Default)]
pub struct PolarizeServer {
    capture: MacScreenCapture,
    input: MacInputSynthesizer,
    window: MacWindowManager,
    clipboard: MacClipboard,
    workspace: MacWorkspace,
    workspace_inspector: MacWorkspaceInspector,
}

/// Maps a [`PolarizeError`] to the MCP [`ErrorData`] shape a tool call
/// result carries. `Coord`/`Selector`/`Action`/`Recording`/
/// `AppNotFound`/`WindowNotFound` are treated as bad input from the
/// caller
/// (`INVALID_PARAMS`) — an `Action` refusal means the caller named an
/// element that does not offer the action, or that is disabled; `Permission` and
/// `Platform` are treated as environment/native failures
/// (`INTERNAL_ERROR`), and so are `Wait`,
/// `ScreenLocked`, and `SessionNotOnConsole` — a wait that expires and a
/// blocked login session both report that the environment did not
/// cooperate, not that the request was malformed. A permission error
/// additionally carries its
/// `PermissionKind`/`PermissionState` as structured `data` so a caller
/// can act on it (e.g. "grant Accessibility access") without parsing the
/// message string.
fn to_error_data(err: PolarizeError) -> ErrorData {
    let message = err.to_string();
    // Every tool's error path funnels through here, so this is the one
    // place that needs a log call to cover all 25 — `message` is
    // already `PolarizeError::Display`'s sanitized text, which PINV-22
    // guarantees never carries a script's raw source.
    tracing::error!(error = %message, "tool call failed");
    match &err {
        PolarizeError::Coord(_)
        | PolarizeError::Selector(_)
        | PolarizeError::Action(_)
        | PolarizeError::Recording(_)
        | PolarizeError::AppNotFound(_)
        | PolarizeError::WindowNotFound(_) => ErrorData::invalid_params(message, None),
        PolarizeError::Permission(PermissionError::NotGranted { kind, state }) => {
            let data = serde_json::to_value(serde_json::json!({
                "permission_kind": kind,
                "permission_state": state,
            }))
            .ok();
            ErrorData::internal_error(message, data)
        }
        PolarizeError::Platform(_)
        | PolarizeError::Wait(_)
        | PolarizeError::ScreenLocked
        | PolarizeError::SessionNotOnConsole => ErrorData::internal_error(message, None),
    }
}

/// Runs one blocking `polarize-core` call on `tokio`'s blocking pool, and
/// maps its result into a tool response.
///
/// A `JoinError` here means the blocking task panicked. That is a bug in
/// `polarize`, not a caller mistake, so it surfaces as an internal error
/// rather than crashing the whole server and dropping the MCP session.
async fn blocking<T, F>(work: F) -> Result<Json<T>, ErrorData>
where
    F: FnOnce() -> Result<T, PolarizeError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result.map(Json).map_err(to_error_data),
        Err(err) => Err(ErrorData::internal_error(
            format!("tool task failed: {err}"),
            None,
        )),
    }
}

/// [`KeyboardRequest`]'s `input_schema`, patched to satisfy MCP's
/// "root type must be `object`" rule.
///
/// `KeyboardRequest` is `#[serde(tag = "action", ...)]` — an internally
/// tagged enum, deliberately shaped that way in `polarize-core` so a
/// `keyboard` call is either a `type` or a `key_press`, never both (see
/// `schema.rs`). `schemars` renders that shape as `"oneOf": [...]` at the
/// schema root with no top-level `"type"` key, which is exactly correct
/// JSON Schema for a discriminated union — but `rmcp`'s own
/// `schema_for_input` rejects any root schema lacking `"type": "object"`
/// (see `rmcp::handler::server::common::validate_and_strip`), and would
/// otherwise panic while building this tool's definition. `"type":
/// "object"` and `"oneOf"` are not in tension (every branch of the
/// `oneOf` is itself an object schema), so adding the former alongside
/// the latter is valid JSON Schema and satisfies `rmcp` without changing
/// `KeyboardRequest`'s actual wire shape or its `polarize-core` tests.
fn keyboard_input_schema() -> Arc<JsonObject> {
    let mut object = (*rmcp::handler::server::common::schema_for_type::<KeyboardRequest>()).clone();
    object.insert(
        "type".to_string(),
        serde_json::Value::String("object".to_string()),
    );
    object.remove("title");
    object.remove("description");
    Arc::new(object)
}

#[tool_router]
impl PolarizeServer {
    /// Captures a window or the whole screen to PNG, optionally scoped
    /// by a bundle id or app name. Returns the PNG as base64 alongside
    /// its pixel dimensions — see
    /// [`polarize_core::schema`]'s "screenshots travel as base64" note
    /// for why.
    #[tool(name = "screenshot")]
    #[tracing::instrument(skip(self))]
    fn screenshot(
        &self,
        Parameters(request): Parameters<ScreenshotRequest>,
    ) -> Result<Json<ScreenshotResponse>, ErrorData> {
        orchestrate::perform_screenshot(&self.capture, &request)
            .map(Json)
            .map_err(to_error_data)
    }

    /// Walks the `AXUIElement` accessibility tree for the frontmost (or
    /// a named) app, returning each element's role, label, normalized
    /// `[0, 1]` frame, and focusable/interactive flags.
    #[tool(name = "describe")]
    #[tracing::instrument(skip(self))]
    async fn describe(
        &self,
        Parameters(request): Parameters<DescribeRequest>,
    ) -> Result<Json<DescribeResponse>, ErrorData> {
        blocking(move || orchestrate::perform_describe(&MacAccessibilityInspector, &request)).await
    }

    /// Posts a synthetic mouse click at a normalized `[0.0, 1.0]`
    /// fraction point of a screen or window — the same coordinate
    /// contract as argent's `gesture-tap`. The fraction is converted to
    /// a real pixel point (against the target's resolved size) before
    /// any native call runs; see PINV-4 in `docs/INVARIANTS.md`.
    #[tool(name = "tap")]
    #[tracing::instrument(skip(self))]
    fn tap(
        &self,
        Parameters(request): Parameters<TapRequest>,
    ) -> Result<Json<TapResponse>, ErrorData> {
        orchestrate::perform_tap(&self.window, &self.input, &request)
            .map(Json)
            .map_err(to_error_data)
    }

    /// Posts synthetic key events: either types a literal string, or
    /// presses one named key with optional modifiers. When the request
    /// names a `target` app, activates that app first — see PINV-14 in
    /// `docs/INVARIANTS.md`.
    #[tool(name = "keyboard", input_schema = keyboard_input_schema())]
    // A `type` request's `text` is exactly what a caller types — which
    // can be a password. Skipped whole, the same reasoning PINV-40
    // gives for withholding `record_flow`'s captured characters.
    #[tracing::instrument(skip(self, request))]
    fn keyboard(
        &self,
        Parameters(request): Parameters<KeyboardRequest>,
    ) -> Result<Json<KeyboardResponse>, ErrorData> {
        orchestrate::perform_keyboard(&self.window, &self.input, &request)
            .map(Json)
            .map_err(to_error_data)
    }

    /// Presses one element through its own accessibility action, rather
    /// than by posting a click at a coordinate. The request names the
    /// element by identifier, role, subrole, or label — see
    /// [`polarize_core::selector`]. An occluded element, an element
    /// below click-target size, and an element the caller cannot
    /// locate on screen all still work. The tool refuses an action the
    /// element does not publish, and refuses a disabled element, before
    /// it calls the platform (PINV-17).
    #[tool(name = "perform_action")]
    #[tracing::instrument(skip(self))]
    async fn perform_action(
        &self,
        Parameters(request): Parameters<PerformActionRequest>,
    ) -> Result<Json<PerformActionResponse>, ErrorData> {
        blocking(move || {
            action::perform_element_action(
                &MacAccessibilityInspector,
                &MacActionPerformer,
                &request,
            )
        })
        .await
    }

    /// Waits until an element appears in an app's accessibility tree, or
    /// until the request's timeout expires. The wait wakes on an
    /// `AXObserver` notification, and re-reads the tree every poll
    /// interval regardless, because some trees never post one
    /// (PINV-19).
    #[tool(name = "await_ui_element")]
    #[tracing::instrument(skip(self))]
    async fn await_ui_element(
        &self,
        Parameters(request): Parameters<AwaitUiElementRequest>,
    ) -> Result<Json<AwaitUiElementResponse>, ErrorData> {
        blocking(move || {
            wait::perform_await_ui_element(
                &MacAccessibilityInspector,
                &MacUiChangeWaiter,
                &SystemClock::new(),
                &request,
            )
        })
        .await
    }

    /// Waits until an app's accessibility tree stops changing for the
    /// requested idle window. Use it after an action that starts an
    /// animation or a load, when there is no single element to wait for.
    #[tool(name = "await_screen_idle")]
    #[tracing::instrument(skip(self))]
    async fn await_screen_idle(
        &self,
        Parameters(request): Parameters<AwaitScreenIdleRequest>,
    ) -> Result<Json<AwaitScreenIdleResponse>, ErrorData> {
        blocking(move || {
            wait::perform_await_screen_idle(
                &MacAccessibilityInspector,
                &MacUiChangeWaiter,
                &SystemClock::new(),
                &request,
            )
        })
        .await
    }

    /// Runs AppleScript source through `osascript`, optionally wrapped in
    /// a `tell application` block for a named target. This reaches
    /// scriptable apps — Finder, Mail, Safari, Music, Notes — with
    /// semantic operations no accessibility or `CGEvent` call can express.
    #[tool(name = "run_applescript")]
    // `request.source` is the raw script text, which can carry a
    // password (PINV-22) — so `request` is skipped whole, not just
    // narrowed, the same way PINV-22 keeps it out of every error too.
    #[tracing::instrument(skip(self, request), fields(target_app = ?request.target_app))]
    async fn run_applescript(
        &self,
        Parameters(request): Parameters<RunAppleScriptRequest>,
    ) -> Result<Json<RunAppleScriptResponse>, ErrorData> {
        blocking(move || script::perform_run_applescript(&MacAppleScriptRunner, &request)).await
    }

    /// Lists a scriptable app's own verbs and classes, read from its
    /// `sdef` scripting dictionary. Call it before `run_applescript` to
    /// find out what an app accepts.
    #[tool(name = "script_dictionary")]
    #[tracing::instrument(skip(self))]
    async fn script_dictionary(
        &self,
        Parameters(request): Parameters<ScriptDictionaryRequest>,
    ) -> Result<Json<ScriptDictionaryResponse>, ErrorData> {
        blocking(move || script::perform_script_dictionary(&MacAppleScriptRunner, &request)).await
    }

    /// One-time setup: shows the system Automation consent dialog for
    /// one target app, if its permission is not already determined, and
    /// reports the resulting state. Call this once per app
    /// `run_applescript` needs to reach — Automation is granted per
    /// (this process, target app) pair, not once for the whole binary.
    /// Unlike `run_applescript` itself, this never refuses locally
    /// before trying: it always attempts a real, harmless script send,
    /// which is what actually raises the system dialog.
    #[tool(name = "request_automation_permission")]
    #[tracing::instrument(skip(self))]
    async fn request_automation_permission(
        &self,
        Parameters(request): Parameters<RequestAutomationPermissionRequest>,
    ) -> Result<Json<RequestAutomationPermissionResponse>, ErrorData> {
        blocking(move || {
            Ok(script::perform_request_automation_permission(
                &MacAppleScriptRunner,
                &request,
            ))
        })
        .await
    }

    /// Writes one accessibility attribute of one element directly:
    /// text, a number, or a selected-text range. This avoids the focus
    /// race and the keyboard-layout dependence of simulated typing. Read
    /// PINV-27 first: web content often accepts the write into the DOM
    /// without firing `input` or `keydown`, so a controlled component
    /// can update and then snap back.
    #[tool(name = "set_value")]
    // A text write's value is exactly what a caller writes into the
    // element — which can be a password. Skipped whole, same reasoning
    // as `keyboard`.
    #[tracing::instrument(skip(self, request), fields(selector = ?request.selector))]
    async fn set_value(
        &self,
        Parameters(request): Parameters<SetValueRequest>,
    ) -> Result<Json<SetValueResponse>, ErrorData> {
        blocking(move || {
            set_value::set_element_value(&MacAccessibilityInspector, &MacValueSetter, &request)
        })
        .await
    }

    /// Reports the element that really sits under a point, asking the
    /// system-wide accessibility element so another app's window counts
    /// as occlusion. It resolves the same fraction `tap` resolves, so a
    /// caller can preflight a click with it (PINV-32).
    #[tool(name = "hit_test_at_point")]
    #[tracing::instrument(skip(self))]
    async fn hit_test_at_point(
        &self,
        Parameters(request): Parameters<HitTestRequest>,
    ) -> Result<Json<HitTestResponse>, ErrorData> {
        blocking(move || hit_test::perform_hit_test(&MacWindowManager, &MacHitTester, &request))
            .await
    }

    /// Reads the general pasteboard. A refused read reports a permission
    /// error rather than empty text, because macOS can withhold the
    /// contents from a programmatic read (PINV-34).
    #[tool(name = "clipboard_read")]
    #[tracing::instrument(skip(self))]
    async fn clipboard_read(
        &self,
        Parameters(request): Parameters<ClipboardReadRequest>,
    ) -> Result<Json<ClipboardReadResponse>, ErrorData> {
        // macOS 26 can raise a consent prompt on a programmatic read
        // (PINV-34), and that prompt blocks until the user answers it.
        blocking(move || clipboard::perform_clipboard_read(&MacClipboard, &request)).await
    }

    /// Replaces the pasteboard contents. macOS never refuses a write, so
    /// this tool needs no permission.
    #[tool(name = "clipboard_write")]
    // `request.text` becomes the pasteboard contents — which can be a
    // password a caller is about to paste elsewhere. Skipped whole,
    // same reasoning as `keyboard`.
    #[tracing::instrument(skip(self, request))]
    fn clipboard_write(
        &self,
        Parameters(request): Parameters<ClipboardWriteRequest>,
    ) -> Result<Json<ClipboardWriteResponse>, ErrorData> {
        clipboard::perform_clipboard_write(&self.clipboard, &request)
            .map(Json)
            .map_err(to_error_data)
    }

    /// Moves and resizes one window. The response reports the frame the
    /// window really ended up with, re-read after the write, plus
    /// whether it matched the request — apps clamp their own minimum and
    /// maximum size (PINV-29).
    #[tool(name = "set_window_frame")]
    #[tracing::instrument(skip(self))]
    async fn set_window_frame(
        &self,
        Parameters(request): Parameters<SetWindowFrameRequest>,
    ) -> Result<Json<SetWindowFrameResponse>, ErrorData> {
        blocking(move || {
            window_control::perform_set_window_frame(
                &MacWindowManager,
                &MacWindowController,
                &request,
            )
        })
        .await
    }

    /// Minimizes, restores, focuses, closes, or full-screens one window.
    /// A full-screen action against a window that publishes no
    /// `AXFullScreen` is refused rather than silently ignored.
    #[tool(name = "window_action")]
    #[tracing::instrument(skip(self))]
    async fn window_action(
        &self,
        Parameters(request): Parameters<WindowActionRequest>,
    ) -> Result<Json<WindowActionResponse>, ErrorData> {
        blocking(move || {
            window_control::perform_window_action(&MacWindowManager, &MacWindowController, &request)
        })
        .await
    }

    /// Lists an app's windows, merging the accessibility window list
    /// with the window server's for durable window ids and an
    /// on-screen flag. A window only one source knows about still
    /// appears, with the other source's fields absent rather than false
    /// (PINV-30).
    #[tool(name = "list_windows")]
    #[tracing::instrument(skip(self))]
    async fn list_windows(
        &self,
        Parameters(request): Parameters<ListWindowsRequest>,
    ) -> Result<Json<ListWindowsResponse>, ErrorData> {
        blocking(move || workspace::perform_list_windows(&MacWorkspace, &request)).await
    }

    /// Starts an app, or reports it was already running.
    #[tool(name = "app_launch")]
    #[tracing::instrument(skip(self))]
    async fn app_launch(
        &self,
        Parameters(request): Parameters<AppLaunchRequest>,
    ) -> Result<Json<AppLaunchResponse>, ErrorData> {
        blocking(move || {
            workspace::perform_app_launch(&MacWorkspace, &SystemClock::new(), &request)
        })
        .await
    }

    /// Asks an app to exit, politely by default. A caller must ask for
    /// `force` explicitly, because a forced quit discards unsaved work
    /// (PINV-31). The response reports whether the app really exited.
    #[tool(name = "app_quit")]
    #[tracing::instrument(skip(self))]
    async fn app_quit(
        &self,
        Parameters(request): Parameters<AppQuitRequest>,
    ) -> Result<Json<AppQuitResponse>, ErrorData> {
        blocking(move || workspace::perform_app_quit(&MacWorkspace, &SystemClock::new(), &request))
            .await
    }

    /// Reports every active display, with bounds in the same global
    /// pixel space `screenshot` and `tap` use.
    #[tool(name = "list_displays")]
    #[tracing::instrument(skip(self))]
    fn list_displays(
        &self,
        Parameters(request): Parameters<ListDisplaysRequest>,
    ) -> Result<Json<ListDisplaysResponse>, ErrorData> {
        workspace::perform_list_displays(&self.workspace, &request)
            .map(Json)
            .map_err(to_error_data)
    }

    /// Finds on-screen text with Vision OCR, for apps whose
    /// accessibility tree is sparse, missing, or wrong. Each match
    /// carries a normalized frame a caller can hand straight to `tap`.
    ///
    /// The first call after an OS update takes roughly 27 seconds, while
    /// macOS compiles the recognition model. Later calls take about
    /// 100 ms.
    #[tool(name = "find_text")]
    #[tracing::instrument(skip(self))]
    async fn find_text(
        &self,
        Parameters(request): Parameters<FindTextRequest>,
    ) -> Result<Json<FindTextResponse>, ErrorData> {
        blocking(move || {
            find_text::perform_find_text(&MacScreenCapture, &MacTextRecognizer, &request)
        })
        .await
    }

    /// Reads every notification banner on screen: its app, title, body,
    /// and frame. Banners are found by structure, not by subrole string,
    /// because those shift between macOS releases (PINV-35).
    #[tool(name = "describe_notifications")]
    #[tracing::instrument(skip(self))]
    async fn describe_notifications(
        &self,
        Parameters(request): Parameters<DescribeNotificationsRequest>,
    ) -> Result<Json<DescribeNotificationsResponse>, ErrorData> {
        blocking(move || MacNotificationCenter::default().describe(&request)).await
    }

    /// Closes one notification banner, then re-reads to report whether
    /// it really went away.
    #[tool(name = "dismiss_notification")]
    #[tracing::instrument(skip(self))]
    async fn dismiss_notification(
        &self,
        Parameters(request): Parameters<DismissNotificationRequest>,
    ) -> Result<Json<DismissNotificationResponse>, ErrorData> {
        blocking(move || MacNotificationCenter::default().dismiss(&request)).await
    }

    /// Reports the app that holds focus right now.
    #[tool(name = "frontmost_app")]
    #[tracing::instrument(skip(self))]
    fn frontmost_app(&self) -> Result<Json<FrontmostAppResponse>, ErrorData> {
        workspace_events::perform_frontmost_app(&self.workspace_inspector)
            .map(Json)
            .map_err(to_error_data)
    }

    /// Records real mouse and keyboard input for a bounded window, then
    /// returns it. The tap listens only: it never modifies or swallows
    /// an event, so the user keeps full control of their Mac while a
    /// recording runs (PINV-39).
    ///
    /// This needs the **Input Monitoring** grant, which is not the
    /// Accessibility grant the other tools use.
    ///
    /// Typed characters are withheld unless the caller opts in, because
    /// a recording captures real keystrokes and those include passwords
    /// (PINV-40). Only input during the call is recorded.
    #[tool(name = "record_flow")]
    #[tracing::instrument(skip(self))]
    async fn record_flow(
        &self,
        Parameters(request): Parameters<RecordFlowRequest>,
    ) -> Result<Json<RecordFlowResponse>, ErrorData> {
        blocking(move || recording::perform_record_flow(&MacFlowRecorder, &MacWorkspace, &request))
            .await
    }

    /// Waits for an app switch, a wake, or a session change. It watches
    /// the real `NSWorkspace` notifications and a polled snapshot diff
    /// at the same time, and names which channel saw the event
    /// (PINV-36).
    #[tool(name = "await_workspace_event")]
    #[tracing::instrument(skip(self))]
    async fn await_workspace_event(
        &self,
        Parameters(request): Parameters<AwaitWorkspaceEventRequest>,
    ) -> Result<Json<AwaitWorkspaceEventResponse>, ErrorData> {
        blocking(move || {
            workspace_events::perform_await_workspace_event(
                &MacWorkspaceInspector,
                &MacWorkspaceNotificationWaiter,
                &SystemClock::new(),
                &request,
            )
        })
        .await
    }
}

/// `#[tool_router(server_handler)]` would emit this `impl ServerHandler`
/// for us. Its shorthand cannot pass a server `name`, though. Without
/// one, it falls back to `Implementation::from_build_env()`. That
/// resolves at `rmcp`'s own compile time, not this crate's, and would
/// report the server as `"rmcp"` to every MCP client.
///
/// Writing this block out explicitly fixes both problems at once:
/// `#[tool_handler]` splices `name = "polarize"` into *our* crate's
/// compilation, and the omitted `version` field's `env!("CARGO_PKG_VERSION")`
/// then resolves to `apps/polarize`'s own version too.
#[tool_handler(name = "polarize")]
impl ServerHandler for PolarizeServer {}
