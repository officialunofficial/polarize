//! macOS TCC permission state, and the pure logic that gates each MCP
//! tool on the permission it needs.
//!
//! `polarize-core` never queries or requests a real permission — that is
//! `polarize-macos`'s job (asking `CGPreflightScreenCaptureAccess` /
//! `AXIsProcessTrusted`, or their equivalents). What lives here is the
//! state enum itself and the pure decision of which permission a tool
//! needs and whether a given state satisfies it, both fully testable
//! without touching TCC.

use std::fmt;

/// The state of one macOS TCC permission, mirroring the states Apple's
/// own APIs report (`kTCCAuthorizationStatus*` / `AXIsProcessTrusted`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    /// The user has never been asked.
    NotDetermined,
    /// The user (or an MDM profile) explicitly denied it.
    Denied,
    /// Granted — the permission can be used.
    Granted,
    /// Blocked by policy (e.g. parental controls, MDM) rather than by a
    /// user choice; macOS reports this distinctly from `Denied`.
    Restricted,
}

impl PermissionState {
    /// Whether a tool gated on this permission may proceed.
    pub fn is_usable(self) -> bool {
        matches!(self, PermissionState::Granted)
    }
}

/// A macOS permission `polarize`'s tools depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    /// Screen Recording — required to capture pixels via
    /// `ScreenCaptureKit`.
    ScreenRecording,
    /// Accessibility — required to walk the `AXUIElement` tree and to
    /// post synthetic `CGEvent`s that other apps receive.
    Accessibility,
    /// Automation — required to send Apple Events to another app, which
    /// is what every `run_applescript` call does. macOS grants this per
    /// (caller, target) pair, not once for the whole system. See
    /// PINV-21.
    Automation,
    /// Clipboard — macOS 26 can ask the user to allow a programmatic
    /// read of the pasteboard. A read that no paste gesture preceded is
    /// the case that prompts. A write never prompts. See PINV-34 and
    /// [`crate::clipboard`].
    Clipboard,
    /// Input Monitoring — required to open a listen-only `CGEventTap`
    /// and read the real input a user makes. macOS calls this grant
    /// `kTCCServiceListenEvent`, and `CGPreflightListenEventAccess`
    /// reports it. It is **not** the Accessibility grant that posts
    /// synthetic events, and it lives in its own System Settings pane.
    /// See [`crate::recording`] and PINV-39.
    InputMonitoring,
}

impl fmt::Display for PermissionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PermissionKind::ScreenRecording => write!(f, "Screen Recording"),
            PermissionKind::Accessibility => write!(f, "Accessibility"),
            PermissionKind::Automation => write!(f, "Automation"),
            PermissionKind::Clipboard => write!(f, "Clipboard"),
            // The exact name of the System Settings pane. A caller who
            // reads "Accessibility" here goes to the wrong pane, grants
            // a permission polarize already holds, and sees no change.
            PermissionKind::InputMonitoring => write!(f, "Input Monitoring"),
        }
    }
}

/// The current state of a specific permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PermissionStatus {
    pub kind: PermissionKind,
    pub state: PermissionState,
}

/// One of `polarize`'s MCP tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Screenshot,
    Describe,
    Tap,
    Keyboard,
    /// `perform_action` — presses one element through its own AX
    /// action. See `crate::action` (PINV-17).
    PerformAction,
    /// `await_ui_element` — see [`crate::wait`].
    AwaitUiElement,
    /// `await_screen_idle` — see [`crate::wait`].
    AwaitScreenIdle,
    RunAppleScript,
    /// `script_dictionary` — reads an app's scripting dictionary via the
    /// `sdef` CLI tool, a plain resource read. See `crate::script`.
    ScriptDictionary,
    /// `set_value` — writes one AX attribute of one element. See
    /// `crate::set_value` (PINV-26).
    SetValue,
    /// `hit_test_at_point` — see [`crate::hit_test`] (PINV-32).
    HitTest,
    /// `clipboard_read` — see [`crate::clipboard`] (PINV-34).
    ClipboardRead,
    /// `clipboard_write` — see [`crate::clipboard`].
    ClipboardWrite,
    /// `set_window_frame` — moves and resizes one window. See
    /// `crate::window_control` (PINV-28, PINV-29).
    SetWindowFrame,
    /// `window_action` — minimizes, restores, focuses, closes, or
    /// full-screens one window. See `crate::window_control`.
    WindowAction,
    /// `find_text` — OCR fallback over the captured screen. See
    /// `crate::find_text` (PINV-37, PINV-38).
    FindText,
    /// `describe_notifications` — reads every notification banner on
    /// screen. See `crate::notifications` (PINV-35).
    DescribeNotifications,
    /// `dismiss_notification` — closes one banner. See
    /// `crate::notifications` (PINV-35).
    DismissNotification,
    /// `list_windows` — merges the AX window list with the window
    /// server's. See `crate::workspace` (PINV-30).
    ListWindows,
    /// `app_launch` — starts an app. See `crate::workspace`.
    AppLaunch,
    /// `app_quit` — asks an app to exit. See `crate::workspace`
    /// (PINV-31).
    AppQuit,
    /// `list_displays` — reports every active display. See
    /// `crate::workspace`.
    ListDisplays,
    /// `frontmost_app` — reports the app that holds focus now. See
    /// `crate::workspace_events` (PINV-36).
    FrontmostApp,
    /// `await_workspace_event` — waits for an app switch, a wake, or a
    /// session change. See `crate::workspace_events` (PINV-36).
    AwaitWorkspaceEvent,
    /// `record_flow` — records the real input a user makes, for a
    /// bounded time. See `crate::recording` (PINV-39, PINV-40).
    RecordFlow,
}

/// # PINV-2: every tool call is gated on at most one permission
///
/// - Always: [`required_permission`] maps each [`ToolKind`] to at most
///   one [`PermissionKind`], and [`check_permission`] refuses to run a
///   tool whose required permission is not `Granted`. A tool that needs
///   no TCC grant maps to `None`, and always passes the check.
/// - Because: `polarize-macos`'s native calls fail in ways that are easy
///   to misdiagnose from the raw OS error alone (a denied AX permission
///   and a genuinely missing UI element can both surface as "element not
///   found"). Checking permission state first turns that into an
///   unambiguous, actionable error before the native call ever runs.
/// - If violated: a caller sees a confusing native failure (or, worse, a
///   `tap`/`keyboard` call that silently no-ops) instead of "grant
///   Accessibility access to run this tool".
pub fn required_permission(tool: ToolKind) -> Option<PermissionKind> {
    match tool {
        ToolKind::Screenshot => Some(PermissionKind::ScreenRecording),
        ToolKind::Describe | ToolKind::Tap | ToolKind::Keyboard => {
            Some(PermissionKind::Accessibility)
        }
        ToolKind::PerformAction => Some(PermissionKind::Accessibility),
        // Both `await` tools read the accessibility tree, and
        // `AXObserverCreate` needs the same trust as `AXUIElement` does.
        ToolKind::AwaitUiElement | ToolKind::AwaitScreenIdle => Some(PermissionKind::Accessibility),
        ToolKind::RunAppleScript => Some(PermissionKind::Automation),
        // `sdef` reads a bundle's scripting-definition resource straight
        // off disk. It sends no Apple Event, so it needs no Automation
        // grant — unlike `run_applescript`, which actually drives the
        // target app.
        ToolKind::ScriptDictionary => None,
        // `set_value` writes through `AXUIElementSetAttributeValue`,
        // which needs the same trust every other AX call needs.
        ToolKind::SetValue => Some(PermissionKind::Accessibility),
        // A hit test reads the accessibility tree, so it needs the same
        // trust `describe` needs.
        ToolKind::HitTest => Some(PermissionKind::Accessibility),
        // macOS can withhold the pasteboard contents from a read
        // (PINV-34). It never refuses a write, so a write is gated on
        // nothing.
        ToolKind::ClipboardRead => Some(PermissionKind::Clipboard),
        ToolKind::ClipboardWrite => None,
        // Both window tools read and write `AXUIElement` attributes.
        ToolKind::SetWindowFrame | ToolKind::WindowAction => Some(PermissionKind::Accessibility),
        // `find_text` adds no new TCC surface. Vision itself needs no
        // permission. Its pixels come from the same `ScreenCaptureKit`
        // capture `screenshot` uses, so it needs the same one.
        ToolKind::FindText => Some(PermissionKind::ScreenRecording),
        // Both notification tools read the notification centre's
        // accessibility tree, and one of them presses a control in it.
        // See PINV-35.
        ToolKind::DescribeNotifications | ToolKind::DismissNotification => {
            Some(PermissionKind::Accessibility)
        }
        // `list_windows` reads the AX window list for titles and frames.
        ToolKind::ListWindows => Some(PermissionKind::Accessibility),
        // These need no TCC grant at all. `NSWorkspace` app control,
        // `CGGetActiveDisplayList`, and reading the frontmost app are
        // all unprivileged, and a clipboard write never prompts. Naming
        // a permission here would put a false row in the very table this
        // function exists to keep honest.
        ToolKind::AppLaunch
        | ToolKind::AppQuit
        | ToolKind::ListDisplays
        | ToolKind::FrontmostApp
        | ToolKind::AwaitWorkspaceEvent => None,
        // A listen-only `CGEventTap` needs Input Monitoring, and only
        // Input Monitoring. The Accessibility grant that posts a
        // `CGEvent` does not open a tap that reads one. See PINV-39.
        ToolKind::RecordFlow => Some(PermissionKind::InputMonitoring),
    }
}

/// Error returned when a tool's required permission is not granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PermissionError {
    #[error("{kind} permission is {state:?}, not granted")]
    NotGranted {
        kind: PermissionKind,
        state: PermissionState,
    },
}

/// Checks that `tool`'s required permission is `Granted` among the given
/// `statuses`. A permission absent from `statuses` is treated as
/// [`PermissionState::NotDetermined`] — see PINV-2.
pub fn check_permission(
    tool: ToolKind,
    statuses: &[PermissionStatus],
) -> Result<(), PermissionError> {
    let Some(kind) = required_permission(tool) else {
        // Nothing to withhold, so nothing can block this call.
        return Ok(());
    };
    let state = statuses
        .iter()
        .find(|status| status.kind == kind)
        .map(|status| status.state)
        .unwrap_or(PermissionState::NotDetermined);

    if state.is_usable() {
        Ok(())
    } else {
        Err(PermissionError::NotGranted { kind, state })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granted_is_usable() {
        assert!(PermissionState::Granted.is_usable());
    }

    #[test]
    fn not_determined_denied_and_restricted_are_not_usable() {
        assert!(!PermissionState::NotDetermined.is_usable());
        assert!(!PermissionState::Denied.is_usable());
        assert!(!PermissionState::Restricted.is_usable());
    }

    #[test]
    fn a_tool_that_needs_no_permission_reports_none() {
        // Three tools genuinely need no TCC grant. Naming one anyway
        // would put a false row in the table PINV-2 makes auditable.
        assert_eq!(required_permission(ToolKind::AppLaunch), None);
        assert_eq!(required_permission(ToolKind::AppQuit), None);
        assert_eq!(required_permission(ToolKind::ListDisplays), None);
        assert_eq!(required_permission(ToolKind::ClipboardWrite), None);
        assert_eq!(required_permission(ToolKind::FrontmostApp), None);
        assert_eq!(required_permission(ToolKind::AwaitWorkspaceEvent), None);
    }

    #[test]
    fn a_tool_that_needs_no_permission_always_passes_the_check() {
        // No permission required means nothing can withhold it, even
        // when every grant this process holds is denied.
        let denied = [
            PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Denied,
            },
            PermissionStatus {
                kind: PermissionKind::ScreenRecording,
                state: PermissionState::Denied,
            },
        ];
        assert!(check_permission(ToolKind::AppLaunch, &denied).is_ok());
        assert!(check_permission(ToolKind::ListDisplays, &[]).is_ok());
    }

    #[test]
    fn list_windows_still_requires_accessibility() {
        // It reads the AX window list, so it is not in the free set.
        assert_eq!(
            required_permission(ToolKind::ListWindows),
            Some(PermissionKind::Accessibility)
        );
    }

    #[test]
    fn screenshot_requires_screen_recording() {
        assert_eq!(
            required_permission(ToolKind::Screenshot),
            Some(PermissionKind::ScreenRecording)
        );
    }

    #[test]
    fn describe_tap_and_keyboard_require_accessibility() {
        assert_eq!(
            required_permission(ToolKind::Describe),
            Some(PermissionKind::Accessibility)
        );
        assert_eq!(
            required_permission(ToolKind::Tap),
            Some(PermissionKind::Accessibility)
        );
        assert_eq!(
            required_permission(ToolKind::Keyboard),
            Some(PermissionKind::Accessibility)
        );
    }

    #[test]
    fn check_permission_passes_when_granted() {
        let statuses = [PermissionStatus {
            kind: PermissionKind::ScreenRecording,
            state: PermissionState::Granted,
        }];
        assert!(check_permission(ToolKind::Screenshot, &statuses).is_ok());
    }

    #[test]
    fn check_permission_fails_when_denied() {
        let statuses = [PermissionStatus {
            kind: PermissionKind::Accessibility,
            state: PermissionState::Denied,
        }];
        let err = check_permission(ToolKind::Tap, &statuses).unwrap_err();
        assert_eq!(
            err,
            PermissionError::NotGranted {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Denied
            }
        );
    }

    #[test]
    fn check_permission_treats_absent_status_as_not_determined() {
        let err = check_permission(ToolKind::Describe, &[]).unwrap_err();
        assert_eq!(
            err,
            PermissionError::NotGranted {
                kind: PermissionKind::Accessibility,
                state: PermissionState::NotDetermined
            }
        );
    }

    #[test]
    fn perform_action_requires_accessibility() {
        assert_eq!(
            required_permission(ToolKind::PerformAction),
            Some(PermissionKind::Accessibility)
        );
    }

    #[test]
    fn check_permission_ignores_unrelated_statuses() {
        let statuses = [PermissionStatus {
            kind: PermissionKind::ScreenRecording,
            state: PermissionState::Granted,
        }];
        // Tap needs Accessibility; a granted ScreenRecording status must
        // not satisfy it.
        let err = check_permission(ToolKind::Tap, &statuses).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Accessibility permission is NotDetermined, not granted"
        );
    }
    #[test]
    fn the_await_tools_require_accessibility() {
        assert_eq!(
            required_permission(ToolKind::AwaitUiElement),
            Some(PermissionKind::Accessibility)
        );
        assert_eq!(
            required_permission(ToolKind::AwaitScreenIdle),
            Some(PermissionKind::Accessibility)
        );
    }

    #[test]
    fn run_applescript_requires_automation() {
        assert_eq!(
            required_permission(ToolKind::RunAppleScript),
            Some(PermissionKind::Automation)
        );
    }

    #[test]
    fn script_dictionary_needs_no_permission() {
        // `sdef` reads a bundle resource off disk; it sends no Apple
        // Event, unlike `run_applescript`.
        assert_eq!(required_permission(ToolKind::ScriptDictionary), None);
    }

    #[test]
    fn automation_permission_displays_its_settings_name() {
        assert_eq!(PermissionKind::Automation.to_string(), "Automation");
    }

    #[test]
    fn find_text_requires_screen_recording_and_adds_no_new_permission() {
        assert_eq!(
            required_permission(ToolKind::FindText),
            Some(PermissionKind::ScreenRecording)
        );
        assert_eq!(
            required_permission(ToolKind::FindText),
            required_permission(ToolKind::Screenshot)
        );
    }

    #[test]
    fn check_permission_reports_automation_by_name() {
        let statuses = [PermissionStatus {
            kind: PermissionKind::Automation,
            state: PermissionState::Denied,
        }];
        let err = check_permission(ToolKind::RunAppleScript, &statuses).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Automation permission is Denied, not granted"
        );
    }

    #[test]
    fn set_value_requires_accessibility() {
        assert_eq!(
            required_permission(ToolKind::SetValue),
            Some(PermissionKind::Accessibility)
        );
    }

    #[test]
    fn hit_test_requires_accessibility() {
        assert_eq!(
            required_permission(ToolKind::HitTest),
            Some(PermissionKind::Accessibility)
        );
    }

    #[test]
    fn only_the_clipboard_read_needs_a_permission() {
        // macOS can withhold the pasteboard contents from a read, so a
        // read is gated. It never refuses a write, so gating a write
        // would claim a grant `polarize` does not use. See PINV-34.
        assert_eq!(
            required_permission(ToolKind::ClipboardRead),
            Some(PermissionKind::Clipboard)
        );
        assert_eq!(required_permission(ToolKind::ClipboardWrite), None);
    }

    #[test]
    fn clipboard_permission_displays_its_own_name() {
        assert_eq!(PermissionKind::Clipboard.to_string(), "Clipboard");
    }

    #[test]
    fn check_permission_reports_a_refused_clipboard_read() {
        let statuses = [PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::NotDetermined,
        }];
        let err = check_permission(ToolKind::ClipboardRead, &statuses).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Clipboard permission is NotDetermined, not granted"
        );
    }
    #[test]
    fn both_window_tools_need_accessibility() {
        assert_eq!(
            required_permission(ToolKind::SetWindowFrame),
            Some(PermissionKind::Accessibility)
        );
        assert_eq!(
            required_permission(ToolKind::WindowAction),
            Some(PermissionKind::Accessibility)
        );
    }
}

// ---- the workspace tools: list_windows, app_launch, app_quit, list_displays ----

// ---- flow recording: record_flow (PINV-39, PINV-40) ----------------------

#[cfg(test)]
mod recording_permission_tests {
    use super::*;

    #[test]
    fn record_flow_requires_input_monitoring() {
        assert_eq!(
            required_permission(ToolKind::RecordFlow),
            Some(PermissionKind::InputMonitoring)
        );
    }

    #[test]
    fn record_flow_does_not_reuse_the_accessibility_grant() {
        // Posting a `CGEvent` and listening for one are two different
        // TCC grants. A caller sent to the Accessibility pane grants
        // what polarize already holds, and nothing improves.
        assert_ne!(
            required_permission(ToolKind::RecordFlow),
            required_permission(ToolKind::Tap)
        );
        assert_ne!(
            required_permission(ToolKind::RecordFlow),
            required_permission(ToolKind::Keyboard)
        );
    }

    #[test]
    fn input_monitoring_displays_its_settings_pane_name() {
        assert_eq!(
            PermissionKind::InputMonitoring.to_string(),
            "Input Monitoring"
        );
    }

    #[test]
    fn a_granted_accessibility_status_does_not_satisfy_record_flow() {
        let statuses = [PermissionStatus {
            kind: PermissionKind::Accessibility,
            state: PermissionState::Granted,
        }];
        let err = check_permission(ToolKind::RecordFlow, &statuses).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Input Monitoring permission is NotDetermined, not granted"
        );
    }

    #[test]
    fn a_granted_input_monitoring_status_lets_record_flow_run() {
        let statuses = [PermissionStatus {
            kind: PermissionKind::InputMonitoring,
            state: PermissionState::Granted,
        }];
        assert!(check_permission(ToolKind::RecordFlow, &statuses).is_ok());
    }

    #[test]
    fn a_denied_input_monitoring_status_names_the_pane() {
        let statuses = [PermissionStatus {
            kind: PermissionKind::InputMonitoring,
            state: PermissionState::Denied,
        }];
        let err = check_permission(ToolKind::RecordFlow, &statuses).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Input Monitoring permission is Denied, not granted"
        );
    }
}
