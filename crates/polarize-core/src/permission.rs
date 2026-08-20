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
}

impl fmt::Display for PermissionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PermissionKind::ScreenRecording => write!(f, "Screen Recording"),
            PermissionKind::Accessibility => write!(f, "Accessibility"),
            PermissionKind::Automation => write!(f, "Automation"),
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
    ScriptDictionary,
}

/// # PINV-2: every tool call is gated on exactly one permission
///
/// - Always: [`required_permission`] maps each [`ToolKind`] to exactly
///   one [`PermissionKind`], and [`check_permission`] refuses to run a
///   tool whose required permission is not `Granted`.
/// - Because: `polarize-macos`'s native calls fail in ways that are easy
///   to misdiagnose from the raw OS error alone (a denied AX permission
///   and a genuinely missing UI element can both surface as "element not
///   found"). Checking permission state first turns that into an
///   unambiguous, actionable error before the native call ever runs.
/// - If violated: a caller sees a confusing native failure (or, worse, a
///   `tap`/`keyboard` call that silently no-ops) instead of "grant
///   Accessibility access to run this tool".
pub fn required_permission(tool: ToolKind) -> PermissionKind {
    match tool {
        ToolKind::Screenshot => PermissionKind::ScreenRecording,
        ToolKind::Describe | ToolKind::Tap | ToolKind::Keyboard => PermissionKind::Accessibility,
        ToolKind::PerformAction => PermissionKind::Accessibility,
        // Both `await` tools read the accessibility tree, and
        // `AXObserverCreate` needs the same trust as `AXUIElement` does.
        ToolKind::AwaitUiElement | ToolKind::AwaitScreenIdle => PermissionKind::Accessibility,
        ToolKind::RunAppleScript | ToolKind::ScriptDictionary => PermissionKind::Automation,
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
    let kind = required_permission(tool);
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
    fn screenshot_requires_screen_recording() {
        assert_eq!(
            required_permission(ToolKind::Screenshot),
            PermissionKind::ScreenRecording
        );
    }

    #[test]
    fn describe_tap_and_keyboard_require_accessibility() {
        assert_eq!(
            required_permission(ToolKind::Describe),
            PermissionKind::Accessibility
        );
        assert_eq!(
            required_permission(ToolKind::Tap),
            PermissionKind::Accessibility
        );
        assert_eq!(
            required_permission(ToolKind::Keyboard),
            PermissionKind::Accessibility
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
            PermissionKind::Accessibility
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
            PermissionKind::Accessibility
        );
        assert_eq!(
            required_permission(ToolKind::AwaitScreenIdle),
            PermissionKind::Accessibility
        );
    }

    #[test]
    fn applescript_tools_require_automation() {
        assert_eq!(
            required_permission(ToolKind::RunAppleScript),
            PermissionKind::Automation
        );
        assert_eq!(
            required_permission(ToolKind::ScriptDictionary),
            PermissionKind::Automation
        );
    }

    #[test]
    fn automation_permission_displays_its_settings_name() {
        assert_eq!(PermissionKind::Automation.to_string(), "Automation");
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
}
