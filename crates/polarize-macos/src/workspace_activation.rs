//! Keeps `NSWorkspace`'s live-application tracking active.
//!
//! `NSWorkspace.runningApplications` and `.frontmostApplication` are
//! not live queries. A minimal Swift probe confirmed this empirically.
//! Against a bare `CFRunLoopRun()` — no `NSApplication`, no observer —
//! pumping the main thread's run loop alone does **not** refresh
//! either property. Registering at least one observer on
//! `NSWorkspace.sharedWorkspace().notificationCenter()` does. The
//! registration itself turns on the underlying tracking. It is not
//! merely what a caller wants notified. See PINV-42.
//!
//! This module registers one observer, once, for the process's whole
//! lifetime. It registers on the real main thread, where
//! [`crate::runloop`] pumps the run loop that delivers to it. It
//! watches four notifications: launch, terminate, activate, and
//! deactivate. Those are exactly the ones
//! `runningApplications`/`frontmostApplication` are backed by. It does
//! nothing with them beyond receiving them. Receiving is the whole
//! point.

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{AnyThread, define_class, msg_send, sel};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSNotification, NSObject, NSString};

/// The notifications `NSWorkspace.runningApplications` and
/// `.frontmostApplication` are backed by. Registering for these (and
/// only these) is enough to keep both properties live — see the
/// module doc.
const ACTIVATING_NOTIFICATIONS: [&str; 4] = [
    "NSWorkspaceDidLaunchApplicationNotification",
    "NSWorkspaceDidTerminateApplicationNotification",
    "NSWorkspaceDidActivateApplicationNotification",
    "NSWorkspaceDidDeactivateApplicationNotification",
];

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `ActivationObserver` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    struct ActivationObserver;

    impl ActivationObserver {
        /// The selector every registration points at. It does nothing:
        /// the registration is what matters, not this callback. See
        /// the module doc.
        #[unsafe(method(handleWorkspaceNotification:))]
        fn __handle(&self, _notification: &NSNotification) {}
    }

    unsafe impl NSObjectProtocol for ActivationObserver {}
);

impl ActivationObserver {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Registers the persistent observer described in the module doc.
///
/// Must be called from the process's real main thread, before
/// [`crate::runloop::run_main_until_stopped`] starts pumping it. The
/// registration itself needs a run loop to deliver into once
/// notifications start arriving. The observer deliberately outlives
/// this call. It is leaked, not dropped: it must live for the whole
/// process, and there is no "shut the server down early" path that
/// would need to remove it.
pub fn activate() {
    let observer = ActivationObserver::new();
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let selector = sel!(handleWorkspaceNotification:);
    for name in ACTIVATING_NOTIFICATIONS {
        let name = NSString::from_str(name);
        unsafe { center.addObserver_selector_name_object(&observer, selector, Some(&name), None) };
    }
    std::mem::forget(observer);
}
