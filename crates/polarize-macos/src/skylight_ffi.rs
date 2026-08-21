//! Runtime symbol resolution for `SkyLight.framework`, macOS's private,
//! undocumented layer between AppKit and the HID event stream.
//!
//! `tap` and `keyboard` need two things only this framework provides.
//! See `docs/INVARIANTS.md`, PINV-46. One is posting an event straight
//! into one process's queue (`SLEventPostToPid`). The other is
//! activating a process without raising its window
//! (`SLPSPostEventRecordTo`, `_SLPSSetFrontProcessWithOptions`,
//! `_SLPSGetFrontProcess`). No public header ships any of the four.
//!
//! This module differs from [`crate::ax_ffi`] on purpose. `ax_ffi`
//! links `ApplicationServices` statically, with
//! `#[link(kind = "framework")]`. Every symbol it names is public,
//! Apple-documented, and stable. A missing one would mean this crate
//! itself is broken, so a build failure is the right result there.
//!
//! These four SkyLight symbols carry none of those guarantees. They
//! are private. They are only informally documented: `yabai`
//! (github.com/koekeishiya/yabai)'s `extern.h` is the source for the
//! `_`-prefixed names below, not Apple. Apple could rename or drop any
//! of them in a future release, without notice. A static link would
//! then fail this whole binary to load. Resolving each one with
//! `dlopen`/`dlsym` at runtime avoids that. A name that disappears
//! yields `None` instead. Every caller in [`crate::input`] and
//! [`crate::window`] treats `None` as "fall back to the public API,"
//! not as a startup crash.
//!
//! Nothing here has been exercised against a real accessibility
//! session. See the crate-level docs and `docs/INVARIANTS.md`.

use std::ffi::{CString, c_void};
use std::os::raw::c_char;
use std::sync::OnceLock;

/// The on-disk path of `SkyLight.framework`. Every macOS release ships
/// it here. A private framework has no `-framework SkyLight` linker
/// flag outside its own SDK, so this loads it by path instead.
const SKYLIGHT_FRAMEWORK_PATH: &str =
    "/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";

/// `RTLD_LAZY` (`dlfcn.h`): resolve each symbol on first use, not at
/// `dlopen` time. Either mode works here, since [`resolve`] calls
/// `dlsym` on every symbol right after. `RTLD_LAZY` is the
/// conventional default.
const RTLD_LAZY: i32 = 1;

// `dlopen` and `dlsym` live in libSystem. Every macOS binary links it
// implicitly. No `#[link(...)]` attribute is needed here, the same way
// none appears on a plain libc call elsewhere in this crate.
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// A raw Mach-O `ProcessSerialNumber`. `SLPSPostEventRecordTo`,
/// `_SLPSSetFrontProcessWithOptions`, and `_SLPSGetFrontProcess` all
/// address a process by this pair, not by pid.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessSerialNumber {
    pub high: u32,
    pub low: u32,
}

/// Posts one `CGEventRef` straight into `pid`'s own event queue. This
/// bypasses the global HID stream, and the shared cursor move that
/// comes with it.
///
/// The real return type is unread either way: the calling convention
/// (System V on x86_64, AAPCS64 on arm64) leaves an unread return value
/// sitting in a register, so `()` is safe even if the real symbol
/// returns a status code.
///
/// This signature is unverified at the header level. No symbol of this
/// name appears in `yabai`'s reverse-engineered headers. It mirrors the
/// public `CGEventPostToPid`'s shape as the closest documented analog,
/// since both post one event to one process. The first real-session
/// probe is this signature's real verification. See PINV-46.
pub type SlEventPostToPidFn = unsafe extern "C" fn(pid: i32, event: *const c_void);

/// Posts a raw SkyLight event record to the process named by `psn`.
/// `bytes` is a buffer the caller builds.
///
/// `yabai`'s `window_manager_focus_window_without_raise` calls this
/// twice: once to deactivate the current front process, once to
/// activate the target. Together they flip AppKit-active state without
/// raising a window.
pub type SlpsPostEventRecordToFn =
    unsafe extern "C" fn(psn: *const ProcessSerialNumber, bytes: *const u8);

/// Raises `psn`'s window, switches to its Space, or both, depending on
/// `mode`.
///
/// `yabai` still calls this after its raise-free event records.
/// Whether `polarize` needs it too, or can drop it as the PRD
/// hypothesizes, is an open question this crate resolves empirically.
pub type SlpsSetFrontProcessWithOptionsFn =
    unsafe extern "C" fn(psn: *const ProcessSerialNumber, wid: u32, mode: u32);

/// Reads the `ProcessSerialNumber` of the current front process into
/// `psn`.
///
/// A caller needs this before deactivating the front process, so the
/// deactivate record names the right one.
pub type SlpsGetFrontProcessFn = unsafe extern "C" fn(psn: *mut ProcessSerialNumber) -> i32;

/// The four SkyLight symbols this crate's raise-free input path needs.
/// Each resolves once and stays cached for the life of the process.
///
/// A `None` field means the name did not resolve on this macOS
/// version. Apple may have renamed it, dropped it, or an entitlement
/// may hide it. Every caller must treat `None` as "unavailable," never
/// as a bug. See PINV-46.
#[derive(Debug, Clone, Copy, Default)]
pub struct SkylightSymbols {
    pub event_post_to_pid: Option<SlEventPostToPidFn>,
    pub post_event_record_to: Option<SlpsPostEventRecordToFn>,
    pub set_front_process_with_options: Option<SlpsSetFrontProcessWithOptionsFn>,
    pub get_front_process: Option<SlpsGetFrontProcessFn>,
}

impl SkylightSymbols {
    /// This struct's four fields, each paired with its symbol name and
    /// whether it resolved.
    ///
    /// A caller that only wants to log or report resolution state reads
    /// this instead of naming each field itself. `apps/polarize`'s
    /// startup log uses this, so it never pokes `SkylightSymbols`'s
    /// fields directly.
    pub fn resolution_summary(&self) -> [(&'static str, bool); 4] {
        [
            ("SLEventPostToPid", self.event_post_to_pid.is_some()),
            ("SLPSPostEventRecordTo", self.post_event_record_to.is_some()),
            (
                "_SLPSSetFrontProcessWithOptions",
                self.set_front_process_with_options.is_some(),
            ),
            ("_SLPSGetFrontProcess", self.get_front_process.is_some()),
        ]
    }
}

static SYMBOLS: OnceLock<SkylightSymbols> = OnceLock::new();

/// Resolves every SkyLight symbol this crate uses. Returns the same
/// result on every call after the first.
///
/// This never panics. A `dlopen` failure degrades the whole struct to
/// `None` fields. Any single `dlsym` miss degrades only that field. See
/// PINV-46.
pub fn symbols() -> SkylightSymbols {
    *SYMBOLS.get_or_init(resolve)
}

fn resolve() -> SkylightSymbols {
    let Some(handle) = open_framework() else {
        return SkylightSymbols::default();
    };
    SkylightSymbols {
        event_post_to_pid: unsafe { resolve_symbol(handle, "SLEventPostToPid") },
        post_event_record_to: unsafe { resolve_symbol(handle, "SLPSPostEventRecordTo") },
        set_front_process_with_options: unsafe {
            resolve_symbol(handle, "_SLPSSetFrontProcessWithOptions")
        },
        get_front_process: unsafe { resolve_symbol(handle, "_SLPSGetFrontProcess") },
    }
}

fn open_framework() -> Option<*mut c_void> {
    let path = CString::new(SKYLIGHT_FRAMEWORK_PATH).expect("path has no interior NUL");
    let handle = unsafe { dlopen(path.as_ptr(), RTLD_LAZY) };
    (!handle.is_null()).then_some(handle)
}

/// Resolves one symbol out of `handle` as a bare `fn` pointer of type
/// `F`. Returns `None` for a name `dlsym` cannot find.
///
/// # Safety
/// `handle` must be a live handle from [`open_framework`]. `F` must be
/// a bare `unsafe extern "C" fn` type. It must match `name`'s real C
/// signature. A mismatch is undefined behavior. That UB strikes the
/// moment a caller invokes the returned pointer, not at resolution
/// time.
unsafe fn resolve_symbol<F: Copy>(handle: *mut c_void, name: &str) -> Option<F> {
    debug_assert_eq!(
        std::mem::size_of::<F>(),
        std::mem::size_of::<*const c_void>(),
        "resolve_symbol::<F> requires F to be a bare fn pointer"
    );
    let c_name = CString::new(name).expect("symbol name has no interior NUL");
    let sym = unsafe { dlsym(handle, c_name.as_ptr()) };
    if sym.is_null() {
        return None;
    }
    // `dlsym` returned a real code address for `name`. The caller's
    // safety contract is what makes reinterpreting it as `F` sound.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&sym) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SkyLight.framework` ships on every real macOS install. This
    /// includes the CI runner: `cargo test --workspace` runs on
    /// `macos-latest`, per `.github/workflows/ci.yml`. `dlopen`ing it
    /// needs no display and no TCC grant, unlike everything else this
    /// crate touches. This test is genuine CI coverage, not a claim
    /// deferred to a real session.
    #[test]
    fn dlopen_resolves_the_real_framework() {
        assert!(
            open_framework().is_some(),
            "SkyLight.framework should be on disk on any real macOS install"
        );
    }

    /// A name that does not exist in the framework must yield `None`,
    /// never a panic. This is the graceful-degradation half of
    /// PINV-46.
    #[test]
    fn unresolvable_symbol_name_yields_none() {
        let handle = open_framework().expect("SkyLight.framework should open");
        let resolved: Option<SlEventPostToPidFn> =
            unsafe { resolve_symbol(handle, "PolarizeDefinitelyNotARealSymbolXYZ") };
        assert!(resolved.is_none());
    }

    /// `symbols()` must never panic. It must also be idempotent: the
    /// `OnceLock` returns the same resolution on every call.
    ///
    /// Whether each field resolves to `Some` depends on the macOS
    /// version running this test. That part is a real-session claim
    /// (PINV-46), not one this test makes. Comparing the function
    /// pointers themselves would not be meaningful either — see
    /// `unpredictable_function_pointer_comparisons` — so this checks
    /// resolved-or-not per field instead.
    #[test]
    fn symbols_is_idempotent_and_does_not_panic() {
        let first = symbols().resolution_summary();
        let second = symbols().resolution_summary();
        assert_eq!(first, second);
    }
}
