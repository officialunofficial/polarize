//! Crate-wide error types.
//!
//! `polarize-macos` and `apps/polarize` both report failures through
//! [`PolarizeError`]; the pure sub-errors it wraps (coordinate conversion,
//! permission checks) live next to the logic that produces them and are
//! folded in here via `#[from]`.

use std::fmt;

use crate::action::ActionError;
use crate::coords::CoordError;
use crate::permission::PermissionError;
use crate::selector::SelectorError;
use crate::wait::WaitError;

/// Which axis a coordinate error refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CoordAxis {
    X,
    Y,
}

impl fmt::Display for CoordAxis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoordAxis::X => write!(f, "x"),
            CoordAxis::Y => write!(f, "y"),
        }
    }
}

/// The top-level error type for all of `polarize`.
///
/// `polarize-core`'s own logic only ever produces the [`Self::Coord`],
/// [`Self::Permission`], [`Self::Selector`], [`Self::Action`], and
/// [`Self::Wait`] variants. The remaining variants exist for
/// `polarize-macos` and `apps/polarize` to report failures from real
/// native-API calls (window lookup, screen capture, AX tree walks, event
/// posting) that `polarize-core` cannot itself experience or test.
#[derive(Debug, thiserror::Error)]
pub enum PolarizeError {
    /// A fraction/pixel coordinate conversion was rejected. See
    /// [`crate::coords::CoordError`] (PINV-1).
    #[error(transparent)]
    Coord(#[from] CoordError),

    /// A tool call needs a permission that is not `Granted`. See
    /// [`crate::permission`] (PINV-2).
    #[error(transparent)]
    Permission(#[from] PermissionError),

    /// An element selector named no criterion, matched nothing, or
    /// matched fewer elements than its `index` needs. See
    /// [`crate::selector`] (PINV-15).
    #[error(transparent)]
    Selector(#[from] SelectorError),

    /// A `perform_action` call refused to act on the element it
    /// resolved. See [`crate::action`] (PINV-17).
    #[error(transparent)]
    Action(#[from] ActionError),

    /// An `await_ui_element` or `await_screen_idle` call reached its
    /// deadline. See [`crate::wait`] (PINV-19).
    #[error(transparent)]
    Wait(#[from] WaitError),

    /// The requested app (by bundle id or name) is not running.
    #[error("app not found: {0}")]
    AppNotFound(String),

    /// The requested window was not found on the requested app.
    #[error("window not found: {0}")]
    WindowNotFound(String),

    /// A real native-API call (`ScreenCaptureKit`, `AXUIElement`,
    /// `CGEvent`, AppKit) failed. `polarize-macos` is the only crate that
    /// produces this variant, since only it makes those calls.
    #[error("platform error: {0}")]
    Platform(String),

    /// The login session reports a locked screen. Screen capture returns
    /// black pixels, and the AX tree describes the lock screen, not the
    /// target app. See [`crate::session`] (PINV-23).
    #[error("screen is locked; unlock this Mac and call the tool again")]
    ScreenLocked,

    /// Another login session holds the console, through Fast User
    /// Switching. Posted `CGEvent`s and `ScreenCaptureKit` frames do not
    /// reach the user who is on screen. See [`crate::session`] (PINV-23).
    #[error(
        "login session is not on the console; another user holds the display through Fast User Switching"
    )]
    SessionNotOnConsole,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_axis_display_matches_json_field_name() {
        assert_eq!(CoordAxis::X.to_string(), "x");
        assert_eq!(CoordAxis::Y.to_string(), "y");
    }

    #[test]
    fn coord_error_converts_into_polarize_error_via_from() {
        let coord_err = CoordError::NonPositiveSize {
            width: 0.0,
            height: 0.0,
        };
        let err: PolarizeError = coord_err.into();
        assert!(matches!(err, PolarizeError::Coord(_)));
    }

    #[test]
    fn permission_error_converts_into_polarize_error_via_from() {
        use crate::permission::{PermissionKind, PermissionState};

        let perm_err = PermissionError::NotGranted {
            kind: PermissionKind::ScreenRecording,
            state: PermissionState::Denied,
        };
        let err: PolarizeError = perm_err.into();
        assert!(matches!(err, PolarizeError::Permission(_)));
    }

    #[test]
    fn selector_error_converts_into_polarize_error_via_from() {
        let selector_err = SelectorError::NoMatch {
            selector: "role=\"AXButton\"".to_string(),
        };
        let err: PolarizeError = selector_err.into();
        assert!(matches!(err, PolarizeError::Selector(_)));
        assert!(err.to_string().contains("AXButton"));
    }

    #[test]
    fn platform_error_display_includes_message() {
        let err = PolarizeError::Platform("CGWindowListCreateImage returned null".into());
        assert_eq!(
            err.to_string(),
            "platform error: CGWindowListCreateImage returned null"
        );
    }

    #[test]
    fn screen_locked_display_names_the_lock() {
        let err = PolarizeError::ScreenLocked;
        assert_eq!(
            err.to_string(),
            "screen is locked; unlock this Mac and call the tool again"
        );
    }

    #[test]
    fn session_not_on_console_display_names_fast_user_switching() {
        let err = PolarizeError::SessionNotOnConsole;
        assert_eq!(
            err.to_string(),
            "login session is not on the console; another user holds the display through Fast User Switching"
        );
    }

    #[test]
    fn app_not_found_display_includes_identifier() {
        let err = PolarizeError::AppNotFound("com.example.NoSuchApp".into());
        assert_eq!(err.to_string(), "app not found: com.example.NoSuchApp");
    }
}
