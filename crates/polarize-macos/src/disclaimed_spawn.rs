//! Sends the Automation bootstrap script through a `posix_spawn`ed
//! `osascript`.
//!
//! By default, this send inherits `polarize`'s own TCC responsibility.
//! `polarize` is normally self-responsible by the time this runs —
//! PINV-52's disclaimed self-respawn already ran, before any
//! Automation check. A plain child of a self-responsible process
//! inherits that responsibility — confirmed live. So a plain send is
//! enough: the resulting grant lands on `polarize`'s own identity.
//!
//! macOS does not check Automation permission against the process
//! that literally calls the Apple Event API. It walks up to the
//! nearest ancestor process TCC treats as "responsible" for that call.
//! Before PINV-52, that climb landed on whatever launched `polarize`
//! — a shell, or an MCP client — never on `polarize` itself.
//! `responsibility_spawnattrs_setdisclaim` is now only a fallback. It
//! is a private, undocumented `posix_spawnattr_t` flag, resolved here
//! the same way `skylight_ffi.rs` resolves its private symbols.
//! [`should_disclaim_bootstrap_send`] decides when to fall back to it:
//! when `polarize` is not, or not yet, self-responsible. Disclaiming
//! there still helps a little — it detaches the child from an
//! unrelated ancestor. But it lands the grant on `osascript`'s own
//! path, not `polarize`'s. See that function's own doc for the full
//! account.
//!
//! This module exists only for the one-shot bootstrap send
//! (`request_automation`). It does not replace `applescript.rs`'s
//! `run`/`run_with_deadline` path that `run_applescript` and
//! `script_dictionary` use for real calls. This send needs no output
//! capture, just the side effect of the attempt. And, when it works,
//! whatever consent dialog macOS raises as a result.

use std::ffi::{CString, c_void};
use std::os::raw::c_char;
use std::ptr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// `posix_spawnattr_t` and `posix_spawn_file_actions_t` are both
/// `typedef void *` per `<spawn.h>` — plain opaque handles, not
/// structs whose layout this crate would need to reproduce.
///
/// `pub(crate)`: [`crate::self_responsibility`]'s own disclaimed spawn
/// (PINV-52) reuses this type and [`init_disclaimed_attr`] below. That
/// avoids duplicating the disclaim-attribute setup a second time.
pub(crate) type PosixSpawnattrT = *mut c_void;
type PosixFileActionsT = *mut c_void;

/// `O_RDWR`, from `<fcntl.h>`. The only flag this module needs: opening
/// `/dev/null` to redirect a pipe end, never creating a new file.
const O_RDWR: i32 = 0x0002;

/// `WNOHANG`, from `<sys/wait.h>`. Lets the poll loop below check
/// whether the child has exited without blocking on it.
const WNOHANG: i32 = 0x0001;

/// How long this waits for the spawned `osascript` before returning
/// regardless. A real consent dialog needs a human to decide, which
/// can take arbitrarily long — this does not wait for that. It only
/// gives the send itself, and the fast Permitted/already-refused
/// cases, room to finish. A child still running past this point is
/// left running, not killed. Killing it would tear down a dialog the
/// user has not yet had a chance to see.
const SEND_POLL_TIMEOUT: Duration = Duration::from_secs(3);

// `pub(crate)` on the four items `self_responsibility.rs` also calls
// directly, for its own `posix_spawnp` send (PINV-52). These are the
// attr/spawn/wait calls themselves, and the process's real `environ`.
// `posix_spawn_file_actions_*` stay private. Only this module's
// bootstrap send redirects stdio. `self_responsibility`'s respawn
// passes a null `file_actions` instead, to inherit stdio unchanged.
// So it never needs these.
unsafe extern "C" {
    pub(crate) fn posix_spawnattr_init(attr: *mut PosixSpawnattrT) -> i32;
    pub(crate) fn posix_spawnattr_destroy(attr: *mut PosixSpawnattrT) -> i32;
    fn posix_spawn_file_actions_init(actions: *mut PosixFileActionsT) -> i32;
    fn posix_spawn_file_actions_destroy(actions: *mut PosixFileActionsT) -> i32;
    fn posix_spawn_file_actions_addopen(
        actions: *mut PosixFileActionsT,
        fd: i32,
        path: *const c_char,
        oflag: i32,
        mode: u16,
    ) -> i32;
    pub(crate) fn posix_spawnp(
        pid: *mut i32,
        file: *const c_char,
        file_actions: *const PosixFileActionsT,
        attrp: *const PosixSpawnattrT,
        argv: *const *mut c_char,
        envp: *const *mut c_char,
    ) -> i32;
    pub(crate) fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;

    /// The current process's environment, as a null-terminated `char**`.
    /// Always present: every process on Darwin links libSystem, which
    /// defines this.
    pub(crate) static environ: *mut *mut c_char;

    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `dlsym`'s "search every already-loaded image" pseudo-handle, from
/// `<dlfcn.h>`. `ax_ffi.rs` uses the same constant for the same reason:
/// the symbol this resolves lives in a library already linked into
/// every process, so there is nothing to `dlopen`.
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

type SetDisclaimFn = unsafe extern "C" fn(*mut PosixSpawnattrT, i32) -> i32;

/// Resolves `responsibility_spawnattrs_setdisclaim` once and caches the
/// result. `None` means the name did not resolve on this macOS version
/// — every caller must treat that as "cannot disclaim here", not as a
/// bug.
///
/// `pub(crate)`: [`crate::self_responsibility::should_respawn_disclaimed`]
/// checks this resolution too, to decide whether a self-respawn has
/// anything to gain. See PINV-52.
pub(crate) fn set_disclaim_fn() -> Option<SetDisclaimFn> {
    static CACHE: OnceLock<Option<usize>> = OnceLock::new();
    let address = *CACHE.get_or_init(|| {
        let name = c"responsibility_spawnattrs_setdisclaim";
        // SAFETY: `RTLD_DEFAULT` and `name` are both valid for the
        // duration of this call; `name` is a `'static` C string.
        let symbol = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
        (!symbol.is_null()).then_some(symbol as usize)
    });
    // SAFETY: `address`, when present, came from a real `dlsym` result
    // for this exact symbol name, immediately above.
    address.map(|address| unsafe { std::mem::transmute::<usize, SetDisclaimFn>(address) })
}

/// Initializes a plain `posix_spawnattr_t`, with no disclaim flag set.
/// The caller owns `posix_spawnattr_destroy`ing it once its
/// `posix_spawnp` call is done with it.
fn init_attr() -> Result<PosixSpawnattrT, String> {
    let mut attr: PosixSpawnattrT = ptr::null_mut();
    // SAFETY: `attr` is a valid local out-pointer.
    if unsafe { posix_spawnattr_init(&mut attr) } != 0 {
        return Err("posix_spawnattr_init failed".to_string());
    }
    Ok(attr)
}

/// Initializes a `posix_spawnattr_t` and applies the disclaim flag on
/// it when `responsibility_spawnattrs_setdisclaim` resolves.
///
/// Returns a valid, non-disclaimed attr when the symbol does not
/// resolve. A caller still spawns with it either way, just without
/// retargeting the child's responsible process.
///
/// `pub(crate)`: shared between [`should_disclaim_bootstrap_send`]'s
/// fallback path below and
/// [`crate::self_responsibility::respawn_self_disclaimed`]'s own send,
/// so the disclaim-attribute setup lives in exactly one place. See
/// PINV-52.
pub(crate) fn init_disclaimed_attr() -> Result<PosixSpawnattrT, String> {
    let mut attr = init_attr()?;
    if let Some(set_disclaim) = set_disclaim_fn() {
        // SAFETY: `attr` was just initialized above. The return value
        // only reports whether this specific attribute could be set.
        // A failure here still leaves a valid (non-disclaimed) `attr`
        // to spawn with.
        let _ = unsafe { set_disclaim(&mut attr, 1) };
    }
    Ok(attr)
}

/// Pure decision: should the bootstrap send disclaim its spawned
/// `osascript` child, or send plainly?
///
/// A disclaimed child becomes its own responsible process.
/// `osascript` has no bundle, so that lands the grant on its own path
/// — `/usr/bin/osascript`. That is a shared identity every other
/// disclaimed script on the machine collides with. It is never
/// `polarize`'s own.
///
/// A plain child instead inherits responsibility from its parent.
/// Once `polarize` itself is self-responsible, a plain child inherits
/// *that* instead. PINV-52's disclaimed self-respawn already ran, by
/// the time this send happens. Confirmed live — see PINV-51's own
/// follow-up in `docs/INVARIANTS.md`.
///
/// So: disclaim only as a fallback. That covers `polarize` not being
/// self-responsible yet — or ever, if the respawn's own symbol never
/// resolved. `None` — undeterminable — degrades to the same fallback,
/// per PINV-46's pattern.
fn should_disclaim_bootstrap_send(self_responsible: Option<bool>) -> bool {
    self_responsible != Some(true)
}

/// Sends `tell application "<target_app_name>" to count of windows`
/// through an `osascript` child, to raise `target_app_name`'s
/// Automation consent dialog if one has not been shown before.
/// Disclaims that child's own TCC responsibility only as a fallback —
/// see [`should_disclaim_bootstrap_send`].
///
/// `count of windows`, not `get its name`. A live session confirmed
/// this matters. `get its name` is answerable from the addressing
/// term alone. It needs no real round-trip to the target app. So it
/// never touches `kTCCServiceAppleEvents` at all — see PINV-51's own
/// follow-up in `docs/INVARIANTS.md`. `count of windows` is in
/// AppleScript's Standard Suite, which nearly every scriptable app
/// implements. Answering it genuinely needs the target app itself, so
/// it does reach the real permission check.
///
/// Returns once the send itself completes, the fast paths (already
/// permitted or already refused) resolve, or [`SEND_POLL_TIMEOUT`]
/// elapses — whichever comes first. It does not wait for a human to
/// decide a consent dialog: a caller checks
/// [`crate::applescript::automation_check`] afterward, and inspects the
/// screen for a dialog through the normal GUI-automation tools if the
/// state is still undetermined.
///
/// Falls back to running without the disclaim flag when
/// `responsibility_spawnattrs_setdisclaim` does not resolve — the send
/// still happens, just without retargeting its responsible process.
pub fn send_bootstrap_script(target_app_name: &str) -> Result<(), String> {
    let script = format!("tell application \"{target_app_name}\" to count of windows");
    let script_path = write_script_to_temp_file(&script)?;

    let program =
        CString::new("osascript").map_err(|error| format!("bad program name: {error}"))?;
    let script_path_c =
        CString::new(script_path.as_str()).map_err(|error| format!("bad script path: {error}"))?;
    let dev_null =
        CString::new("/dev/null").map_err(|error| format!("bad /dev/null path: {error}"))?;

    let self_responsible = crate::self_responsibility::is_self_responsible();
    let mut attr = if should_disclaim_bootstrap_send(self_responsible) {
        init_disclaimed_attr()?
    } else {
        init_attr()?
    };
    let mut file_actions: PosixFileActionsT = ptr::null_mut();

    // SAFETY: every pointer passed below is either a live local (`attr`,
    // `file_actions`) or a `CString`/`static` kept alive for the whole
    // call. `posix_spawnp` is given a valid, null-terminated `argv` and
    // the process's own real `environ`.
    let result = unsafe {
        if posix_spawn_file_actions_init(&mut file_actions) != 0 {
            posix_spawnattr_destroy(&mut attr);
            return Err("posix_spawn_file_actions_init failed".to_string());
        }
        // Redirects stdin/stdout/stderr to `/dev/null`. This process's
        // real stdout/stderr may be an MCP stdio transport; the spawned
        // child must never inherit those descriptors directly.
        posix_spawn_file_actions_addopen(&mut file_actions, 0, dev_null.as_ptr(), O_RDWR, 0);
        posix_spawn_file_actions_addopen(&mut file_actions, 1, dev_null.as_ptr(), O_RDWR, 0);
        posix_spawn_file_actions_addopen(&mut file_actions, 2, dev_null.as_ptr(), O_RDWR, 0);

        let argv: [*mut c_char; 3] = [
            program.as_ptr().cast_mut(),
            script_path_c.as_ptr().cast_mut(),
            ptr::null_mut(),
        ];

        let mut pid: i32 = 0;
        let spawn_status = posix_spawnp(
            &mut pid,
            program.as_ptr(),
            &file_actions,
            &attr,
            argv.as_ptr(),
            environ,
        );

        posix_spawn_file_actions_destroy(&mut file_actions);
        posix_spawnattr_destroy(&mut attr);

        if spawn_status != 0 {
            Err(format!("posix_spawnp failed: errno {spawn_status}"))
        } else {
            wait_briefly(pid);
            Ok(())
        }
    };

    let _ = std::fs::remove_file(&script_path);
    result
}

/// Polls a child with `WNOHANG` until it exits or
/// [`SEND_POLL_TIMEOUT`] passes. A child still running past the
/// deadline is left alone — see [`SEND_POLL_TIMEOUT`]'s own doc for
/// why killing it would be wrong.
///
/// # Safety
/// `pid` must be a real child of this process, from a `posix_spawn*`
/// call this function's caller just made.
unsafe fn wait_briefly(pid: i32) {
    let deadline = Instant::now() + SEND_POLL_TIMEOUT;
    loop {
        let mut status: i32 = 0;
        // SAFETY: `pid` is a live child pid per this function's
        // contract; `status` is a valid local to write into.
        let outcome = unsafe { waitpid(pid, &mut status, WNOHANG) };
        if outcome == pid || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Writes `script` to a fresh temp file and returns its path.
/// `osascript <path>` avoids needing a stdin pipe, which would need
/// its own `posix_spawn_file_actions` wiring for a send this module
/// otherwise keeps deliberately minimal.
fn write_script_to_temp_file(script: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!(
        "polarize-automation-bootstrap-{}.applescript",
        std::process::id()
    ));
    std::fs::write(&path, script).map_err(|error| format!("cannot write temp script: {error}"))?;
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| "temp script path is not valid UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `responsibility_spawnattrs_setdisclaim` ships on every real
    /// macOS install (confirmed via prior research against LLVM's own
    /// use of it). This needs no display and no TCC grant, unlike
    /// everything else this crate touches, so it runs for real in CI.
    #[test]
    fn set_disclaim_fn_resolves_on_a_real_macos_install() {
        assert!(
            set_disclaim_fn().is_some(),
            "responsibility_spawnattrs_setdisclaim should resolve via dlsym on a real macOS install"
        );
    }

    /// `symbols()`-style idempotency check: calling this twice must
    /// resolve to the same answer, and never panic.
    #[test]
    fn set_disclaim_fn_is_idempotent_and_does_not_panic() {
        let first = set_disclaim_fn().is_some();
        let second = set_disclaim_fn().is_some();
        assert_eq!(first, second);
    }

    #[test]
    fn should_disclaim_bootstrap_send_false_when_confirmed_self_responsible() {
        assert!(!should_disclaim_bootstrap_send(Some(true)));
    }

    #[test]
    fn should_disclaim_bootstrap_send_true_when_confirmed_not_self_responsible() {
        assert!(should_disclaim_bootstrap_send(Some(false)));
    }

    #[test]
    fn should_disclaim_bootstrap_send_true_when_undeterminable() {
        assert!(should_disclaim_bootstrap_send(None));
    }

    /// A real, harmless send. This runs for real in CI: it does not
    /// need a display or a TCC grant to exercise the `posix_spawn`
    /// path — `osascript`'s own scripting-error path (a bogus target)
    /// still proves this function's plumbing runs it and returns.
    #[test]
    fn send_bootstrap_script_does_not_panic_or_hang() {
        let result = send_bootstrap_script("PolarizeDefinitelyNotARealAppXYZ123");
        assert!(result.is_ok(), "{result:?}");
    }
}
