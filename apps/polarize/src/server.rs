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
//! - `perform_action` walks a whole accessibility tree, then makes a
//!   synchronous `AXUIElementPerformAction` call that some apps answer
//!   slowly.
//!
//! `screenshot`, `describe`, `tap`, and `keyboard` stay synchronous.
//! Each returns in milliseconds.
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
use polarize_core::error::PolarizeError;
use polarize_core::orchestrate;
use polarize_core::permission::PermissionError;
use polarize_core::schema::{
    DescribeRequest, DescribeResponse, KeyboardRequest, KeyboardResponse, ScreenshotRequest,
    ScreenshotResponse, TapRequest, TapResponse,
};
use polarize_core::script::{
    self, RunAppleScriptRequest, RunAppleScriptResponse, ScriptDictionaryRequest,
    ScriptDictionaryResponse,
};
use polarize_core::wait::{
    self, AwaitScreenIdleRequest, AwaitScreenIdleResponse, AwaitUiElementRequest,
    AwaitUiElementResponse, SystemClock,
};
use polarize_macos::accessibility::MacAccessibilityInspector;
use polarize_macos::action::MacActionPerformer;
use polarize_macos::applescript::MacAppleScriptRunner;
use polarize_macos::capture::MacScreenCapture;
use polarize_macos::input::MacInputSynthesizer;
use polarize_macos::observer::MacUiChangeWaiter;
use polarize_macos::window::MacWindowManager;
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
    inspector: MacAccessibilityInspector,
    input: MacInputSynthesizer,
    window: MacWindowManager,
}

/// Maps a [`PolarizeError`] to the MCP [`ErrorData`] shape a tool call
/// result carries. `Coord`/`Selector`/`Action`/`AppNotFound`/
/// `WindowNotFound` are treated as bad input from the caller
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
    match &err {
        PolarizeError::Coord(_)
        | PolarizeError::Selector(_)
        | PolarizeError::Action(_)
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
    fn describe(
        &self,
        Parameters(request): Parameters<DescribeRequest>,
    ) -> Result<Json<DescribeResponse>, ErrorData> {
        orchestrate::perform_describe(&self.inspector, &request)
            .map(Json)
            .map_err(to_error_data)
    }

    /// Posts a synthetic mouse click at a normalized `[0.0, 1.0]`
    /// fraction point of a screen or window — the same coordinate
    /// contract as argent's `gesture-tap`. The fraction is converted to
    /// a real pixel point (against the target's resolved size) before
    /// any native call runs; see PINV-4 in `docs/INVARIANTS.md`.
    #[tool(name = "tap")]
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
    async fn script_dictionary(
        &self,
        Parameters(request): Parameters<ScriptDictionaryRequest>,
    ) -> Result<Json<ScriptDictionaryResponse>, ErrorData> {
        blocking(move || script::perform_script_dictionary(&MacAppleScriptRunner, &request)).await
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
