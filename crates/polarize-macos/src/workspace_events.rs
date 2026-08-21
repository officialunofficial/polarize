//! The macOS side of the two workspace tools: a snapshot reader, and an
//! `NSWorkspace` notification observer on a thread of its own.
//!
//! Read `polarize_core::workspace_events` first. It explains why
//! `polarize` offers a bounded wait rather than an event stream, and
//! why the wait watches a notification channel and a poll channel at
//! the same time (PINV-36).
//!
//! ## Why a dedicated thread, again
//!
//! `NSNotificationCenter` delivers through a run loop, exactly as an
//! `AXObserver` does. `apps/polarize` is an async `rmcp` server, and no
//! Tokio worker thread runs a `CFRunLoop`. So
//! [`MacWorkspaceNotificationWaiter::wait_for_workspace_notification`]
//! starts one thread per call. That thread creates the observer object,
//! registers it, runs the run loop, removes the registration, and
//! releases the object, all before it ends. Only plain data crosses
//! back. This is the same rule PINV-20 states for `AXObserver`.
//!
//! ## The open question a human must answer
//!
//! Nobody has confirmed that `NSWorkspace`'s notifications reach a
//! process whose *main* run loop never runs. `NSWorkspace` posts these
//! notifications from a distributed-notification port, and Apple does
//! not document which run loop that port is scheduled on. If the port
//! lives on the main run loop, this observer receives nothing, and
//! every wait is carried by the poll channel instead.
//!
//! That is why `polarize_core::workspace_events` marks each event with
//! the channel that saw it. Run `await_workspace_event`, switch apps,
//! and read `event.source`. `notification` means this module works.
//! `poll` means it does not, and the feature still does. See PINV-36.
//!
//! ## Why neither tool preflights the login session
//!
//! PINV-23 makes every tool that captures pixels, reads the
//! accessibility tree, or posts input call
//! [`crate::session::ensure_session_usable`] first. Neither tool here
//! does any of those three things. Both read `NSWorkspace`, which keeps
//! working while the screen is locked and while another user holds the
//! console.
//!
//! More than that: refusing to run off the console would break the one
//! tool that reports it. `frontmost_app` returns `on_console` as a
//! field, and `await_workspace_event` exists partly to report a Fast
//! User Switch. A preflight would turn both facts into an error the
//! caller cannot act on. This is the same reasoning PINV-23's exclusion
//! note gives for the two AppleScript tools.
//!
//! Neither tool needs a TCC permission either. `NSWorkspace` publishes
//! the frontmost app and these notifications to any process.
//!
//! ## What is not verified
//!
//! Nothing here has run. See the crate-level "what is and is not
//! verified" note and PINV-36's enforcement entry.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2_foundation::{NSNotification, NSObject, NSString};
use polarize_core::error::PolarizeError;
use polarize_core::session::SessionInspector;
use polarize_core::workspace_events::{
    WorkspaceApp, WorkspaceEvent, WorkspaceEventKind, WorkspaceEventSource, WorkspaceInspector,
    WorkspaceNotificationWaiter, WorkspaceSnapshot,
};

// ---- the snapshot reader ------------------------------------------------

/// The moment this process first read a snapshot.
///
/// `WorkspaceSnapshot::monotonic_ms` only ever appears in a difference
/// between two snapshots, so any fixed origin works. `Instant` is the
/// clock that stops while the Mac sleeps, which is exactly the property
/// `polarize_core::workspace_events::diff_snapshots` reads a wake from.
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

/// [`WorkspaceInspector`] over `NSWorkspace` and the login-session
/// dictionary.
#[derive(Debug, Default)]
pub struct MacWorkspaceInspector;

impl WorkspaceInspector for MacWorkspaceInspector {
    fn snapshot(&self) -> Result<WorkspaceSnapshot, PolarizeError> {
        let workspace = NSWorkspace::sharedWorkspace();
        let frontmost = workspace.frontmostApplication().map(|app| WorkspaceApp {
            name: app
                .localizedName()
                .map(|name| name.to_string())
                .unwrap_or_default(),
            bundle_id: app.bundleIdentifier().map(|id| id.to_string()),
        });

        // The same reader PINV-23's preflight uses. Reading it here
        // reports Fast User Switching instead of refusing over it.
        let session = crate::session::MacSessionInspector.session_state();

        let start = PROCESS_START.get_or_init(Instant::now);
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or(0);

        Ok(WorkspaceSnapshot {
            frontmost,
            on_console: session.on_console,
            monotonic_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            wall_ms,
        })
    }
}

/// Reads which app is frontmost right now, with no wait at all.
pub fn frontmost_app()
-> Result<polarize_core::workspace_events::FrontmostAppResponse, PolarizeError> {
    polarize_core::workspace_events::perform_frontmost_app(&MacWorkspaceInspector)
}

// ---- the notification observer ------------------------------------------

/// What the observer object records.
///
/// A `Mutex` rather than a `RefCell`: the Objective-C runtime hands the
/// object back through a callback, and a `Mutex` states the sharing
/// rule plainly instead of relying on the run loop being single
/// threaded.
struct ObserverIvars {
    seen: Mutex<Vec<WorkspaceEventKind>>,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `WorkspaceObserver` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[ivars = ObserverIvars]
    struct WorkspaceObserver;

    impl WorkspaceObserver {
        /// The selector every registration points at.
        ///
        /// It runs on the observer thread, inside the run loop. It does
        /// the least work it can: it records which notification came in.
        #[unsafe(method(handleWorkspaceNotification:))]
        fn __handle(&self, notification: &NSNotification) {
            let name = notification.name().to_string();
            if let Some(kind) = WorkspaceEventKind::from_notification_name(&name)
                && let Ok(mut seen) = self.ivars().seen.lock()
            {
                seen.push(kind);
            }
        }
    }

    unsafe impl NSObjectProtocol for WorkspaceObserver {}
);

impl WorkspaceObserver {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(ObserverIvars {
            seen: Mutex::new(Vec::new()),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn collected(&self) -> Vec<WorkspaceEventKind> {
        self.ivars()
            .seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }
}

/// [`WorkspaceNotificationWaiter`] over `NSWorkspace`'s notification
/// centre and `CFRunLoop`.
#[derive(Debug, Default)]
pub struct MacWorkspaceNotificationWaiter;

impl WorkspaceNotificationWaiter for MacWorkspaceNotificationWaiter {
    fn wait_for_workspace_notification(
        &self,
        budget: Duration,
    ) -> Result<Vec<WorkspaceEvent>, PolarizeError> {
        if budget.is_zero() {
            return Ok(Vec::new());
        }
        std::thread::Builder::new()
            .name("polarize-workspace-observer".to_string())
            .spawn(move || observe(budget))
            .map_err(|err| {
                PolarizeError::Platform(format!(
                    "could not start the workspace observer thread: {err}"
                ))
            })
            .and_then(|handle| match handle.join() {
                Ok(kinds) => Ok(kinds
                    .into_iter()
                    .map(|kind| WorkspaceEvent {
                        kind,
                        source: WorkspaceEventSource::Notification,
                        // The app is filled in from the snapshot the
                        // core wait takes right after this call. Reading
                        // `NSWorkspaceApplicationKey` out of the
                        // notification would duplicate that, with more
                        // native code and no more information.
                        app: None,
                    })
                    .collect()),
                Err(_) => Err(PolarizeError::Platform(
                    "the workspace observer thread panicked".to_string(),
                )),
            })
    }
}

/// Runs one whole observer lifecycle on the calling thread.
///
/// Every Objective-C object here is created, used, and released before
/// this function returns, so nothing that is not `Send` escapes the
/// thread. Only a `Vec<WorkspaceEventKind>` crosses back. See PINV-20
/// and PINV-36.
fn observe(budget: Duration) -> Vec<WorkspaceEventKind> {
    let observer = WorkspaceObserver::new();
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let selector = sel!(handleWorkspaceNotification:);

    // The names come from `polarize_core`'s own table, so this module
    // and the core wait agree on one list. Registration is best-effort:
    // `addObserver:selector:name:object:` returns nothing, so a name
    // this macOS does not publish simply never fires.
    for kind in WorkspaceEventKind::ALL {
        let name = NSString::from_str(kind.notification_name());
        unsafe { center.addObserver_selector_name_object(&observer, selector, Some(&name), None) };
    }

    let mode = unsafe { kCFRunLoopDefaultMode };
    let started = Instant::now();
    // `true` means "return as soon as one source is handled". The only
    // source this fresh thread's run loop should carry is the
    // notification port, so the call returns on the first notification,
    // or when the budget expires.
    let _ = CFRunLoop::run_in_mode(mode, budget.as_secs_f64(), true);

    // No `?` sits between the registration and here, so the observer is
    // always removed again.
    unsafe { center.removeObserver(&observer) };
    let collected = observer.collected();

    // `CFRunLoopRunInMode` reports `Finished` the moment a run loop has
    // nothing left to do, which is exactly what happens if the
    // notification port never reached this thread. Sleeping out the
    // rest keeps the promise the core wait relies on: one call costs
    // one poll interval, never a busy loop. See PINV-20 and PINV-36.
    if collected.is_empty() {
        let elapsed = started.elapsed();
        if elapsed < budget {
            std::thread::sleep(budget - elapsed);
        }
    }
    collected
}
