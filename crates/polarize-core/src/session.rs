//! Login-session state, and the pure rule that gates a tool call on it.
//!
//! macOS reports two facts about the current login session that every
//! `polarize` tool depends on: whether the screen is locked, and whether
//! this session owns the console. `polarize-core` never reads those
//! facts. It only models them ([`SessionState`]), decides what they mean
//! for a tool call ([`check_session`]), and names the trait
//! `polarize-macos` implements to read them ([`SessionInspector`]).
//!
//! [`SessionInspector`] stays here, next to the decision it feeds, and
//! not in [`crate::traits`]. The traits module holds the tool surface the
//! MCP server itself drives. This check runs inside `polarize-macos`,
//! before each of those tools, and the server never calls it directly.

use crate::error::PolarizeError;

/// The two login-session facts a tool call depends on.
///
/// `on_console` is `false` when Fast User Switching gives the console to
/// another login session. `screen_locked` is `true` when the login window
/// covers the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionState {
    /// Whether this login session owns the console.
    pub on_console: bool,
    /// Whether the screen is locked.
    pub screen_locked: bool,
}

impl Default for SessionState {
    /// The usable session: on the console, and unlocked. This is also the
    /// state [`SessionState::from_flags`] falls back to — see PINV-24.
    fn default() -> Self {
        Self {
            on_console: true,
            screen_locked: false,
        }
    }
}

impl SessionState {
    /// Builds a state from two optional flags, one per session-dictionary
    /// key. `None` means the platform did not report that key.
    ///
    /// An absent flag reads as the usable value: on the console, and
    /// unlocked. This rule fails open on purpose — see PINV-24 in
    /// `docs/INVARIANTS.md`.
    pub fn from_flags(on_console: Option<bool>, screen_locked: Option<bool>) -> Self {
        let fallback = Self::default();
        Self {
            on_console: on_console.unwrap_or(fallback.on_console),
            screen_locked: screen_locked.unwrap_or(fallback.screen_locked),
        }
    }

    /// Whether a tool call may run in this state.
    pub fn is_usable(self) -> bool {
        self.on_console && !self.screen_locked
    }
}

/// # PINV-23: a tool call preflights the login session, and reports the console first
///
/// - Always: [`check_session`] refuses a tool call when the session does
///   not own the console, or when the screen is locked. It reports
///   [`PolarizeError::SessionNotOnConsole`] first when both facts hold,
///   and [`PolarizeError::ScreenLocked`] only when this session still
///   owns the console.
/// - Because: both states break the native calls silently.
///   `ScreenCaptureKit` returns black or lock-window pixels, the AX tree
///   describes the login window, and a posted `CGEvent` reaches nobody.
///   The two states also need different repairs, and Fast User Switching
///   raises both flags at once: it locks the session it switches away
///   from. An unlock only repairs a session that still owns the console.
///   So the console fact is the one that tells a caller what to do.
/// - If violated: a caller gets a black screenshot, a lock-screen AX
///   tree, or a click that lands nowhere, with no error to explain it.
///   A caller who reads "screen is locked" during Fast User Switching
///   unlocks the Mac, sees nothing improve, and has no next step.
pub fn check_session(state: SessionState) -> Result<(), PolarizeError> {
    // The console fact comes first on purpose. See PINV-23 above.
    if !state.on_console {
        return Err(PolarizeError::SessionNotOnConsole);
    }
    if state.screen_locked {
        return Err(PolarizeError::ScreenLocked);
    }
    Ok(())
}

/// Reads the real login-session state. `polarize-macos` implements this
/// over `CGSessionCopyCurrentDictionary`.
pub trait SessionInspector {
    /// The current login-session state. Reads never fail: an unavailable
    /// dictionary or an absent key degrades to the usable default, per
    /// PINV-24.
    fn session_state(&self) -> SessionState;
}

/// Reads the session state through `inspector`, then applies
/// [`check_session`] to it. This is the one call a tool preflights with.
pub fn ensure_session_usable(inspector: &impl SessionInspector) -> Result<(), PolarizeError> {
    check_session(inspector.session_state())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake inspector that hands back a fixed state.
    struct FakeInspector(SessionState);

    impl SessionInspector for FakeInspector {
        fn session_state(&self) -> SessionState {
            self.0
        }
    }

    fn state(on_console: bool, screen_locked: bool) -> SessionState {
        SessionState {
            on_console,
            screen_locked,
        }
    }

    #[test]
    fn default_state_is_on_console_and_unlocked() {
        assert_eq!(SessionState::default(), state(true, false));
        assert!(SessionState::default().is_usable());
    }

    #[test]
    fn check_session_passes_on_an_unlocked_console_session() {
        assert!(check_session(state(true, false)).is_ok());
    }

    #[test]
    fn check_session_reports_screen_locked_when_only_the_screen_is_locked() {
        let err = check_session(state(true, true)).unwrap_err();
        assert!(matches!(err, PolarizeError::ScreenLocked));
    }

    #[test]
    fn check_session_reports_not_on_console_when_only_the_console_is_gone() {
        let err = check_session(state(false, false)).unwrap_err();
        assert!(matches!(err, PolarizeError::SessionNotOnConsole));
    }

    #[test]
    fn check_session_prefers_the_console_error_when_both_facts_hold() {
        // Fast User Switching locks the session it switches away from,
        // so both flags hold at once. PINV-23 reports the console fact,
        // because an unlock does not repair that session.
        let err = check_session(state(false, true)).unwrap_err();
        assert!(matches!(err, PolarizeError::SessionNotOnConsole));
    }

    #[test]
    fn is_usable_matches_check_session_on_every_state() {
        for on_console in [true, false] {
            for screen_locked in [true, false] {
                let state = state(on_console, screen_locked);
                assert_eq!(state.is_usable(), check_session(state).is_ok());
            }
        }
    }

    #[test]
    fn from_flags_keeps_both_reported_values() {
        assert_eq!(
            SessionState::from_flags(Some(false), Some(true)),
            state(false, true)
        );
        assert_eq!(
            SessionState::from_flags(Some(true), Some(false)),
            state(true, false)
        );
    }

    #[test]
    fn from_flags_treats_an_absent_lock_flag_as_unlocked() {
        // macOS omits `CGSSessionScreenIsLocked` while the screen is
        // unlocked. An absent key must not block every tool call.
        assert_eq!(
            SessionState::from_flags(Some(true), None),
            state(true, false)
        );
    }

    #[test]
    fn from_flags_treats_an_absent_console_flag_as_on_console() {
        assert_eq!(
            SessionState::from_flags(None, Some(false)),
            state(true, false)
        );
    }

    #[test]
    fn from_flags_with_no_flags_is_the_usable_default() {
        assert_eq!(
            SessionState::from_flags(None, None),
            SessionState::default()
        );
        assert!(check_session(SessionState::from_flags(None, None)).is_ok());
    }

    #[test]
    fn ensure_session_usable_passes_on_a_usable_session() {
        let inspector = FakeInspector(state(true, false));
        assert!(ensure_session_usable(&inspector).is_ok());
    }

    #[test]
    fn ensure_session_usable_reports_the_state_the_inspector_reads() {
        let locked = FakeInspector(state(true, true));
        assert!(matches!(
            ensure_session_usable(&locked).unwrap_err(),
            PolarizeError::ScreenLocked
        ));

        let switched_away = FakeInspector(state(false, true));
        assert!(matches!(
            ensure_session_usable(&switched_away).unwrap_err(),
            PolarizeError::SessionNotOnConsole
        ));
    }
}
