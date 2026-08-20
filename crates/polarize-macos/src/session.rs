//! The login-session preflight, over `CGSessionCopyCurrentDictionary`.
//!
//! `objc2-core-graphics` already binds `CGSessionCopyCurrentDictionary`
//! (`objc2_core_graphics::CGSessionCopyCurrentDictionary`), so this module
//! uses that binding instead of a hand-written `extern "C"` declaration.
//! It does **not** bind the two dictionary keys this module reads, so the
//! keys are literal strings. `ax_ffi.rs` explains the same choice for
//! `AXUIElement` attribute names: the values are long-stable public API,
//! and a literal cannot link against a wrong extern symbol.
//!
//! Watch the two key names. They are not symmetric. The console key is
//! `"kCGSSessionOnConsoleKey"`, with the `k` prefix and the double `S`.
//! The lock key is `"CGSSessionScreenIsLocked"`, with no `k` prefix. A
//! wrong key reads as an absent key, which fails open and reports a
//! usable session (PINV-24). Nothing warns you.
//!
//! The decision this feeds is pure, and lives in
//! [`polarize_core::session`], where real unit tests cover it. This module
//! only reads the two flags. That read has no automated test coverage
//! anywhere; see the crate-level "what is and is not verified" note.

use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::CGSessionCopyCurrentDictionary;
use polarize_core::error::PolarizeError;
use polarize_core::session as core_session;
use polarize_core::session::{SessionInspector, SessionState};
use std::ffi::c_void;

/// Whether this login session owns the console. `false` means Fast User
/// Switching gave the console to another session.
const ON_CONSOLE_KEY: &str = "kCGSSessionOnConsoleKey";

/// Whether the screen is locked. macOS adds this key only while the
/// screen is locked, so an absent key means "unlocked".
const SCREEN_IS_LOCKED_KEY: &str = "CGSSessionScreenIsLocked";

/// [`SessionInspector`] over `CGSessionCopyCurrentDictionary`.
#[derive(Debug, Default)]
pub struct MacSessionInspector;

impl SessionInspector for MacSessionInspector {
    fn session_state(&self) -> SessionState {
        let Some(dictionary) = CGSessionCopyCurrentDictionary() else {
            // A process outside a GUI login session gets no dictionary
            // at all. Report the usable default and let the native call
            // report its own, more specific failure (PINV-24).
            return SessionState::default();
        };
        SessionState::from_flags(
            bool_for_key(&dictionary, ON_CONSOLE_KEY),
            bool_for_key(&dictionary, SCREEN_IS_LOCKED_KEY),
        )
    }
}

/// Reads one `CFBoolean` value out of the session dictionary.
///
/// Returns `None` when the key is absent, or when its value is not a
/// `CFBoolean`. [`SessionState::from_flags`] then applies the fail-open
/// rule (PINV-24).
fn bool_for_key(dictionary: &CFDictionary, key: &str) -> Option<bool> {
    let key = CFString::from_str(key);
    let key_ptr: *const c_void = CFRetained::as_ptr(&key).as_ptr().cast_const().cast();
    // `CFDictionaryGetValue` hands back a borrowed (+0) value that lives
    // as long as `dictionary` does, so no retain is needed here.
    let value: *const c_void = unsafe { dictionary.value(key_ptr) };
    let value: &CFType = unsafe { value.cast::<CFType>().as_ref() }?;
    value
        .downcast_ref::<objc2_core_foundation::CFBoolean>()
        .map(objc2_core_foundation::CFBoolean::value)
}

/// The one call every tool preflights the login session with.
///
/// Reads the real session state, then applies
/// [`polarize_core::session::check_session`] to it (PINV-23). Every tool
/// in this crate calls this line right after its TCC permission check:
///
/// ```ignore
/// crate::session::ensure_session_usable()?;
/// ```
pub fn ensure_session_usable() -> Result<(), PolarizeError> {
    core_session::ensure_session_usable(&MacSessionInspector)
}
