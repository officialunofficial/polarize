//! [`UiChangeWaiter`] over `AXObserver` and `CFRunLoop`.
//!
//! `polarize_core::wait` decides *how long* to wait and *what* to look
//! for. This module does the one thing only macOS can do: block until
//! the watched app posts an accessibility notification.
//!
//! ## Why a dedicated thread
//!
//! An `AXObserver` delivers notifications through a `CFRunLoop` source.
//! `apps/polarize` is an async `rmcp` server, and a Tokio worker thread
//! has no `CFRunLoop` running. Worse, a raw `AXObserverRef` and a raw
//! `CFRunLoopRef` are tied to the thread that made them; neither is
//! `Send`. So [`MacUiChangeWaiter::wait_for_change`] starts one thread
//! per call, and that thread creates, uses, and destroys every
//! Core Foundation handle by itself. Only a `Result<bool, String>` —
//! plain data — crosses back. See PINV-20 in `docs/INVARIANTS.md`.
//!
//! The caller joins the thread rather than reading a channel. Joining
//! carries the same result back, and it also proves the thread finished
//! its cleanup before the caller moves on. A channel would let the
//! caller return while the observer was still being torn down.
//!
//! ## Why the wait still consumes its whole budget
//!
//! `wait_for_change` returns `false` only after `budget` has really
//! elapsed. `polarize_core::wait` counts one call as one poll interval,
//! so a call that returned at once would turn the poll fallback into a
//! busy loop that re-walks the accessibility tree as fast as it can.
//!
//! ## What is not verified
//!
//! Nothing here has run against a real accessibility session. See the
//! crate-level "what is and is not verified" note and PINV-20's
//! enforcement entry.

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFRunLoop, CFRunLoopSource, CFString, kCFRunLoopDefaultMode};
use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::schema::AppIdentifier;
use polarize_core::traits::UiChangeWaiter;

use crate::ax_ffi::{
    AX_ERROR_SUCCESS, AXError, AXIsProcessTrusted, AXUIElementCreateApplication, AXUIElementRef,
};
use crate::window::resolve_running_app;

/// The notifications a wait listens for.
///
/// `AXCreated` covers a new element (a sheet, a menu, a row).
/// `AXLayoutChanged` covers a window that re-arranged what it already
/// had. `AXValueChanged` covers a control whose value moved, such as a
/// progress indicator or a text field.
///
/// The names are literal strings, not the framework's `kAX*Notification`
/// extern symbols, for the same reason [`crate::ax_ffi`] passes attribute
/// names as literals: the string values are long-stable, documented
/// public API, and a literal cannot link against a wrong symbol.
const WATCHED_NOTIFICATIONS: [&str; 3] = ["AXCreated", "AXLayoutChanged", "AXValueChanged"];

/// `kAXErrorNotificationAlreadyRegistered` (`AXError.h`). Registering
/// the same notification twice is a success for our purposes.
const AX_ERROR_NOTIFICATION_ALREADY_REGISTERED: AXError = -25209;

/// Opaque handle matching the C `AXObserverRef` typedef.
#[repr(C)]
pub struct OpaqueAXObserver {
    _private: [u8; 0],
}

/// `AXObserverRef` is `CFTypeRef`-compatible but is not one of
/// `objc2-core-foundation`'s known concrete types, so it is modeled as
/// its own raw pointer type — the same choice [`crate::ax_ffi`] makes
/// for `AXUIElementRef`.
pub type AXObserverRef = *const OpaqueAXObserver;

/// Apple's `AXObserverCallback` typedef, copied field for field:
///
/// ```c
/// typedef void (*AXObserverCallback)(AXObserverRef observer,
///                                    AXUIElementRef element,
///                                    CFStringRef notification,
///                                    void *refcon);
/// ```
///
/// A wrong signature here is undefined behavior. Nothing in this
/// repository can detect that, so it is copied from the header rather
/// than inferred.
type AXObserverCallback = unsafe extern "C" fn(
    observer: AXObserverRef,
    element: AXUIElementRef,
    notification: *const CFString,
    refcon: *mut c_void,
);

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXObserverCreate(
        application: i32,
        callback: AXObserverCallback,
        out_observer: *mut AXObserverRef,
    ) -> AXError;
    fn AXObserverAddNotification(
        observer: AXObserverRef,
        element: AXUIElementRef,
        notification: *const CFString,
        refcon: *mut c_void,
    ) -> AXError;
    fn AXObserverRemoveNotification(
        observer: AXObserverRef,
        element: AXUIElementRef,
        notification: *const CFString,
    ) -> AXError;
    fn AXObserverGetRunLoopSource(observer: AXObserverRef) -> *const CFRunLoopSource;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// The observer's notification callback.
///
/// It runs on the observer thread, inside `CFRunLoopRunInMode`. It does
/// the least work it can: it records that something happened. The run
/// loop then returns on its own, because the wait asks it to stop after
/// it handles one source.
///
/// # Safety
/// `refcon` is the `*mut c_void` handed to `AXObserverAddNotification`.
/// [`observe_for_change`] always passes a pointer to an [`AtomicBool`]
/// that outlives every registration.
unsafe extern "C" fn observer_callback(
    _observer: AXObserverRef,
    _element: AXUIElementRef,
    _notification: *const CFString,
    refcon: *mut c_void,
) {
    if let Some(changed) = unsafe { refcon.cast::<AtomicBool>().as_ref() } {
        changed.store(true, Ordering::SeqCst);
    }
}

/// An owned (+1) `AXUIElementRef`, released on [`Drop`].
///
/// [`crate::ax_ffi::AxElement`] already does this, but it keeps its raw
/// pointer private, and `AXObserverAddNotification` needs the raw
/// pointer. This crate does not own `ax_ffi.rs` in the change that added
/// this module, so it carries its own small guard instead.
struct OwnedElement(AXUIElementRef);

impl Drop for OwnedElement {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0.cast()) };
        }
    }
}

/// An owned (+1) `AXObserverRef`, released on [`Drop`].
///
/// Releasing the observer is what disposes of its run-loop source. A
/// leak here would be one leaked source per tool call on a server that
/// runs for hours.
struct OwnedObserver(AXObserverRef);

impl Drop for OwnedObserver {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0.cast()) };
        }
    }
}

/// `UiChangeWaiter` implementation over `AXObserver`.
#[derive(Debug, Default)]
pub struct MacUiChangeWaiter;

impl UiChangeWaiter for MacUiChangeWaiter {
    fn wait_for_change(
        &self,
        app: Option<&AppIdentifier>,
        budget: Duration,
    ) -> Result<bool, PolarizeError> {
        // The same preflight `describe` runs, for the same reason.
        // `AXIsProcessTrusted` cannot tell "never asked" from "denied",
        // so report the more conservative of the two. See PINV-10 and
        // PINV-11 in docs/INVARIANTS.md.
        if !unsafe { AXIsProcessTrusted() } {
            return Err(PolarizeError::Permission(PermissionError::NotGranted {
                kind: PermissionKind::Accessibility,
                state: PermissionState::NotDetermined,
            }));
        }
        crate::session::ensure_session_usable()?;

        if budget.is_zero() {
            return Ok(false);
        }

        // Resolve the app on this thread. `NSRunningApplication` is not
        // `Send`, so only its pid crosses to the observer thread.
        let pid = resolve_running_app(app)?.processIdentifier();

        let started = Instant::now();
        let outcome = std::thread::Builder::new()
            .name("polarize-ax-observer".to_string())
            .spawn(move || observe_for_change(pid, budget))
            .map_err(|err| {
                PolarizeError::Platform(format!("could not start the observer thread: {err}"))
            })
            .and_then(|handle| match handle.join() {
                Ok(Ok(changed)) => Ok(changed),
                Ok(Err(message)) => Err(PolarizeError::Platform(message)),
                Err(_) => Err(PolarizeError::Platform(
                    "the observer thread panicked".to_string(),
                )),
            });

        // Honour the budget even when the observer could not be built.
        // `AXObserverCreate` fails, and an app refuses every
        // notification, in exactly the cases polling exists to cover.
        // Returning early would hand `polarize_core::wait` a failure
        // with no time elapsed, which it must treat as a fault rather
        // than as one poll interval. See PINV-19 and PINV-20.
        if outcome.is_err() {
            let remaining = budget.saturating_sub(started.elapsed());
            if !remaining.is_zero() {
                std::thread::sleep(remaining);
            }
        }
        outcome
    }
}

/// Runs one whole `AXObserver` lifecycle on the calling thread.
///
/// Every Core Foundation handle here is created, used, and destroyed
/// before this function returns, so nothing that is not `Send` escapes
/// the thread. See PINV-20.
///
/// Returns `true` when a notification arrived inside `budget`.
fn observe_for_change(pid: i32, budget: Duration) -> Result<bool, String> {
    // Declared first, so it outlives every registration that points at
    // it and drops only after the observer is gone.
    let changed = AtomicBool::new(false);
    let refcon = ptr::from_ref(&changed).cast_mut().cast::<c_void>();

    let element = OwnedElement(unsafe { AXUIElementCreateApplication(pid) });
    if element.0.is_null() {
        return Err(format!(
            "AXUIElementCreateApplication returned null for pid {pid}"
        ));
    }

    let mut raw_observer: AXObserverRef = ptr::null();
    let err = unsafe { AXObserverCreate(pid, observer_callback, &mut raw_observer) };
    if err != AX_ERROR_SUCCESS || raw_observer.is_null() {
        return Err(format!(
            "AXObserverCreate failed for pid {pid} with AXError {err}"
        ));
    }
    let observer = OwnedObserver(raw_observer);

    // An app may refuse one notification and accept another; a web view
    // host often does. Registering is best-effort, and only a total
    // failure is an error. See PINV-20.
    let mut registered: Vec<&'static str> = Vec::new();
    let mut last_error = AX_ERROR_SUCCESS;
    for name in WATCHED_NOTIFICATIONS {
        let notification = CFString::from_str(name);
        let err = unsafe {
            AXObserverAddNotification(
                observer.0,
                element.0,
                &*notification as *const CFString,
                refcon,
            )
        };
        if err == AX_ERROR_SUCCESS || err == AX_ERROR_NOTIFICATION_ALREADY_REGISTERED {
            registered.push(name);
        } else {
            last_error = err;
        }
    }
    if registered.is_empty() {
        return Err(format!(
            "AXObserverAddNotification refused every notification for pid {pid}; last AXError {last_error}"
        ));
    }

    let run_loop = CFRunLoop::current().ok_or("CFRunLoopGetCurrent returned null")?;
    let source = unsafe { AXObserverGetRunLoopSource(observer.0).as_ref() }
        .ok_or("AXObserverGetRunLoopSource returned null")?;
    let mode = unsafe { kCFRunLoopDefaultMode };

    let started = Instant::now();
    run_loop.add_source(Some(source), mode);
    // `true` means "return as soon as one source is handled". The only
    // source on this fresh thread's run loop is the observer's, so the
    // call returns on the first notification, or when `budget` expires.
    let _ = CFRunLoop::run_in_mode(mode, budget.as_secs_f64(), true);
    // No `?` sits between `add_source` and here, so the source is always
    // removed again.
    run_loop.remove_source(Some(source), mode);

    for name in registered {
        let notification = CFString::from_str(name);
        unsafe {
            AXObserverRemoveNotification(observer.0, element.0, &*notification as *const CFString)
        };
    }
    drop(observer);
    drop(element);

    let changed = changed.load(Ordering::SeqCst);
    if !changed {
        // `CFRunLoopRunInMode` can return early — it reports `Finished`
        // the moment a run loop has nothing left to do. Sleeping out the
        // rest keeps the promise `polarize_core::wait` relies on: one
        // call costs one poll interval.
        let elapsed = started.elapsed();
        if elapsed < budget {
            std::thread::sleep(budget - elapsed);
        }
    }
    Ok(changed)
}
