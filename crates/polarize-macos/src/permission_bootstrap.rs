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
use polarize_core::script::AutomationCheck;

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

/// Shows the system Automation consent dialog for one target app (if
/// not already determined). Returns the resulting permission state.
///
/// Unlike [`request_accessibility`] and [`request_screen_recording`],
/// Automation is granted per (this process, target app) pair — see
/// PINV-44. So a caller names every target it commonly scripts. It is
/// not granted once for the whole binary.
pub fn request_automation(target_app_name: &str) -> AutomationCheck {
    crate::applescript::request_automation(target_app_name)
}
