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
    CFArray, CFBoolean, CFNumber, CFRange, CFRetained, CFString, CFType, CGPoint, CGSize,
};
use polarize_core::ax_batch::{AxAttributeSlot, AxAttributes, BATCHED_ATTRIBUTES};
use polarize_core::coords::{PixelPoint, PixelSize};
use std::ffi::{CString, c_void};
use std::ptr;
use std::ptr::NonNull;
use std::sync::OnceLock;

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
pub const AX_VALUE_TYPE_CF_RANGE: AXValueType = 4;
/// `kAXValueAXErrorType`. This one is never a value `polarize` wants.
/// [`AXUIElementCopyMultipleAttributeValues`] writes it into every slot
/// it could not read — see [`AxElement::batch_attributes`] and PINV-41.
pub const AX_VALUE_TYPE_AX_ERROR: AXValueType = 5;

/// `typedef UInt32 AXCopyMultipleAttributeOptions;`
/// (`AXUIElement.h`). `polarize` passes no option, which is what keeps
/// the result array aligned with the names it asked for. The one flag,
/// `kAXCopyMultipleAttributeOptionStopOnError`, would instead truncate
/// the result at the first attribute that failed.
pub type AXCopyMultipleAttributeOptions = u32;
pub const AX_COPY_MULTIPLE_ATTRIBUTE_OPTIONS_NONE: AXCopyMultipleAttributeOptions = 0;

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
    /// Reads many attributes of one element in one call.
    ///
    /// With no option set, `values` comes back with exactly one slot
    /// per name in `attributes`, in the same order. A slot the call
    /// could not read holds an `AXValue` of type
    /// `kAXValueAXErrorType`, not a gap and not a null. See
    /// [`AxElement::batch_attributes`] and PINV-41.
    fn AXUIElementCopyMultipleAttributeValues(
        element: AXUIElementRef,
        attributes: *const CFArray,
        options: AXCopyMultipleAttributeOptions,
        values: *mut *const CFArray,
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
    /// The process id that owns `element`. A hit test needs this: the
    /// system-wide element can return an element of any app.
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
    fn AXValueGetType(value: *const c_void) -> AXValueType;
    /// The `CFTypeID` of `AXValueRef`. `AXValueGetType` is defined only
    /// for that type, so this is what makes calling it sound.
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetValue(value: *const c_void, the_type: AXValueType, out: *mut c_void) -> bool;
    /// Wraps a `CGPoint`/`CGSize` in the `AXValue` box every geometric
    /// AX attribute takes. Returns `NULL` on failure.
    fn AXValueCreate(the_type: AXValueType, value_ptr: *const c_void) -> *const CFType;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFRelease(cf: *const c_void);
}

// `dlsym`/`RTLD_DEFAULT` live in libSystem, linked implicitly into
// every macOS binary — no `#[link(...)]` attribute needed, mirroring
// `crate::skylight_ffi`'s own `dlopen`/`dlsym` declarations.
unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const std::os::raw::c_char) -> *mut c_void;
}

/// `RTLD_DEFAULT` (`dlfcn.h`): search every image already loaded into
/// this process, in load order, rather than one specific handle from
/// `dlopen`. `ApplicationServices` is already loaded — this module's
/// own `#[link]` block above put it there — so this is enough to find
/// [`GET_WINDOW`]'s symbol without a separate `dlopen` call.
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

/// `_AXUIElementGetWindow(AXUIElementRef, CGWindowID *) -> AXError`.
/// `CGWindowID` is `u32`.
type GetWindowFn = unsafe extern "C" fn(AXUIElementRef, *mut u32) -> AXError;

static GET_WINDOW: OnceLock<Option<GetWindowFn>> = OnceLock::new();

/// Resolves `_AXUIElementGetWindow` once, and caches the result.
///
/// No public header declares this function. Every independent
/// reference to it — `yabai`'s `extern.h`, `jmgao/metamove`'s
/// `window.mm` — declares it with `__attribute__((weak_import))`, and
/// checks it resolves before calling it. That is the same "private,
/// can vanish without notice" posture PINV-46 already established for
/// `SkyLight.framework`'s symbols. This resolves it the same way, at
/// runtime, instead of a static `#[link]` extern. A missing symbol
/// yields `None`, never a panic — see [`AxElement::window_id`].
fn get_window_fn() -> Option<GetWindowFn> {
    *GET_WINDOW.get_or_init(|| {
        let name = CString::new("_AXUIElementGetWindow").expect("no interior NUL");
        let sym = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
        if sym.is_null() {
            return None;
        }
        // `dlsym` gave back a real code address for `name`; the
        // fixed, hand-verified `GetWindowFn` signature above is what
        // makes reinterpreting it as one sound.
        Some(unsafe { std::mem::transmute::<*mut c_void, GetWindowFn>(sym) })
    })
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

    /// The system-wide accessibility element.
    ///
    /// A hit test needs this one. `AXUIElementCopyElementAtPosition`
    /// searches only inside the element it is called on, so an
    /// application element reports nothing about another app's window
    /// covering the point. Only the system-wide element hit-tests
    /// across applications. See PINV-32.
    pub fn system_wide() -> Self {
        Self(unsafe { AXUIElementCreateSystemWide() })
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

    /// Reads every attribute an [`AxNode`](polarize_core::ax::AxNode)
    /// carries, in one call where that works.
    ///
    /// This is the read path a tree walk repeats once per node, so its
    /// cost is the cost of `describe`. One batch call replaces the nine
    /// to eleven single reads the fallback below performs.
    ///
    /// The fallback runs when the batch call fails outright, and when
    /// its result array does not line up with the names it was asked
    /// for. A degraded but correct read beats a fast wrong one, so a
    /// batch this code cannot trust is never guessed at. See PINV-41.
    pub fn node_attributes(&self) -> AxAttributes {
        self.batch_attributes(&BATCHED_ATTRIBUTES)
            .and_then(|slots| AxAttributes::from_batch(&slots))
            .unwrap_or_else(|| self.node_attributes_one_at_a_time())
    }

    /// Reads `attributes` in one `AXUIElementCopyMultipleAttributeValues`
    /// call, as one slot per name.
    ///
    /// Returns `None` when the call itself fails. The caller then reads
    /// the attributes one at a time (PINV-41).
    ///
    /// ## The error placeholder
    ///
    /// No option is passed, so the call reads every name it can and
    /// reports the rest in place. A slot it could not read holds an
    /// `AXValue` of type `kAXValueAXErrorType`. That placeholder is a
    /// real Core Foundation object of a real type. Only its
    /// `AXValueType` separates it from a value, so
    /// [`slot_from_value`] checks that type on every slot.
    pub fn batch_attributes(&self, attributes: &[&str]) -> Option<Vec<AxAttributeSlot>> {
        let names: Vec<CFRetained<CFString>> = attributes
            .iter()
            .map(|name| CFString::from_str(name))
            .collect();
        let names = CFArray::from_retained_objects(&names);
        let names: &CFArray = AsRef::as_ref(&*names);

        let mut values: *const CFArray = ptr::null();
        let err = unsafe {
            AXUIElementCopyMultipleAttributeValues(
                self.0,
                ptr::from_ref(names),
                AX_COPY_MULTIPLE_ATTRIBUTE_OPTIONS_NONE,
                &mut values,
            )
        };
        if err != AX_ERROR_SUCCESS {
            return None;
        }
        let values = NonNull::new(values.cast_mut())?;
        let values: CFRetained<CFArray> = unsafe { CFRetained::from_raw(values) };
        Some(
            (0..values.count())
                .map(|index| slot_from_value(unsafe { values.value_at_index(index) }))
                .collect(),
        )
    }

    /// Reads the same attributes with one call each.
    ///
    /// This is the fallback [`Self::node_attributes`] uses when the
    /// batch call fails. It reads exactly the attributes
    /// [`BATCHED_ATTRIBUTES`] names, and it degrades each one to the
    /// same default (PINV-12, PINV-16), so both paths build the same
    /// node.
    fn node_attributes_one_at_a_time(&self) -> AxAttributes {
        let defaults = AxAttributes::default();
        let non_empty = |attribute: &str| {
            self.string_attribute(attribute)
                .filter(|value| !value.is_empty())
        };
        let role = self.string_attribute("AXRole").unwrap_or(defaults.role);
        let label = ["AXTitle", "AXDescription", "AXValue"]
            .into_iter()
            .find_map(non_empty);
        let position = self.point_attribute("AXPosition").unwrap_or_default();
        let size = self.size_attribute("AXSize").unwrap_or_default();
        let enabled = self.bool_attribute("AXEnabled").unwrap_or(defaults.enabled);
        AxAttributes {
            role,
            label,
            position: PixelPoint {
                x: position.x,
                y: position.y,
            },
            size: PixelSize {
                width: size.width,
                height: size.height,
            },
            enabled,
            subrole: non_empty("AXSubrole"),
            role_description: non_empty("AXRoleDescription"),
            identifier: non_empty("AXIdentifier"),
            help: non_empty("AXHelp"),
        }
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

    /// Writes an `AXValue`-wrapped `CFRange`, e.g.
    /// `AXSelectedTextRange`. `location` and `length` count UTF-16 code
    /// units, which is what the AX API means by a text index.
    pub fn set_range_attribute(
        &self,
        attribute: &str,
        location: isize,
        length: isize,
    ) -> Result<(), String> {
        let range = CFRange { location, length };
        let boxed = ax_value(AX_VALUE_TYPE_CF_RANGE, &range)?;
        self.set_attribute(attribute, &boxed)
    }

    /// The process id of the app that owns this element.
    ///
    /// A hit test asks the system-wide element, so the element it gets
    /// back may belong to any app — including one the caller never
    /// named. This is how the caller learns which. See PINV-32.
    pub fn pid(&self) -> Option<i32> {
        let mut pid: i32 = 0;
        let err = unsafe { AXUIElementGetPid(self.0, &mut pid) };
        (err == AX_ERROR_SUCCESS && pid > 0).then_some(pid)
    }

    /// This element's `CGWindowID`, when it is a window element and
    /// `_AXUIElementGetWindow` resolved.
    ///
    /// Returns `None` when the symbol did not resolve on this macOS
    /// version, or when the call itself fails — e.g. `self` is not a
    /// window element. A caller degrades to its own fallback either
    /// way; see [`crate::window::MacWindowManager::activate_app_without_raise`]
    /// and PINV-48 in `docs/INVARIANTS.md`.
    pub fn window_id(&self) -> Option<u32> {
        let get_window = get_window_fn()?;
        let mut wid: u32 = 0;
        let err = unsafe { get_window(self.0, &mut wid) };
        (err == AX_ERROR_SUCCESS).then_some(wid)
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

/// Reads one slot of a batch result into an [`AxAttributeSlot`].
///
/// `raw` is borrowed (+0) out of the result array, exactly as
/// [`AxElement::children`] borrows a child.
///
/// Every branch that is not a value this reader understands ends at
/// [`AxAttributeSlot::Unread`], which the pure reader then degrades to
/// the field's default. That covers a null slot, the batch call's
/// `kAXValueAXErrorType` placeholder, and a value of a type the
/// attribute does not use. Each conversion mirrors one single-attribute
/// reader above, so a batched read and a one-at-a-time read of the same
/// element agree. See PINV-41.
fn slot_from_value(raw: *const c_void) -> AxAttributeSlot {
    let Some(borrowed) = NonNull::new(raw.cast_mut()) else {
        return AxAttributeSlot::Unread;
    };
    // Retain the borrowed reference so `CFRetained` owns a real +1 —
    // the same handoff `children` and `action_names` perform.
    let retained = unsafe { CFRetain(borrowed.as_ptr()) };
    let Some(retained) = NonNull::new(retained.cast_mut().cast::<CFType>()) else {
        return AxAttributeSlot::Unread;
    };
    let value: CFRetained<CFType> = unsafe { CFRetained::from_raw(retained) };

    // Mirrors `string_attribute`.
    if let Some(text) = value.downcast_ref::<CFString>() {
        return AxAttributeSlot::Text(text.to_string());
    }
    // Mirrors `bool_attribute`.
    if let Some(flag) = value.downcast_ref::<CFBoolean>() {
        return AxAttributeSlot::Flag(flag.value());
    }

    // `AXValueGetType` is defined only for an `AXValueRef`. A batched
    // read returns whatever type each attribute really has, and several
    // are neither a string, a boolean, nor an `AXValue`: `AXValue` on a
    // slider or a stepper is a `CFNumber`, and a future attribute could
    // be anything. Handing one of those to `AXValueGetType` reads a
    // foreign object through the wrong lens. The single-attribute
    // readers never met this, because each one asked for a specific
    // attribute whose type it already knew.
    let raw = value_as_ax_value_ptr(&value);
    if unsafe { CFGetTypeID(raw) } != unsafe { AXValueGetTypeID() } {
        return AxAttributeSlot::Unread;
    }
    match unsafe { AXValueGetType(raw) } {
        // The placeholder for a slot the batch call could not read. It
        // is a real object of a real type, so nothing but this check
        // separates it from a value. Treating it as one would write a
        // wrong point, size, or string into the node, which PINV-16
        // forbids.
        AX_VALUE_TYPE_AX_ERROR => AxAttributeSlot::Unread,
        // Mirrors `point_attribute`.
        AX_VALUE_TYPE_CG_POINT => {
            let mut point = CGPoint::ZERO;
            let ok =
                unsafe { AXValueGetValue(raw, AX_VALUE_TYPE_CG_POINT, (&raw mut point).cast()) };
            if ok {
                AxAttributeSlot::Point(PixelPoint {
                    x: point.x,
                    y: point.y,
                })
            } else {
                AxAttributeSlot::Unread
            }
        }
        // Mirrors `size_attribute`.
        AX_VALUE_TYPE_CG_SIZE => {
            let mut size = CGSize::ZERO;
            let ok = unsafe { AXValueGetValue(raw, AX_VALUE_TYPE_CG_SIZE, (&raw mut size).cast()) };
            if ok {
                AxAttributeSlot::Size(PixelSize {
                    width: size.width,
                    height: size.height,
                })
            } else {
                AxAttributeSlot::Unread
            }
        }
        _ => AxAttributeSlot::Unread,
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

/// Walks `path` down an element's children, one index per step.
///
/// `polarize-core` resolves an index path against the tree `describe`
/// returned; this follows the same indices down the live hierarchy. The
/// two must agree — that is PINV-18, and it is why an out-of-range index
/// is an error rather than a stop at the parent. The app changed its
/// interface between the two walks, so acting on the parent would press
/// something the caller never named.
pub fn walk_path(root: AxElement, path: &[usize]) -> Result<AxElement, String> {
    let mut current = root;
    for (step, &index) in path.iter().enumerate() {
        let children = current.children();
        let count = children.len();
        current = children.into_iter().nth(index).ok_or_else(|| {
            format!(
                "the element path {path:?} left the tree at step {step}: \
                 index {index} of {count} children. The app changed its \
                 interface. Run `describe` again."
            )
        })?;
    }
    Ok(current)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `dlsym(RTLD_DEFAULT, ...)` against a symbol in an
    /// already-linked framework needs no display and no TCC grant,
    /// unlike everything else in this module. This runs for real in
    /// CI (`cargo test --workspace` runs on `macos-latest`, per
    /// `.github/workflows/ci.yml`), not just on a real session.
    ///
    /// Whether `_AXUIElementGetWindow` actually resolves to `Some` on
    /// this macOS version is a real-session claim, not one this test
    /// makes — see PINV-48.
    #[test]
    fn get_window_fn_is_idempotent_and_does_not_panic() {
        assert_eq!(get_window_fn().is_some(), get_window_fn().is_some());
    }
}
