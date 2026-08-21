//! [`ValueSetter`] over `AXUIElementSetAttributeValue` — see
//! [`crate::ax_ffi`] for why that means hand-written FFI rather than
//! `objc2-accessibility`.
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note. No `set_value` call has run against a real app in
//! this environment. A human on a real macOS session with Accessibility
//! permission granted must confirm three things. First, a text write
//! reaches a real text field. Second, a number write moves a real
//! slider. Third, a range write moves the caret of a real text view.
//!
//! This module makes no decisions. [`polarize_core::set_value`] picks
//! the element, picks the attribute, and builds the typed value. This
//! module walks the path, asks the live element, and performs the
//! write. See PINV-26.
//!
//! ## The write can succeed and still change nothing a user would see
//!
//! `AXUIElementSetAttributeValue` returns `kAXErrorSuccess` when the app
//! accepts the value. A `WKWebView`, an Electron app, or a React app
//! usually accepts it into the DOM and fires no `input` event. The
//! app's own JavaScript never runs, so a controlled component snaps
//! back. `polarize` cannot see that, and it reports success. See
//! PINV-27, and the house rule in `polarize_core::set_value`.

use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::schema::AppIdentifier;
use polarize_core::set_value::{AttributeValue, AttributeWrite};
use polarize_core::traits::ValueSetter;

use crate::ax_ffi::{self, AxElement};
use crate::window::resolve_running_app;

/// `ValueSetter` implementation over `AXUIElementSetAttributeValue`.
#[derive(Debug, Default)]
pub struct MacValueSetter;

impl ValueSetter for MacValueSetter {
    fn set_value_at_path(
        &self,
        app: Option<&AppIdentifier>,
        path: &[usize],
        write: &AttributeWrite,
    ) -> Result<(), PolarizeError> {
        // `AXIsProcessTrusted` collapses "never asked" and "explicitly
        // denied" into the same `false` — `NotDetermined` is the more
        // conservative of the two to report when we cannot distinguish
        // them (it does not claim the user made an explicit choice).
        // See PINV-10 (preflight before any further native call) and
        // PINV-11 (never falsely report `Denied`) in docs/INVARIANTS.md.
        if !unsafe { ax_ffi::AXIsProcessTrusted() } {
            return Err(PolarizeError::Permission(PermissionError::NotGranted {
                kind: PermissionKind::Accessibility,
                state: PermissionState::NotDetermined,
            }));
        }
        crate::session::ensure_session_usable()?;

        let running = resolve_running_app(app)?;
        let pid = running.processIdentifier();
        let element = ax_ffi::walk_path(AxElement::for_application(pid), path)
            .map_err(PolarizeError::Platform)?;

        // The live element is the only authority on settability. Core
        // already refused the writes the tree rules out (PINV-26). This
        // asks the app itself, which knows about a read-only field, a
        // locked document, and a secure text field.
        if !element.is_attribute_settable(&write.attribute) {
            return Err(PolarizeError::Platform(format!(
                "AXUIElementIsAttributeSettable reports {:?} is not settable on the element at \
                 path {path:?}. The app publishes the attribute as read-only. Try `perform_action` \
                 with AXPress, or `keyboard`, instead.",
                write.attribute
            )));
        }

        let result = match &write.value {
            AttributeValue::Text(text) => element.set_string_attribute(&write.attribute, text),
            AttributeValue::Number(number) => {
                element.set_number_attribute(&write.attribute, *number)
            }
            AttributeValue::Range { location, length } => {
                // A `CFRange` counts in `CFIndex`, which is signed. A
                // selection past `isize::MAX` cannot be expressed, and
                // saying so beats writing a wrapped negative index.
                match (isize::try_from(*location), isize::try_from(*length)) {
                    (Ok(location), Ok(length)) => {
                        element.set_range_attribute(&write.attribute, location, length)
                    }
                    _ => Err(format!(
                        "the selection range {location}..+{length} does not fit a CFIndex"
                    )),
                }
            }
        };
        result.map_err(PolarizeError::Platform)
    }
}
