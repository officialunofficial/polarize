//! One-time setup helpers behind `apps/polarize`'s `--request-permissions`
//! flag. No MCP tool call path uses this module — every real
//! `describe`/`tap`/`keyboard`/`screenshot` call still preflights with the
//! non-prompting checks (PINV-10/PINV-11).
//!
//! Manually adding a raw (non-`.app`) binary through System Settings'
//! Accessibility or Screen Recording "+" picker does not reliably produce
//! a working grant: the entry can show up toggled on while
//! `AXIsProcessTrusted`/`CGPreflightScreenCaptureAccess` still report
//! `false`. Calling the OS's own prompting APIs is what actually
//! registers a functional grant.

use crate::ax_ffi;
use objc2_core_graphics::CGRequestScreenCaptureAccess;

/// Shows the system Accessibility consent alert (if not already
/// trusted) and returns whether this binary is trusted afterward.
pub fn request_accessibility() -> bool {
    ax_ffi::request_accessibility_permission_with_prompt()
}

/// Shows the system Screen Recording consent alert (if not already
/// trusted) and returns whether this binary is trusted afterward.
pub fn request_screen_recording() -> bool {
    CGRequestScreenCaptureAccess()
}
