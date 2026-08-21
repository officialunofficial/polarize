//! Resolves a pid to a `ProcessSerialNumber`, via Carbon's Process
//! Manager.
//!
//! `SLPSPostEventRecordTo` and `_SLPSSetFrontProcessWithOptions`
//! (`crate::skylight_ffi`) address a process by
//! [`ProcessSerialNumber`](crate::skylight_ffi::ProcessSerialNumber),
//! not by pid. `polarize` otherwise only knows a target app's pid —
//! `NSRunningApplication::processIdentifier`. This module bridges the
//! two.
//!
//! `GetNextProcess` and `GetProcessPID` are Carbon Process Manager
//! calls. Apple deprecated the whole Process Manager years ago. Apple
//! never removed these two. `yabai` (github.com/koekeishiya/yabai)
//! still links `Carbon.framework` today, on current Apple Silicon
//! macOS, for exactly this pair. This module links statically for the
//! same reason [`crate::ax_ffi`] does: these calls are deprecated, but
//! they are real, Apple-documented API. Neither is an undocumented
//! private symbol. A future SDK dropping them outright should be a
//! build failure here, not a silent `None`.
//!
//! `yabai` builds a whole pid-to-psn table once, then keeps it current
//! by watching process-launch notifications. `polarize` keeps no such
//! long-lived table. This module instead walks every running process
//! with `GetNextProcess`, on every call, until it finds the pid it
//! wants. `GetNextProcess` is the same primitive `yabai` uses to build
//! its own table in the first place (`process_manager.c`). This module
//! just runs that same walk fresh each time, instead of caching it.
//!
//! Nothing here has been exercised against a real accessibility
//! session. See the crate-level docs and `docs/INVARIANTS.md`.

use crate::skylight_ffi::ProcessSerialNumber;

/// `kNoProcess` (`Processes.h`): the sentinel that starts a
/// `GetNextProcess` walk from the beginning.
const K_NO_PROCESS: u32 = 0;

/// `noErr` (`MacTypes.h`): the `OSErr`/`OSStatus` success value shared
/// across every classic Carbon call, including both calls this module
/// makes.
const NO_ERR: i16 = 0;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    /// Advances `psn` to the next running process, in an
    /// implementation-defined order. Pass `{ high: 0, low: 0 }`
    /// (`kNoProcess`) to start a walk. Returns a nonzero `OSErr` once
    /// the walk is exhausted.
    fn GetNextProcess(psn: *mut ProcessSerialNumber) -> i16;

    /// Writes `psn`'s pid into `pid`. Returns a nonzero `OSErr` if
    /// `psn` no longer names a running process.
    fn GetProcessPID(psn: *const ProcessSerialNumber, pid: *mut i32) -> i16;
}

/// Finds the [`ProcessSerialNumber`] of the running process with
/// `pid`, by walking every process `GetNextProcess` enumerates.
///
/// Returns `None` when no running process has this pid — the pid
/// exited between the caller's own lookup and this call, or `pid` was
/// never valid.
pub fn find_psn_for_pid(pid: i32) -> Option<ProcessSerialNumber> {
    let mut psn = ProcessSerialNumber {
        high: K_NO_PROCESS,
        low: K_NO_PROCESS,
    };
    loop {
        if unsafe { GetNextProcess(&mut psn) } != NO_ERR {
            return None;
        }
        let mut found_pid: i32 = 0;
        if unsafe { GetProcessPID(&psn, &mut found_pid) } == NO_ERR && found_pid == pid {
            return Some(psn);
        }
    }
}
