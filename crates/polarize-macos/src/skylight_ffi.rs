//! Runtime symbol resolution for `SkyLight.framework`, macOS's private,
//! undocumented layer between AppKit and the HID event stream.
//!
//! `tap` and `keyboard` need two things this framework alone provides
//! (see `docs/INVARIANTS.md`, PINV-46): posting an event straight into
//! one process's queue (`SLEventPostToPid`), and activating a process
//! without raising its window (`SLPSPostEventRecordTo`,
//! `_SLPSSetFrontProcessWithOptions`, `_SLPSGetFrontProcess`). No public
//! header ships any of the four.
//!
//! This differs from [`crate::ax_ffi`] on purpose. `ax_ffi` links
//! `ApplicationServices` statically with `#[link(kind = "framework")]`,
//! because every symbol it names is public, Apple-documented, and stable
//! — a missing one would mean this crate itself is broken, and a build
//! failure is the right result. These four SkyLight symbols carry none
//! of those guarantees: they are private, informally documented (cross-
//! referenced against `yabai`, github.com/koekeishiya/yabai, whose
//! `extern.h` is the source for the `_`-prefixed names below), and
//! could be renamed or dropped in a future macOS release without
//! notice. A static link would then fail the whole binary to load.
//! Resolving each one with `dlopen`/`dlsym` at runtime instead means a
//! name that disappears yields `None`, which every caller in
//! [`crate::input`] and [`crate::window`] treats as "fall back to the
//! public API" rather than a startup crash.
//!
//! Nothing here has been exercised against a real accessibility session
//! — see the crate-level docs and `docs/INVARIANTS.md`.

use std::ffi::{CString, c_void};
use std::os::raw::c_char;
use std::sync::OnceLock;

/// `/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight` — the
/// on-disk path every macOS release ships this framework at. There is no
/// `-framework SkyLight` linker flag for a private framework outside its
/// own SDK, so this is loaded by path instead.
const SKYLIGHT_FRAMEWORK_PATH: &str =
    "/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";

/// `RTLD_LAZY` (`dlfcn.h`): resolve symbols on first use rather than at
/// `dlopen` time. Either mode would work here, since every symbol is
/// resolved explicitly right after via `dlsym`; `RTLD_LAZY` is the
/// conventional default.
const RTLD_LAZY: i32 = 1;

// `dlopen`/`dlsym` live in libSystem, which every macOS binary links
// implicitly — no `#[link(...)]` attribute is needed, the same way none
// appears on a plain libc call elsewhere in this crate.
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// A raw Mach-O `ProcessSerialNumber` — `SLPSPostEventRecordTo`,
/// `_SLPSSetFrontProcessWithOptions`, and `_SLPSGetFrontProcess` all
/// address a process by this pair rather than by pid.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessSerialNumber {
    pub high: u32,
    pub low: u32,
}

/// Posts one `CGEventRef` directly into `pid`'s own event queue,
/// bypassing the global HID stream (and the shared cursor move that
/// comes with it). Return type is unread on the (System V / AAPCS64)
/// calling convention either way, so `()` is safe even if the real
/// symbol returns a status code.
///
/// Unverified at the header level: no symbol of this name is
/// mentioned in `yabai`'s reverse-engineered headers. Its signature
/// mirrors the public `CGEventPostToPid`'s shape as the closest
/// documented analog, since both post one event to one process. The
/// first real-session probe is this signature's real verification —
/// see PINV-46.
pub type SlEventPostToPidFn = unsafe extern "C" fn(pid: i32, event: *const c_void);

/// Posts a raw SkyLight event record — a `0xf8`-byte buffer built by
/// the caller — to the process named by `psn`. `yabai`'s
/// `window_manager_focus_window_without_raise` uses two calls to this,
/// one deactivating the current front process and one activating the
/// target, to flip AppKit-active state without raising a window.
pub type SlpsPostEventRecordToFn =
    unsafe extern "C" fn(psn: *const ProcessSerialNumber, bytes: *const u8);

/// Raises `psn`'s window and/or switches to its Space, depending on
/// `mode`. `yabai` still calls this after its raise-free event records;
/// whether `polarize` needs it too, or can drop it entirely as the PRD
/// hypothesizes, is this crate's open question to resolve empirically.
pub type SlpsSetFrontProcessWithOptionsFn =
    unsafe extern "C" fn(psn: *const ProcessSerialNumber, wid: u32, mode: u32);

/// Reads the `ProcessSerialNumber` of the current front process into
/// `psn`. Needed before deactivating it, so the deactivate record names
/// the right process.
pub type SlpsGetFrontProcessFn = unsafe extern "C" fn(psn: *mut ProcessSerialNumber) -> i32;

/// The four SkyLight symbols this crate's raise-free input path needs,
/// each resolved once and cached for the life of the process.
///
/// A `None` field means the name did not resolve on this macOS version
/// — Apple renamed it, dropped it, or an entitlement hides it. Every
/// caller must treat that as "unavailable" and fall back to the public
/// API, never as a bug. See PINV-46.
#[derive(Debug, Clone, Copy, Default)]
pub struct SkylightSymbols {
    pub event_post_to_pid: Option<SlEventPostToPidFn>,
    pub post_event_record_to: Option<SlpsPostEventRecordToFn>,
    pub set_front_process_with_options: Option<SlpsSetFrontProcessWithOptionsFn>,
    pub get_front_process: Option<SlpsGetFrontProcessFn>,
}

static SYMBOLS: OnceLock<SkylightSymbols> = OnceLock::new();

/// Resolves every SkyLight symbol this crate uses, or returns the
/// already-resolved result of the first call. Never panics: a
/// `dlopen` failure, or any single `dlsym` miss, degrades that field
/// (or the whole struct) to `None` rather than aborting.
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
/// `handle` must be a live handle from [`open_framework`]. `F` must be a
/// bare `unsafe extern "C" fn` type matching the real C signature of
/// `name` — a mismatch is undefined behavior the moment a caller
/// invokes the returned pointer, not at resolution time.
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
    // `dlsym` gave back a real code address for `name`; the caller's
    // safety contract is what makes reinterpreting it as `F` sound.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&sym) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SkyLight.framework` ships on every real macOS install, including
    /// the CI runner (`cargo test --workspace` runs on `macos-latest`,
    /// per `.github/workflows/ci.yml`) — `dlopen`ing it needs no display
    /// and no TCC grant, unlike everything else this crate touches. This
    /// is genuinely CI-covered, not a claim deferred to a real session.
    #[test]
    fn dlopen_resolves_the_real_framework() {
        assert!(
            open_framework().is_some(),
            "SkyLight.framework should be on disk on any real macOS install"
        );
    }

    /// A name that does not exist in the framework must yield `None`,
    /// never a panic — the graceful-degradation half of PINV-46.
    #[test]
    fn unresolvable_symbol_name_yields_none() {
        let handle = open_framework().expect("SkyLight.framework should open");
        let resolved: Option<SlEventPostToPidFn> =
            unsafe { resolve_symbol(handle, "PolarizeDefinitelyNotARealSymbolXYZ") };
        assert!(resolved.is_none());
    }

    /// `symbols()` must never panic, and must be idempotent — the
    /// `OnceLock` returns the same resolution on every call. Whether
    /// each individual field resolves to `Some` depends on the macOS
    /// version this test happens to run on, so that part is a real-
    /// session claim (PINV-46), not one this test makes. (Comparing the
    /// function pointers themselves is not meaningful — see
    /// `unpredictable_function_pointer_comparisons` — so this checks
    /// resolved-or-not per field instead.)
    #[test]
    fn symbols_is_idempotent_and_does_not_panic() {
        let first = symbols();
        let second = symbols();
        assert_eq!(
            first.event_post_to_pid.is_some(),
            second.event_post_to_pid.is_some()
        );
        assert_eq!(
            first.post_event_record_to.is_some(),
            second.post_event_record_to.is_some()
        );
        assert_eq!(
            first.set_front_process_with_options.is_some(),
            second.set_front_process_with_options.is_some()
        );
        assert_eq!(
            first.get_front_process.is_some(),
            second.get_front_process.is_some()
        );
    }
}
