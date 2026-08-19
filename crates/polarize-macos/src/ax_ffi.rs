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

use objc2_core_foundation::{CFArray, CFBoolean, CFRetained, CFString, CFType, CGPoint, CGSize};
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

/// `typedef uint32_t AXValueType;` (`AXValue.h`). Only the two variants
/// `polarize` reads (position/size) are declared.
pub type AXValueType = u32;
pub const AX_VALUE_TYPE_CG_POINT: AXValueType = 1;
pub const AX_VALUE_TYPE_CG_SIZE: AXValueType = 2;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    pub fn AXIsProcessTrusted() -> bool;
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
    fn AXValueGetType(value: *const c_void) -> AXValueType;
    fn AXValueGetValue(value: *const c_void, the_type: AXValueType, out: *mut c_void) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
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

    /// Reads a `CFBoolean`-typed attribute (e.g. `AXFocused`).
    #[allow(dead_code)] // read via is_attribute_settable for AXFocused today; kept for direct boolean attributes
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

    /// Whether the element has at least one AX action it can perform (a
    /// button-like "this can be clicked" signal). Errors are treated as
    /// "no actions" rather than propagated — a best-effort classification,
    /// not a hard fact the caller should branch safety-critical logic on.
    pub fn has_actions(&self) -> bool {
        let mut names: *const CFArray = ptr::null();
        let err = unsafe { AXUIElementCopyActionNames(self.0, &mut names) };
        if err != AX_ERROR_SUCCESS {
            return false;
        }
        let Some(names) = NonNull::new(names.cast_mut()) else {
            return false;
        };
        let names: CFRetained<CFArray> = unsafe { CFRetained::from_raw(names) };
        names.count() > 0
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
}

/// Casts a copied attribute value's CF pointer to the raw pointer
/// `AXValueGetType`/`AXValueGetValue` expect. `AXValueRef` is itself
/// CFType-compatible (it is what `AXUIElementCopyAttributeValue` handed
/// back), so reusing the already-retained `CFType` pointer here — rather
/// than re-copying — is sound.
fn value_as_ax_value_ptr(value: &CFRetained<CFType>) -> *const c_void {
    CFRetained::as_ptr(value).as_ptr().cast_const().cast()
}
