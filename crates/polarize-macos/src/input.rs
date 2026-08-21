//! [`InputSynthesizer`] over `CGEvent` (`objc2-core-graphics`).
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note. In particular: no click or keystroke posted by this
//! module has landed on a real screen in this environment. There is no
//! display here, and no Input Monitoring or Accessibility permission to
//! grant. A human on a real macOS session must confirm a `tap`/`keyboard`
//! call visibly does what it claims. Pure event construction succeeding
//! is not the same as the target app receiving and acting on the event.
//!
//! The pure pieces this module delegates to are unit-tested in
//! [`crate::keymap`]: the modifier-to-flags mapping, the keycode table,
//! and the multi-click event sequence.

use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGMouseButton,
    CGPreflightPostEventAccess,
};
use std::ffi::c_void;

use polarize_core::coords::PixelPoint;
use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::schema::{Modifier, NamedKey, PostPath};
use polarize_core::traits::InputSynthesizer;

use crate::keymap;
use crate::skylight_ffi;

/// `InputSynthesizer` implementation over `CGEvent`.
#[derive(Debug, Default)]
pub struct MacInputSynthesizer;

/// Checks Accessibility permission before any `CGEvent` post. Every
/// [`InputSynthesizer`] method calls this first — see PINV-10/PINV-11 in
/// `docs/INVARIANTS.md`.
///
/// `CGPreflightPostEventAccess` collapses "never asked" and "explicitly
/// denied" into the same `false`, same caveat as `AXIsProcessTrusted` in
/// `accessibility.rs`. `NotDetermined` is the more conservative of the two
/// to report when this method cannot tell them apart.
fn ensure_input_permission() -> Result<(), PolarizeError> {
    if CGPreflightPostEventAccess() {
        crate::session::ensure_session_usable()
    } else {
        Err(PolarizeError::Permission(PermissionError::NotGranted {
            kind: PermissionKind::Accessibility,
            state: PermissionState::NotDetermined,
        }))
    }
}

fn event_source() -> Result<objc2_core_foundation::CFRetained<CGEventSource>, PolarizeError> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .ok_or_else(|| PolarizeError::Platform("CGEventSourceCreate returned null".to_string()))
}

impl InputSynthesizer for MacInputSynthesizer {
    /// Posts the click via `SLEventPostToPid` when `pid` is `Some` and
    /// [`skylight_ffi::symbols`] resolved the symbol. Falls back to the
    /// global `CGEvent` post otherwise. See PINV-46 and PINV-47.
    fn click_at_pixel(
        &self,
        point: PixelPoint,
        click_count: u8,
        pid: Option<i32>,
    ) -> Result<PostPath, PolarizeError> {
        ensure_input_permission()?;
        let source = event_source()?;
        let cg_point = CGPoint {
            x: point.x,
            y: point.y,
        };

        // A pid alone is not enough. The symbol must have resolved on
        // this macOS version too — see PINV-46.
        let pid_post = pid.zip(skylight_ffi::symbols().event_post_to_pid);

        for (event_type, click_state) in keymap::click_event_sequence(click_count) {
            let event =
                CGEvent::new_mouse_event(Some(&source), event_type, cg_point, CGMouseButton::Left)
                    .ok_or_else(|| {
                        PolarizeError::Platform("CGEventCreateMouseEvent returned null".to_string())
                    })?;
            CGEvent::set_integer_value_field(
                Some(&event),
                CGEventField::MouseEventClickState,
                click_state,
            );
            match pid_post {
                Some((pid, post_to_pid)) => {
                    // `SLEventPostToPid` takes the same `CGEventRef` the
                    // global post takes — reuse the already-retained
                    // pointer rather than building a second event.
                    let event_ptr: *const c_void =
                        CFRetained::as_ptr(&event).as_ptr().cast_const().cast();
                    unsafe { post_to_pid(pid, event_ptr) };
                }
                None => CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event)),
            }
        }
        Ok(if pid_post.is_some() {
            PostPath::Pid
        } else {
            PostPath::Global
        })
    }

    fn type_text(&self, text: &str) -> Result<(), PolarizeError> {
        ensure_input_permission()?;
        let source = event_source()?;
        // Virtual key `0` plus an explicit Unicode payload is the standard
        // way to post arbitrary text via `CGEvent` without needing a
        // layout-dependent keycode for every character — the same trick
        // most macOS UI-automation tools use.
        let utf16: Vec<u16> = text.encode_utf16().collect();

        for key_down in [true, false] {
            let event =
                CGEvent::new_keyboard_event(Some(&source), 0, key_down).ok_or_else(|| {
                    PolarizeError::Platform("CGEventCreateKeyboardEvent returned null".to_string())
                })?;
            unsafe {
                CGEvent::keyboard_set_unicode_string(
                    Some(&event),
                    utf16.len() as _,
                    utf16.as_ptr(),
                );
            }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
        Ok(())
    }

    fn press_key(&self, key: NamedKey, modifiers: &[Modifier]) -> Result<(), PolarizeError> {
        ensure_input_permission()?;
        let source = event_source()?;
        let keycode = keymap::named_key_to_keycode(key);
        let flags = keymap::modifiers_to_cgevent_flags(modifiers);

        for key_down in [true, false] {
            let event =
                CGEvent::new_keyboard_event(Some(&source), keycode, key_down).ok_or_else(|| {
                    PolarizeError::Platform("CGEventCreateKeyboardEvent returned null".to_string())
                })?;
            CGEvent::set_flags(Some(&event), flags);
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
        Ok(())
    }
}
