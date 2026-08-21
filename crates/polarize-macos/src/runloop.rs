//! Pumps the process's real main-thread `CFRunLoop`.
//!
//! `NSWorkspace`'s `runningApplications` and `frontmostApplication`
//! are not live queries. Apple documents both as a cache. It
//! "persist[s] until the next turn of the main run loop in a common
//! mode" (`NSRunningApplication`'s class overview). `NSWorkspace
//! .runningApplications`'s own discussion states the same policy. A
//! `#[tokio::main]` binary parks the real OS main thread inside
//! tokio's own executor. It never turns a `CFRunLoop` there. So that
//! cache freezes at whatever it read first, and never updates again —
//! see PINV-42. Reading it from any thread is documented safe. Only
//! the *refresh* needs the main run loop turning.
//!
//! `apps/polarize` therefore keeps the real main thread free for
//! [`run_main_until_stopped`]. It runs its own async server logic on a
//! separately-constructed `tokio` runtime instead of `#[tokio::main]`.

use objc2_core_foundation::{CFRunLoop, CFRunLoopRunResult, kCFRunLoopDefaultMode};

/// Runs the calling thread's `CFRunLoop`, in the default mode, until
/// [`stop_main`] is called from any thread.
///
/// Must be called from the process's real main thread. `CFRunLoop`
/// has no notion of "the main run loop" other than whichever thread
/// this actually runs on. `stop_main` only ever stops
/// `CFRunLoopGetMain`'s run loop.
///
/// `CFRunLoopRunInMode` can return before `stop_main` is ever called
/// — `TimedOut`, or `Finished` when no source is installed yet. So
/// this loops until it sees `Stopped`.
pub fn run_main_until_stopped() {
    let mode = unsafe { kCFRunLoopDefaultMode };
    loop {
        let result = CFRunLoop::run_in_mode(mode, f64::MAX, false);
        if result == CFRunLoopRunResult::Stopped {
            break;
        }
    }
}

/// Asks the process's main run loop to stop, so
/// [`run_main_until_stopped`] returns.
///
/// `CFRunLoopStop` is documented safe to call from any thread that
/// holds the target `CFRunLoopRef`, which is exactly what
/// `CFRunLoop::main()` hands back.
pub fn stop_main() {
    if let Some(main) = CFRunLoop::main() {
        main.stop();
    }
}
