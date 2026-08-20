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

use std::ffi::c_void;
use std::ptr;
use std::ptr::NonNull;

use objc2_core_foundation::{CFRange, CFRetained, CFType};
use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::schema::AppIdentifier;
use polarize_core::set_value::{AttributeValue, AttributeWrite};
use polarize_core::traits::ValueSetter;

use crate::ax_ffi::{self, AXValueType, AxElement};
use crate::window::resolve_running_app;

/// `kAXValueCFRangeType` from `AXValue.h`. `AXSelectedTextRange` takes
/// an `AXValue` of this type, and of no other.
///
/// [`crate::ax_ffi`] declares only the point and the size types, because
/// only geometry needs them. A range is the third type this crate
/// writes, so it is declared here, next to its one caller.
const AX_VALUE_TYPE_CF_RANGE: AXValueType = 4;

// `AXValueCreate` boxes a `CFRange` for `AXSelectedTextRange`.
//
// `crate::ax_ffi` declares the same symbol for its own geometric
// setters, and it keeps that declaration private. The signature here
// matches that one exactly, and both name the same function of the same
// framework.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXValueCreate(the_type: AXValueType, value_ptr: *const c_void) -> *const CFType;
}

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
        let element = walk_path(AxElement::for_application(pid), path)?;

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
                set_range_attribute(&element, &write.attribute, *location, *length)
            }
        };
        result.map_err(PolarizeError::Platform)
    }
}

/// Writes an `AXValue`-wrapped `CFRange`, e.g. `AXSelectedTextRange`.
///
/// `AXValueCreate` copies out of `range`, so the borrow does not need to
/// outlive the call. This mirrors `ax_ffi`'s own `ax_value` helper,
/// which that module keeps private for its geometric setters.
fn set_range_attribute(
    element: &AxElement,
    attribute: &str,
    location: usize,
    length: usize,
) -> Result<(), String> {
    let range = CFRange {
        location: isize::try_from(location)
            .map_err(|_| format!("selection location {location} does not fit a CFIndex"))?,
        length: isize::try_from(length)
            .map_err(|_| format!("selection length {length} does not fit a CFIndex"))?,
    };
    let raw = unsafe { AXValueCreate(AX_VALUE_TYPE_CF_RANGE, ptr::from_ref(&range).cast()) };
    let raw = NonNull::new(raw.cast_mut())
        .ok_or_else(|| "AXValueCreate returned null for kAXValueCFRangeType".to_string())?;
    let boxed: CFRetained<CFType> = unsafe { CFRetained::from_raw(raw) };
    element.set_attribute(attribute, &boxed)
}

/// Walks `path` down from `root`, one `AXChildren` index at a time.
///
/// This mirrors `polarize_core::selector::node_at_path`, which walks the
/// same indices over the in-memory tree. See PINV-18.
///
/// An index that no longer names a child is an error, not a silent stop.
/// The tree changed between the two walks, so writing to the parent
/// element instead would fill in something the caller never named.
///
/// `crate::action` holds its own copy of this walk, because it keeps the
/// copy private. The two must stay identical: both are the platform half
/// of PINV-18.
fn walk_path(root: AxElement, path: &[usize]) -> Result<AxElement, PolarizeError> {
    let mut element = root;
    for (depth, &index) in path.iter().enumerate() {
        let children = element.children();
        let count = children.len();
        element = children.into_iter().nth(index).ok_or_else(|| {
            PolarizeError::Platform(format!(
                "element path {path:?} does not resolve: the element at depth {depth} \
                 has {count} child element(s), so index {index} is out of range. \
                 The app's interface probably changed after `describe` ran; \
                 call `describe` again."
            ))
        })?;
    }
    Ok(element)
}
