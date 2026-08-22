//! Resolves `polarize`'s own TCC "responsible process" identity, and
//! makes `polarize` its own responsible process through a disclaimed
//! self-respawn. See PINV-52.
//!
//! A real `.app` bundle (`just bundle-app`) gives `polarize` a
//! LaunchServices-acceptable identity on disk. It does not, by
//! itself, change which process TCC treats as responsible for an
//! Apple Event send. PINV-44's "Correction" section confirmed that
//! live. TCC assigns responsibility at spawn, and a plain
//! `fork`/`exec` inherits it from the parent, bundle directory or
//! not. Two things break that inheritance. One is a LaunchServices
//! launch — `open`, Finder, `NSWorkspace`. The other is the private
//! `responsibility_spawnattrs_setdisclaim` attribute. PINV-51 already
//! uses it, for its one-shot bootstrap send. `polarize`'s MCP stdio
//! use case is spawned directly by its client — never through
//! LaunchServices. It needs its stdio pipes preserved. So a
//! disclaimed self-respawn is the mechanism this module adds.
//!
//! This module resolves two private, undocumented symbols.
//! [`crate::skylight_ffi`] and [`crate::disclaimed_spawn`] resolve
//! theirs the same way. Each uses `dlsym(RTLD_DEFAULT, ...)`, cached
//! once. Each degrades to `None`, rather than panicking, when a name
//! does not resolve on this macOS version. That is PINV-46's pattern.
//! The disclaimed-spawn attribute setup itself is not duplicated here
//! — see [`crate::disclaimed_spawn::init_disclaimed_attr`], which this
//! module's own send reuses.
//!
//! [`respawn_self_disclaimed`] must run before any other thread
//! starts. It mutates this process's own environment, via
//! [`std::env::set_var`]. That is only sound with no other thread that
//! could read it concurrently. `apps/polarize/src/main.rs` calls it
//! before building its `tokio` runtime, for exactly this reason.

use std::ffi::{CString, c_void};
use std::os::raw::c_char;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};

use crate::disclaimed_spawn;

// `dlsym` is declared separately in every module that needs private
// symbol resolution in this crate (`ax_ffi`, `skylight_ffi`,
// `disclaimed_spawn`) rather than shared — each is a plain,
// zero-cost `libSystem` link declaration, not real duplicated logic.
unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `dlsym`'s "search every already-loaded image" pseudo-handle, from
/// `<dlfcn.h>`. See `disclaimed_spawn.rs`'s own copy of this constant
/// for why there is nothing to `dlopen` here either.
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

/// Env var this process's own disclaimed respawn sets on itself
/// before spawning. The respawned child's own
/// [`should_respawn_disclaimed`] check then sees it. That way, it
/// does not respawn again. Its value is never read, only its
/// presence.
const RESPAWN_SENTINEL_VAR: &str = "POLARIZE_DISCLAIMED_RESPAWN";

/// A live session confirmed a real risk. Plain `waitpid`-and-exit
/// does not handle it. `posix_spawnp`, like `fork`, puts a child in
/// its parent's own process group. So a terminal's job-control
/// `Ctrl-C` — which signals the whole foreground process group —
/// reaches both. A `kill <pid>` naming this process's own pid
/// specifically does not. Confirmed live: `SIGTERM` sent only to the
/// parent's pid left the respawned child running, orphaned. An MCP
/// client shutting `polarize` down typically does exactly that. It
/// sends `kill(child_pid, SIGTERM)`, not a process-group-wide signal.
/// So the parent must forward what it receives.
///
/// `signum: i32, handler: SigHandler` are `<signal.h>`'s
/// `signal(int, void (*)(int))`. On Darwin this installs a BSD
/// "reliable" handler. It stays installed across deliveries, unlike
/// old SysV `signal()` semantics. So no re-arming is needed after
/// each call.
type SigHandler = extern "C" fn(i32);

unsafe extern "C" {
    fn signal(signum: i32, handler: SigHandler) -> SigHandler;
    fn kill(pid: i32, sig: i32) -> i32;
    /// Darwin's per-thread `errno` accessor. There is no `errno`
    /// global symbol to `extern` directly on Darwin. Every libc call
    /// site that needs it calls this instead.
    fn __error() -> *mut i32;
}

const EINTR: i32 = 4;
const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGQUIT: i32 = 3;
const SIGTERM: i32 = 15;

/// The respawned child's pid, once known — `0` before a respawn's
/// child exists yet. [`forward_signal_to_child`] reads this from
/// inside a signal handler, so it must be an atomic, not a plain
/// `static mut`.
static CHILD_PID: AtomicI32 = AtomicI32::new(0);

/// Forwards a caught signal to [`CHILD_PID`], when one is set.
///
/// Async-signal-safe: an atomic load plus [`kill`], both on POSIX's
/// async-signal-safe function list. Nothing else runs in this
/// handler.
extern "C" fn forward_signal_to_child(signal_number: i32) {
    let child = CHILD_PID.load(Ordering::SeqCst);
    if child > 0 {
        // SAFETY: `kill` is async-signal-safe; `child` is a plain
        // `i32` pid.
        unsafe {
            kill(child, signal_number);
        }
    }
}

/// Installs [`forward_signal_to_child`] for the signals that ask
/// `polarize` to stop. A process manager or an interactive shell most
/// plausibly sends these: `SIGHUP`, `SIGINT`, `SIGQUIT`, `SIGTERM`.
/// Must run before [`CHILD_PID`] is set. That way no delivered signal
/// is silently dropped, between "handler installed" and "child pid
/// known". Before the pid is known, this handler is a safe no-op —
/// `child == 0`.
fn install_signal_forwarding() {
    for signal_number in [SIGHUP, SIGINT, SIGQUIT, SIGTERM] {
        // SAFETY: `forward_signal_to_child` is `extern "C" fn(i32)`,
        // exactly `signal`'s documented handler type. Installing a
        // handler itself has no other precondition.
        unsafe {
            signal(signal_number, forward_signal_to_child);
        }
    }
}

type ResponsibilityGetPidResponsibleForPidFn = unsafe extern "C" fn(pid: i32) -> i32;

/// Resolves `responsibility_get_pid_responsible_for_pid` once and
/// caches the result. `None` means the name did not resolve on this
/// macOS version. Every caller must treat that as "cannot determine
/// responsibility here," not as a bug.
///
/// Not `responsibility_get_responsible_for_pid` — a similarly named
/// sibling in the same private family.
/// `/usr/lib/system/libquarantine.tbd` lists both, alongside
/// `responsibility_get_uniqueid_responsible_for_pid`. That one does
/// not return a bare `pid_t`. Calling it as `pid_t -> pid_t` crashed
/// with `SIGBUS`, in this crate's own tests. `..._get_pid_..._for_pid`
/// is the variant whose name — and observed behavior once called
/// correctly below — matches "answer a pid with a pid."
fn responsible_pid_fn() -> Option<ResponsibilityGetPidResponsibleForPidFn> {
    static CACHE: OnceLock<Option<usize>> = OnceLock::new();
    let address = *CACHE.get_or_init(|| {
        let name = c"responsibility_get_pid_responsible_for_pid";
        // SAFETY: `RTLD_DEFAULT` and `name` are both valid for the
        // duration of this call; `name` is a `'static` C string.
        let symbol = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
        (!symbol.is_null()).then_some(symbol as usize)
    });
    // SAFETY: `address`, when present, came from a real `dlsym` result
    // for this exact symbol name, immediately above.
    address.map(|address| unsafe {
        std::mem::transmute::<usize, ResponsibilityGetPidResponsibleForPidFn>(address)
    })
}

/// Looks up `pid`'s TCC-responsible process. `None` when the private
/// symbol does not resolve on this macOS version. `pid` itself is a
/// valid answer — a process can be its own responsible process.
pub fn responsible_pid_for(pid: i32) -> Option<i32> {
    let resolve = responsible_pid_fn()?;
    // SAFETY: `resolve` came from a real `dlsym` result for this exact
    // symbol name, resolved in `responsible_pid_fn`. `pid` is a plain
    // `i32`, the only argument this C function takes.
    let responsible = unsafe { resolve(pid) };
    (responsible > 0).then_some(responsible)
}

/// Whether this process is its own TCC-responsible process right now.
/// `None` when [`responsible_pid_for`] cannot determine it.
pub fn is_self_responsible() -> Option<bool> {
    let own_pid = std::process::id() as i32;
    responsible_pid_for(own_pid).map(|responsible| responsible == own_pid)
}

/// One line summarizing this process's own responsible-process state,
/// for `apps/polarize`'s startup log. This is the observable half of
/// PINV-52, checkable with zero TCC grants. Mirrors
/// [`crate::skylight_ffi::SkylightSymbols::resolution_summary`]'s role
/// for PINV-46.
pub fn responsibility_summary() -> String {
    let own_pid = std::process::id() as i32;
    match responsible_pid_for(own_pid) {
        Some(responsible) if responsible == own_pid => "self-responsible".to_string(),
        Some(responsible) => format!("responsible_pid={responsible}"),
        None => {
            "undeterminable (responsibility_get_pid_responsible_for_pid unresolved)".to_string()
        }
    }
}

/// Whether this process's environment already carries
/// [`RESPAWN_SENTINEL_VAR`]. This is the pure half of
/// [`already_respawned`], factored out so it is unit-testable. It
/// avoids touching real process environment.
fn sentinel_present(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some()
}

/// Whether this process is itself the product of one disclaimed
/// self-respawn already.
pub fn already_respawned() -> bool {
    sentinel_present(std::env::var_os(RESPAWN_SENTINEL_VAR).as_deref())
}

/// Pure decision: should the caller respawn itself disclaimed?
///
/// - `sentinel_present` — this process is already the product of one
///   respawn; never respawn a second time.
/// - `disclaim_available` — `responsibility_spawnattrs_setdisclaim`
///   resolved; a respawn with no disclaim mechanism cannot help.
/// - `self_responsible` — this process's own
///   [`is_self_responsible`] answer. `None` means the checking symbol
///   itself did not resolve. Per PINV-46's degrade pattern, an
///   unknown answer never forces a respawn. Only a confirmed
///   `Some(false)` does.
fn decide_respawn(
    sentinel_present: bool,
    disclaim_available: bool,
    self_responsible: Option<bool>,
) -> bool {
    if sentinel_present || !disclaim_available {
        return false;
    }
    self_responsible == Some(false)
}

/// Whether `apps/polarize`'s `main` should call
/// [`respawn_self_disclaimed`] before it starts the MCP server.
pub fn should_respawn_disclaimed() -> bool {
    decide_respawn(
        already_respawned(),
        disclaimed_spawn::set_disclaim_fn().is_some(),
        is_self_responsible(),
    )
}

/// Converts a path to the `CString` `posix_spawnp` needs.
fn path_to_cstring(path: &std::path::Path) -> Result<CString, String> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|error| format!("executable path has an embedded NUL byte: {error}"))
}

/// Converts a run of `OsString` arguments (as from
/// [`std::env::args_os`]) into `CString`s `posix_spawnp`'s `argv`
/// needs.
fn os_args_to_cstrings<I>(args: I) -> Result<Vec<CString>, String>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    args.into_iter()
        .map(|arg| {
            CString::new(arg.into_vec())
                .map_err(|error| format!("argument has an embedded NUL byte: {error}"))
        })
        .collect()
}

/// Builds the null-terminated `argv` this process's disclaimed
/// respawn passes to its own executable. That is the executable path
/// itself, followed by every argument this process was started with.
///
/// Every pointer here borrows from `exe`/`args`. The caller must keep
/// them alive, for as long as the returned `Vec` is used.
fn build_respawn_argv(exe: &CString, args: &[CString]) -> Vec<*mut c_char> {
    let mut argv: Vec<*mut c_char> = Vec::with_capacity(args.len() + 2);
    argv.push(exe.as_ptr().cast_mut());
    argv.extend(args.iter().map(|arg| arg.as_ptr().cast_mut()));
    argv.push(ptr::null_mut());
    argv
}

/// Converts a raw `waitpid` status into a process exit code this
/// process can pass to [`std::process::exit`].
///
/// A normal exit reports the child's own exit code (`WEXITSTATUS`). A
/// signal-killed child reports 128 + the signal number —
/// `WIFSIGNALED`/`WTERMSIG`. That is the same convention every POSIX
/// shell uses for `$?` after a signal.
fn exit_code_from_wait_status(status: i32) -> i32 {
    let signal = status & 0x7f;
    if signal == 0 {
        (status >> 8) & 0xff
    } else {
        128 + signal
    }
}

/// Blocks until `pid` exits, then reports its exit code.
///
/// Retries on `EINTR`: [`install_signal_forwarding`]'s own handler
/// runs while this call blocks, and can interrupt it. `EINTR` there
/// means "a signal was forwarded," not "the child is gone." The child
/// is still live. So this loops back into `waitpid`, rather than
/// reporting a spurious error.
///
/// # Safety
/// `pid` must be a real, still-live child of this process, from a
/// `posix_spawnp` call this function's caller just made.
unsafe fn wait_for_child_exit_code(pid: i32) -> Result<i32, String> {
    loop {
        let mut status: i32 = 0;
        // SAFETY: `pid` is a live child pid per this function's
        // contract; `status` is a valid local out-pointer. `0` (no
        // `WNOHANG`) blocks until the child exits or a signal
        // interrupts the call.
        let outcome = unsafe { disclaimed_spawn::waitpid(pid, &mut status, 0) };
        if outcome == pid {
            return Ok(exit_code_from_wait_status(status));
        }
        if outcome == -1 {
            // SAFETY: `__error()` is the documented Darwin way to read
            // `errno` from a plain FFI call site, right after the call
            // that may have set it.
            let error = unsafe { *__error() };
            if error == EINTR {
                continue;
            }
            return Err(format!("waitpid failed: errno {error}"));
        }
        return Err(format!(
            "waitpid returned {outcome}, expected child pid {pid}"
        ));
    }
}

/// Re-execs this process's own executable disclaimed, so the
/// respawned child becomes its own TCC-responsible process (PINV-52).
///
/// The child inherits this process's stdin/stdout/stderr unchanged —
/// no `posix_spawn_file_actions` redirect — so an MCP stdio transport
/// keeps working through it untouched. This process then blocks on
/// the child. On success, it exits with its exit status — it never
/// returns in that case. It returns `Err` only when the respawn could
/// not even be set up. That means no child was ever created. The
/// caller then keeps running the server in this process instead. That
/// is exactly as if no respawn had been attempted.
///
/// Marks [`RESPAWN_SENTINEL_VAR`] on this process's own environment
/// first, via [`std::env::set_var`]. The respawned child's own
/// [`should_respawn_disclaimed`] check then sees it. That way, it does
/// not respawn again. Per that function's own safety contract, this
/// must run before any other thread starts — see this module's own
/// doc comment.
///
/// Installs [`install_signal_forwarding`] before spawning. It records
/// the child's pid for it, once the child exists. A live session
/// confirmed this is not optional. `SIGTERM` sent only to this
/// process's own pid otherwise leaves the respawned child running,
/// orphaned. See [`SigHandler`]'s own doc comment.
pub fn respawn_self_disclaimed() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| format!("current_exe: {error}"))?;
    let exe_c = path_to_cstring(&exe)?;
    let original_args = os_args_to_cstrings(std::env::args_os().skip(1))?;
    let argv = build_respawn_argv(&exe_c, &original_args);

    // SAFETY: this function's own doc comment states the requirement
    // — no other thread has started yet, so nothing can race this
    // write. `apps/polarize/src/main.rs` calls this before building
    // its `tokio` runtime, satisfying it.
    unsafe { std::env::set_var(RESPAWN_SENTINEL_VAR, "1") };

    install_signal_forwarding();

    let mut attr = disclaimed_spawn::init_disclaimed_attr()?;
    let mut pid: i32 = 0;
    // SAFETY: `exe_c` and every `argv` entry stay alive until after
    // this call. `argv` is null-terminated (`build_respawn_argv`). A
    // null `file_actions` is `posix_spawn`'s own documented default:
    // "inherit open file descriptors unchanged." `environ` is this
    // process's real environment, just updated with the sentinel
    // above.
    let spawn_status = unsafe {
        disclaimed_spawn::posix_spawnp(
            &mut pid,
            exe_c.as_ptr(),
            ptr::null(),
            &attr,
            argv.as_ptr(),
            disclaimed_spawn::environ,
        )
    };
    // SAFETY: `attr` was initialized by `init_disclaimed_attr` above.
    unsafe { disclaimed_spawn::posix_spawnattr_destroy(&mut attr) };

    if spawn_status != 0 {
        return Err(format!("posix_spawnp failed: errno {spawn_status}"));
    }
    CHILD_PID.store(pid, Ordering::SeqCst);

    // SAFETY: `pid` is the real child `posix_spawnp` just created.
    let exit_code = unsafe { wait_for_child_exit_code(pid) }?;
    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `responsibility_get_pid_responsible_for_pid` ships on every
    /// real macOS install this crate targets. Confirmed live, against
    /// this session's own dev machine: it is listed in
    /// `/usr/lib/system/libquarantine.tbd`'s exported symbols. This
    /// needs no display and no TCC grant, unlike everything else this
    /// crate touches. So it runs for real in CI. `disclaimed_spawn`'s
    /// and `skylight_ffi`'s own resolution tests rely on the same
    /// reasoning.
    #[test]
    fn responsible_pid_fn_resolves_on_a_real_macos_install() {
        assert!(
            responsible_pid_fn().is_some(),
            "responsibility_get_pid_responsible_for_pid should resolve via dlsym on a real macOS install"
        );
    }

    /// A real, harmless call against this test process's own pid. Every
    /// process has *some* responsible process — even itself — so this
    /// must report `Some`.
    #[test]
    fn responsible_pid_for_self_process_returns_some() {
        let own_pid = std::process::id() as i32;
        assert!(responsible_pid_for(own_pid).is_some());
    }

    /// Idempotency + no-panic check, matching `skylight_ffi`'s and
    /// `disclaimed_spawn`'s own versions of this test.
    #[test]
    fn responsible_pid_fn_is_idempotent_and_does_not_panic() {
        let first = responsible_pid_fn().is_some();
        let second = responsible_pid_fn().is_some();
        assert_eq!(first, second);
    }

    #[test]
    fn sentinel_present_is_false_for_a_missing_env_var() {
        assert!(!sentinel_present(None));
    }

    #[test]
    fn sentinel_present_is_true_for_any_value_including_empty() {
        assert!(sentinel_present(Some(std::ffi::OsStr::new("1"))));
        assert!(sentinel_present(Some(std::ffi::OsStr::new(""))));
    }

    #[test]
    fn decide_respawn_true_only_when_fresh_disclaim_available_and_not_self_responsible() {
        assert!(decide_respawn(false, true, Some(false)));
    }

    #[test]
    fn decide_respawn_false_when_sentinel_already_present() {
        assert!(!decide_respawn(true, true, Some(false)));
    }

    #[test]
    fn decide_respawn_false_when_disclaim_attribute_unavailable() {
        assert!(!decide_respawn(false, false, Some(false)));
    }

    #[test]
    fn decide_respawn_false_when_already_self_responsible() {
        assert!(!decide_respawn(false, true, Some(true)));
    }

    #[test]
    fn decide_respawn_false_when_responsibility_is_undeterminable() {
        assert!(!decide_respawn(false, true, None));
    }

    #[test]
    fn os_args_to_cstrings_converts_plain_arguments() {
        let args = vec![
            std::ffi::OsString::from("--request-permissions"),
            std::ffi::OsString::from("Finder"),
        ];
        let result = os_args_to_cstrings(args).expect("plain args should convert");
        assert_eq!(
            result,
            vec![
                CString::new("--request-permissions").unwrap(),
                CString::new("Finder").unwrap(),
            ]
        );
    }

    #[test]
    fn os_args_to_cstrings_rejects_an_embedded_nul() {
        let bad = std::ffi::OsString::from_vec(vec![b'a', 0, b'b']);
        assert!(os_args_to_cstrings(vec![bad]).is_err());
    }

    #[test]
    fn build_respawn_argv_places_exe_first_and_null_terminates() {
        let exe = CString::new("/path/to/polarize").unwrap();
        let args = vec![CString::new("--foo").unwrap(), CString::new("bar").unwrap()];
        let argv = build_respawn_argv(&exe, &args);
        assert_eq!(argv.len(), 4);
        assert_eq!(argv[0], exe.as_ptr().cast_mut());
        assert_eq!(argv[1], args[0].as_ptr().cast_mut());
        assert_eq!(argv[2], args[1].as_ptr().cast_mut());
        assert!(argv[3].is_null());
    }

    #[test]
    fn build_respawn_argv_with_no_extra_args_is_just_exe_and_null() {
        let exe = CString::new("/path/to/polarize").unwrap();
        let argv = build_respawn_argv(&exe, &[]);
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[0], exe.as_ptr().cast_mut());
        assert!(argv[1].is_null());
    }

    #[test]
    fn exit_code_from_wait_status_reads_a_normal_exit() {
        // WIFEXITED encoding: exit code in bits 8-15, low 7 bits zero.
        assert_eq!(exit_code_from_wait_status(0 << 8), 0);
        assert_eq!(exit_code_from_wait_status(42 << 8), 42);
        assert_eq!(exit_code_from_wait_status(255 << 8), 255);
    }

    #[test]
    fn exit_code_from_wait_status_maps_a_signal_kill_to_128_plus_signal() {
        // SIGTERM = 15, SIGKILL = 9, encoded directly in the low 7 bits.
        assert_eq!(exit_code_from_wait_status(15), 128 + 15);
        assert_eq!(exit_code_from_wait_status(9), 128 + 9);
    }
}
