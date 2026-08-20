//! Raw FFI bindings to the classic `AXUIElement`/`AXValue` C API (part of
//! the `ApplicationServices` → `HIServices` framework stack).
//!
//! `objc2-accessibility` — the crate the locked design names for this —
//! does **not** bind this API. It binds Apple's newer, unrelated
//! "Accessibility" framework (assistive-technology *content-authoring*
//! types like `AXCustomContent`/`AXChart`/`AXBrailleMap`, for an app to
//! describe its own accessible content), not the classic `AXUIElementRef`
//! *inspection* API every screen reader and UI-automation tool actually
//! walks. As of writing there is no `objc2-application-services` crate in
//! the `objc2` umbrella project either. This module fills that gap with a
//! small, hand-written `extern "C"` surface — just the handful of functions
//! `polarize` needs — built on `objc2-core-foundation`'s `CFRetained` for
//! memory management, so it stays consistent with the rest of the crate's
//! CF-based reference counting rather than introducing a second scheme.
//!
//! Attribute *names* (`"AXRole"`, `"AXChildren"`, …) are passed as literal
//! strings rather than linked against the framework's `kAX*Attribute`
//! extern symbols: their string values are long-stable, Apple-documented
//! public API, and a literal avoids any risk of mis-declaring a wrong
//! extern symbol (which would fail to link, or worse, link against the
//! wrong symbol of the same name in another framework).
//!
//! Nothing here has been exercised against a real accessibility session —
//! see the crate-level docs and `docs/INVARIANTS.md`.

use objc2_core_foundation::{
    CFArray, CFBoolean, CFNumber, CFRetained, CFString, CFType, CGPoint, CGSize,
};
use std::ffi::c_void;
use std::ptr;
use std::ptr::NonNull;

/// Opaque handle matching the C `AXUIElementRef` typedef.
#[repr(C)]
pub struct OpaqueAXUIElement {
    _private: [u8; 0],
}

/// `AXUIElementRef` is `CFTypeRef`-compatible — `CFRetain`/`CFRelease`/
/// `CFGetTypeID` all work on it — but it is not one of
/// `objc2-core-foundation`'s own known concrete types, so it is modeled as
/// its own raw pointer type rather than as a `CFType` subtype.
pub type AXUIElementRef = *const OpaqueAXUIElement;

/// `typedef int32_t AXError;` (`AXError.h`). `0` is `kAXErrorSuccess`.
pub type AXError = i32;
pub const AX_ERROR_SUCCESS: AXError = 0;

/// The `AXError.h` codes [`ax_error_name`] can name. `polarize` reads
/// none of these as a value; they exist to turn a bare number in an
/// error message into a term a reader can search for.
const AX_ERROR_FAILURE: AXError = -25200;
const AX_ERROR_ILLEGAL_ARGUMENT: AXError = -25201;
const AX_ERROR_INVALID_UI_ELEMENT: AXError = -25202;
const AX_ERROR_CANNOT_COMPLETE: AXError = -25204;
const AX_ERROR_ACTION_UNSUPPORTED: AXError = -25206;
const AX_ERROR_NOT_IMPLEMENTED: AXError = -25208;
const AX_ERROR_API_DISABLED: AXError = -25211;

/// `typedef uint32_t AXValueType;` (`AXValue.h`). Only the two variants
/// `polarize` reads (position/size) are declared.
pub type AXValueType = u32;
pub const AX_VALUE_TYPE_CG_POINT: AXValueType = 1;
pub const AX_VALUE_TYPE_CG_SIZE: AXValueType = 2;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    pub fn AXIsProcessTrusted() -> bool;
    pub fn AXIsProcessTrustedWithOptions(options: *const CFType) -> bool;
    /// The options-dictionary key that, when set to `true`, makes
    /// [`AXIsProcessTrustedWithOptions`] show the system Accessibility
    /// consent alert (and register this process in the Accessibility
    /// list) if it is not already trusted, rather than silently
    /// reporting `false`. Used only by `request-permissions` (see
    /// `apps/polarize/src/main.rs`) — every real tool call still uses
    /// the non-prompting [`AXIsProcessTrusted`] (PINV-10/PINV-11).
    pub static kAXTrustedCheckOptionPrompt: *const CFString;
    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    #[allow(dead_code)] // reserved for a future "describe whatever's under the cursor" tool
    pub fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: *const CFString,
        value: *mut *const CFType,
    ) -> AXError;
    fn AXUIElementCopyActionNames(element: AXUIElementRef, names: *mut *const CFArray) -> AXError;
    fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: *const CFString,
        settable: *mut bool,
    ) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: *const CFString) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: *const CFString,
        value: *const CFType,
    ) -> AXError;
    /// The element at a point in the **global display** coordinate
    /// space, the same space `crate::input` posts clicks into (PINV-4).
    /// `application` must be an application element, or the system-wide
    /// element.
    fn AXUIElementCopyElementAtPosition(
        application: AXUIElementRef,
        x: f32,
        y: f32,
        element: *mut AXUIElementRef,
    ) -> AXError;
    fn AXValueGetType(value: *const c_void) -> AXValueType;
    fn AXValueGetValue(value: *const c_void, the_type: AXValueType, out: *mut c_void) -> bool;
    /// Wraps a `CGPoint`/`CGSize` in the `AXValue` box every geometric
    /// AX attribute takes. Returns `NULL` on failure.
    fn AXValueCreate(the_type: AXValueType, value_ptr: *const c_void) -> *const CFType;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// Calls `AXIsProcessTrustedWithOptions` with the prompt option set,
/// which shows the system Accessibility consent alert (and registers
/// this exact binary in the Accessibility list as a real, functional
/// grant) if it is not already trusted.
///
/// Only `apps/polarize`'s `--request-permissions` bootstrap flag calls
/// this. It exists because manually adding a raw (non-`.app`) binary
/// through System Settings' Accessibility "+" picker does not reliably
/// produce a working grant. The entry can show up toggled on and still
/// leave `AXIsProcessTrusted` returning `false`. Prompting through the
/// API is what macOS itself uses to register a real one. Every actual
/// tool call still goes through the non-prompting `AXIsProcessTrusted`
/// (PINV-10/PINV-11). This function is a one-time setup helper, not
/// part of any tool's request path.
pub fn request_accessibility_permission_with_prompt() -> bool {
    let prompt_key =
        unsafe { kAXTrustedCheckOptionPrompt.as_ref() }.expect("ApplicationServices constant");
    let options =
        objc2_core_foundation::CFDictionary::from_slices(&[prompt_key], &[CFBoolean::new(true)]);
    let ptr = CFRetained::as_ptr(&options)
        .as_ptr()
        .cast_const()
        .cast::<CFType>();
    unsafe { AXIsProcessTrustedWithOptions(ptr) }
}

/// An owned, retained `AXUIElementRef`. Released on [`Drop`].
pub struct AxElement(AXUIElementRef);

impl Drop for AxElement {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0.cast()) };
        }
    }
}

impl AxElement {
    /// The accessibility element representing `pid`'s whole application.
    ///
    /// Always succeeds per `AXUIElementCreateApplication`'s own contract —
    /// even a `pid` that does not exist yields a (dead) element; attribute
    /// reads on it then fail with an [`AXError`] rather than this
    /// constructor failing.
    pub fn for_application(pid: i32) -> Self {
        Self(unsafe { AXUIElementCreateApplication(pid) })
    }

    /// Wraps an already-retained (+1) `AXUIElementRef`, e.g. one read out
    /// of a `kAXChildrenAttribute` array — see [`Self::children`].
    ///
    /// # Safety
    /// `raw` must be non-null and already retained; ownership transfers to
    /// the returned [`AxElement`], which releases it on `Drop`.
    unsafe fn from_retained(raw: AXUIElementRef) -> Self {
        Self(raw)
    }

    fn copy_attribute(&self, attribute: &str) -> Option<CFRetained<CFType>> {
        let attr = CFString::from_str(attribute);
        let mut value: *const CFType = ptr::null();
        let err =
            unsafe { AXUIElementCopyAttributeValue(self.0, &*attr as *const CFString, &mut value) };
        if err != AX_ERROR_SUCCESS {
            return None;
        }
        NonNull::new(value.cast_mut()).map(|p| unsafe { CFRetained::from_raw(p) })
    }

    /// Reads a `CFString`-typed attribute (e.g. `AXRole`, `AXTitle`).
    pub fn string_attribute(&self, attribute: &str) -> Option<String> {
        let value = self.copy_attribute(attribute)?;
        value.downcast_ref::<CFString>().map(ToString::to_string)
    }

    /// Reads a `CFBoolean`-typed attribute (e.g. `AXEnabled`).
    pub fn bool_attribute(&self, attribute: &str) -> Option<bool> {
        let value = self.copy_attribute(attribute)?;
        value.downcast_ref::<CFBoolean>().map(CFBoolean::value)
    }

    /// Reads an `AXValue`-wrapped `CGPoint` attribute (e.g. `AXPosition`).
    pub fn point_attribute(&self, attribute: &str) -> Option<CGPoint> {
        let value = self.copy_attribute(attribute)?;
        let raw = value_as_ax_value_ptr(&value);
        if unsafe { AXValueGetType(raw) } != AX_VALUE_TYPE_CG_POINT {
            return None;
        }
        let mut point = CGPoint::ZERO;
        let ok = unsafe { AXValueGetValue(raw, AX_VALUE_TYPE_CG_POINT, (&raw mut point).cast()) };
        ok.then_some(point)
    }

    /// Reads an `AXValue`-wrapped `CGSize` attribute (e.g. `AXSize`).
    pub fn size_attribute(&self, attribute: &str) -> Option<CGSize> {
        let value = self.copy_attribute(attribute)?;
        let raw = value_as_ax_value_ptr(&value);
        if unsafe { AXValueGetType(raw) } != AX_VALUE_TYPE_CG_SIZE {
            return None;
        }
        let mut size = CGSize::ZERO;
        let ok = unsafe { AXValueGetValue(raw, AX_VALUE_TYPE_CG_SIZE, (&raw mut size).cast()) };
        ok.then_some(size)
    }

    /// The element's `AXChildren`, or an empty `Vec` if it has none or the
    /// attribute could not be read.
    pub fn children(&self) -> Vec<AxElement> {
        let Some(value) = self.copy_attribute("AXChildren") else {
            return Vec::new();
        };
        let Some(array) = value.downcast_ref::<CFArray>() else {
            return Vec::new();
        };
        (0..array.count())
            .filter_map(|i| {
                let borrowed = unsafe { array.value_at_index(i) };
                if borrowed.is_null() {
                    return None;
                }
                let retained = unsafe { CFRetain(borrowed) };
                Some(unsafe { AxElement::from_retained(retained.cast()) })
            })
            .collect()
    }

    /// The element's AX action names, e.g. `["AXPress", "AXShowMenu"]`.
    ///
    /// An error is reported as an empty list rather than propagated. A
    /// caller reads this as "no action available", which is the same
    /// conclusion it draws from a real empty list, and neither one should
    /// abort a whole tree walk (PINV-12).
    pub fn action_names(&self) -> Vec<String> {
        let mut names: *const CFArray = ptr::null();
        let err = unsafe { AXUIElementCopyActionNames(self.0, &mut names) };
        if err != AX_ERROR_SUCCESS {
            return Vec::new();
        }
        let Some(names) = NonNull::new(names.cast_mut()) else {
            return Vec::new();
        };
        let names: CFRetained<CFArray> = unsafe { CFRetained::from_raw(names) };
        (0..names.count())
            .filter_map(|i| {
                let borrowed = unsafe { names.value_at_index(i) };
                if borrowed.is_null() {
                    return None;
                }
                // `value_at_index` hands back a borrowed (+0) reference;
                // retain it so `CFRetained::from_raw` owns a real +1, the
                // same handoff `children` performs.
                let retained = unsafe { CFRetain(borrowed) };
                let value: CFRetained<CFType> =
                    unsafe { CFRetained::from_raw(NonNull::new(retained.cast_mut().cast())?) };
                value.downcast_ref::<CFString>().map(ToString::to_string)
            })
            .collect()
    }

    /// Whether `attribute` is settable on this element — used as a
    /// best-effort "can this receive keyboard focus" signal for
    /// `AXFocused`. Errors are treated as "not settable".
    pub fn is_attribute_settable(&self, attribute: &str) -> bool {
        let attr = CFString::from_str(attribute);
        let mut settable = false;
        let err = unsafe {
            AXUIElementIsAttributeSettable(self.0, &*attr as *const CFString, &mut settable)
        };
        err == AX_ERROR_SUCCESS && settable
    }

    /// Performs one AX action, e.g. `"AXPress"`, on this element.
    ///
    /// Unlike [`Self::action_names`], a failure here is **not** degraded
    /// to a default. A caller asked the app to do something, so a
    /// non-success [`AXError`] is the whole result of the call, and it
    /// travels back as a message (see [`ax_error_name`]).
    ///
    /// `AXUIElementPerformAction` is synchronous. It returns when the
    /// app finishes the action, or when the AX timeout expires. The
    /// caller checks the element first, so that a hang stays unlikely —
    /// see PINV-17 in `docs/INVARIANTS.md`.
    pub fn perform_action(&self, action: &str) -> Result<(), String> {
        let name = CFString::from_str(action);
        let err = unsafe { AXUIElementPerformAction(self.0, &*name as *const CFString) };
        if err == AX_ERROR_SUCCESS {
            return Ok(());
        }
        Err(format!(
            "AXUIElementPerformAction({action:?}) failed: {} ({err})",
            ax_error_name(err)
        ))
    }

    /// Reads an `AXUIElement`-typed attribute, e.g. `AXCloseButton`,
    /// `AXFocusedUIElement`, or `AXParent`.
    ///
    /// The value arrives retained (+1), and the returned [`AxElement`]
    /// takes ownership of that reference rather than retaining again.
    pub fn element_attribute(&self, attribute: &str) -> Option<AxElement> {
        let value = self.copy_attribute(attribute)?;
        let raw = CFRetained::into_raw(value).as_ptr().cast_const();
        Some(unsafe { AxElement::from_retained(raw.cast()) })
    }

    /// Reads an `AXUIElement`-array attribute, e.g. `AXWindows`.
    pub fn element_array_attribute(&self, attribute: &str) -> Vec<AxElement> {
        let Some(value) = self.copy_attribute(attribute) else {
            return Vec::new();
        };
        let Some(array) = value.downcast_ref::<CFArray>() else {
            return Vec::new();
        };
        (0..array.count())
            .filter_map(|index| {
                let borrowed = unsafe { array.value_at_index(index) };
                if borrowed.is_null() {
                    return None;
                }
                let retained = unsafe { CFRetain(borrowed) };
                Some(unsafe { AxElement::from_retained(retained.cast()) })
            })
            .collect()
    }

    /// Writes one attribute from an already-built Core Foundation value.
    ///
    /// An error carries the `AXError` name as well as its number,
    /// because the number alone is not searchable. The typed setters
    /// below are what callers normally use.
    pub fn set_attribute(&self, attribute: &str, value: &CFType) -> Result<(), String> {
        let attr = CFString::from_str(attribute);
        let err = unsafe {
            AXUIElementSetAttributeValue(self.0, &*attr as *const CFString, value as *const CFType)
        };
        if err == AX_ERROR_SUCCESS {
            return Ok(());
        }
        Err(format!(
            "AXUIElementSetAttributeValue({attribute:?}) failed: {} ({err})",
            ax_error_name(err)
        ))
    }

    /// Writes a `CFString`-typed attribute, e.g. `AXValue` on a text
    /// field.
    pub fn set_string_attribute(&self, attribute: &str, value: &str) -> Result<(), String> {
        let value = CFString::from_str(value);
        self.set_attribute(attribute, &value)
    }

    /// Writes a `CFBoolean`-typed attribute, e.g. `AXMinimized`.
    pub fn set_bool_attribute(&self, attribute: &str, value: bool) -> Result<(), String> {
        self.set_attribute(attribute, CFBoolean::new(value).as_ref())
    }

    /// Writes a `CFNumber`-typed attribute, e.g. `AXValue` on a slider.
    pub fn set_number_attribute(&self, attribute: &str, value: f64) -> Result<(), String> {
        self.set_attribute(attribute, &CFNumber::new_f64(value))
    }

    /// Writes an `AXValue`-wrapped `CGPoint`, e.g. `AXPosition`.
    pub fn set_point_attribute(&self, attribute: &str, value: CGPoint) -> Result<(), String> {
        let boxed = ax_value(AX_VALUE_TYPE_CG_POINT, &value)?;
        self.set_attribute(attribute, &boxed)
    }

    /// Writes an `AXValue`-wrapped `CGSize`, e.g. `AXSize`.
    pub fn set_size_attribute(&self, attribute: &str, value: CGSize) -> Result<(), String> {
        let boxed = ax_value(AX_VALUE_TYPE_CG_SIZE, &value)?;
        self.set_attribute(attribute, &boxed)
    }

    /// The deepest element at `x`/`y`, in the **global display**
    /// coordinate space — the space `crate::input` clicks into
    /// (PINV-4).
    ///
    /// macOS hit-tests the point and returns whatever really sits on
    /// top. An occluded element never comes back, which is what makes
    /// this a usable `tap` preflight.
    pub fn element_at_position(&self, x: f64, y: f64) -> Option<AxElement> {
        let mut raw: AXUIElementRef = ptr::null();
        let err = unsafe { AXUIElementCopyElementAtPosition(self.0, x as f32, y as f32, &mut raw) };
        if err != AX_ERROR_SUCCESS || raw.is_null() {
            return None;
        }
        // The copy already retained it.
        Some(unsafe { AxElement::from_retained(raw) })
    }
}

/// The `AXError.h` name of one error code, or `"kAXErrorUnknown"`.
///
/// The raw number stays in the message next to this name, so an
/// unmapped code is still readable. See [`AxElement::perform_action`].
fn ax_error_name(err: AXError) -> &'static str {
    match err {
        AX_ERROR_SUCCESS => "kAXErrorSuccess",
        AX_ERROR_FAILURE => "kAXErrorFailure",
        AX_ERROR_ILLEGAL_ARGUMENT => "kAXErrorIllegalArgument",
        AX_ERROR_INVALID_UI_ELEMENT => "kAXErrorInvalidUIElement",
        AX_ERROR_CANNOT_COMPLETE => "kAXErrorCannotComplete",
        AX_ERROR_ACTION_UNSUPPORTED => "kAXErrorActionUnsupported",
        AX_ERROR_NOT_IMPLEMENTED => "kAXErrorNotImplemented",
        AX_ERROR_API_DISABLED => "kAXErrorAPIDisabled",
        _ => "kAXErrorUnknown",
    }
}

/// Boxes a geometric value in the `AXValue` every AX setter takes.
///
/// `AXValueCreate` copies out of `value_ptr`, so the borrow does not
/// need to outlive the call.
fn ax_value<T>(value_type: AXValueType, value: &T) -> Result<CFRetained<CFType>, String> {
    let raw = unsafe { AXValueCreate(value_type, ptr::from_ref(value).cast()) };
    let raw = NonNull::new(raw.cast_mut())
        .ok_or_else(|| format!("AXValueCreate returned null for AXValueType {value_type}"))?;
    Ok(unsafe { CFRetained::from_raw(raw) })
}

/// Casts a copied attribute value's CF pointer to the raw pointer
/// `AXValueGetType`/`AXValueGetValue` expect. `AXValueRef` is itself
/// CFType-compatible (it is what `AXUIElementCopyAttributeValue` handed
/// back), so reusing the already-retained `CFType` pointer here — rather
/// than re-copying — is sound.
fn value_as_ax_value_ptr(value: &CFRetained<CFType>) -> *const c_void {
    CFRetained::as_ptr(value).as_ptr().cast_const().cast()
}
