//! Locating and spawning `PolarizeSetupHelper`, and the real
//! `std::process::Child`'s [`HelperChild`] implementation the
//! `polarize-core` wait loop drives (PLZ-5).
//!
//! This binary is spawned directly with `std::process::Command`, never
//! through `open`/`NSWorkspace`. `open` hands the process to `launchd`,
//! and it stops being this process's child — which would break
//! PINV-64's "the parent always terminates its own helper child"
//! guarantee. The helper's own skeleton sets `.regular` activation
//! policy and activates itself (`apps/setup-helper/Sources/…/main.swift`),
//! so a direct exec still raises a visible window.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use polarize_core::bootstrap::HelperChild;

/// `SIGUSR1`'s numeric value on Darwin (`<sys/signal.h>`). Sent by
/// [`HelperProcess::notify_all_granted`] as a courtesy "the parent's
/// read says you're done" signal (PLZ-9) — never a substitute for
/// [`HelperProcess::terminate`]'s `SIGKILL`, which still runs
/// unconditionally after the grace sleep in
/// `polarize_core::bootstrap::wait_for_grants_or_close`. `SIGUSR1`'s
/// default disposition is also "terminate the process," so a stale
/// helper binary with no handler installed for it still dies rather
/// than hanging around.
const SIGUSR1: i32 = 30;

// No `libc` dependency exists in this crate (`self_responsibility.rs`'s
// own module doc comment explains why every module that needs a raw
// libSystem call declares it locally rather than sharing one). `kill`
// is a plain, zero-cost link against libSystem, exactly like that
// module's own `kill` declaration — this is a separate declaration in
// a separate module, not a duplicate symbol.
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Overrides where [`locate_helper`] looks, bypassing the bundle-relative
/// resolution entirely. Set this for a dev build (`target/debug/polarize`
/// has no `Contents/Resources/…` sibling to find) or for a test that
/// wants a stub binary in place of the real Swift helper.
pub const HELPER_PATH_ENV_VAR: &str = "POLARIZE_SETUP_HELPER";

/// Where the helper's executable sits, relative to `Polarize.app`'s
/// `Contents/` directory. Matches the `justfile`'s `bundle-app` recipe
/// (`helper_bin`) — a loose executable sitting directly beside
/// `polarize` in `Contents/MacOS/`, not a nested `.app` bundle. Both
/// binaries are signed with the same `CFBundleIdentifier`, so both
/// read as one app, `Polarize.app`, to LaunchServices, TCC, and
/// System Settings.
const HELPER_RELATIVE_TO_CONTENTS: &str = "MacOS/PolarizeSetupHelper";

/// Why [`locate_helper`] could not find a helper binary to spawn.
///
/// Every variant is a graceful, non-fatal condition from the caller's
/// point of view: `apps/polarize` warns and falls through to its final
/// report (AC 4 in PLZ-5) rather than treating any of these as reason to
/// abort `--request-permissions` itself.
#[derive(Debug)]
pub enum LocateHelperError {
    /// `std::env::current_exe()` itself failed.
    CurrentExe(std::io::Error),
    /// This process is not running from inside a `.../Contents/MacOS/`
    /// directory — e.g. a bare `cargo run`/`target/debug/polarize`
    /// build with no bundle around it, and
    /// [`HELPER_PATH_ENV_VAR`] was not set either.
    NotBundled,
    /// A path was named or resolved, but nothing executable exists
    /// there.
    NotFound(PathBuf),
}

impl std::fmt::Display for LocateHelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocateHelperError::CurrentExe(err) => write!(f, "current_exe: {err}"),
            LocateHelperError::NotBundled => write!(
                f,
                "not running from inside a Contents/MacOS directory, and \
                 {HELPER_PATH_ENV_VAR} is not set"
            ),
            LocateHelperError::NotFound(path) => {
                write!(f, "no helper binary at {}", path.display())
            }
        }
    }
}

impl std::error::Error for LocateHelperError {}

/// The pure half of [`locate_helper`]: given this process's own
/// executable path, resolves where the helper *would* sit inside the
/// same bundle — without touching the filesystem. `None` when `exe`
/// does not sit inside a `Contents/MacOS/` directory at all.
///
/// Kept separate from [`locate_helper`] so this resolution can be
/// unit-tested against made-up paths, with no real bundle on disk.
fn resolve_from_exe_path(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    Some(contents_dir.join(HELPER_RELATIVE_TO_CONTENTS))
}

/// The pure half of [`own_bundle_path`]: given this process's own
/// executable path, resolves `Polarize.app` itself — the bundle this
/// exe runs inside, not the helper's. `None` when `exe` does not sit
/// inside a `<name>.app/Contents/MacOS/` directory at all, e.g. a bare
/// `target/debug/polarize` dev build.
///
/// Kept separate from [`own_bundle_path`] so this resolution can be
/// unit-tested against made-up paths, with no real bundle on disk. See
/// PINV-59: the drag payload must always name Polarize's own bundle,
/// never the helper's — this is the one place that bundle path gets
/// resolved, from the running Polarize process's own location.
fn own_bundle_from_exe_path(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name()? != "Contents" {
        return None;
    }
    let bundle_dir = contents_dir.parent()?;
    let name = bundle_dir.file_name()?.to_str()?;
    if !name.ends_with(".app") {
        return None;
    }
    Some(bundle_dir.to_path_buf())
}

/// Resolves `Polarize.app`'s own bundle path from
/// `std::env::current_exe()`, for the setup helper's `--for-bundle`
/// argv flag (PINV-59). Returns `None` for an unbundled dev run — the
/// helper's drag view then simply does not render, which is
/// safe-by-default rather than guessing at a bundle path.
pub fn own_bundle_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    own_bundle_from_exe_path(&exe)
}

/// Finds `PolarizeSetupHelper`'s executable.
///
/// [`HELPER_PATH_ENV_VAR`], when set, always wins. Otherwise this
/// resolves relative to `std::env::current_exe()`, matching the real
/// `Polarize.app` layout `justfile`'s `bundle-app` recipe produces.
pub fn locate_helper() -> Result<PathBuf, LocateHelperError> {
    if let Ok(overridden) = std::env::var(HELPER_PATH_ENV_VAR) {
        let path = PathBuf::from(overridden);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(LocateHelperError::NotFound(path))
        };
    }

    let exe = std::env::current_exe().map_err(LocateHelperError::CurrentExe)?;
    let candidate = resolve_from_exe_path(&exe).ok_or(LocateHelperError::NotBundled)?;
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(LocateHelperError::NotFound(candidate))
    }
}

/// Spawns the helper directly (never through `open`/`NSWorkspace` — see
/// the module doc comment), with its stdio nulled so the helper's own
/// output never mixes into `--request-permissions`'s terminal report.
/// Returns a [`HelperProcess`], ready for
/// `polarize_core::bootstrap::wait_for_grants_or_close`.
pub fn spawn_helper(helper_exe: &Path, args: &[String]) -> std::io::Result<HelperProcess> {
    Command::new(helper_exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(HelperProcess)
}

/// A real spawned helper child process.
///
/// Rust's orphan rule blocks implementing `polarize_core`'s
/// [`HelperChild`] directly on `std::process::Child` (a foreign trait,
/// foreign type) — this newtype is the local type that lets
/// `polarize-macos` provide the real implementation.
#[derive(Debug)]
pub struct HelperProcess(pub Child);

/// The SIGUSR1-then-SIGKILL ladder PLZ-9 adds: `notify_all_granted`
/// sends `SIGUSR1`, a courtesy notification the helper can use to show
/// a success frame; `terminate` still sends `SIGKILL` (via
/// [`Child::kill`]) and reaps the process with [`Child::wait`]
/// unconditionally, after `polarize_core::bootstrap`'s grace sleep. A
/// plain-window skeleton has no in-progress state a forceful kill could
/// corrupt, and now that a real UI has a way to react first, there is
/// still no true SIGTERM-then-SIGKILL ladder here — `SIGUSR1` only ever
/// buys the helper a bounded head start, never a veto, over the
/// `SIGKILL` that always follows.
impl HelperChild for HelperProcess {
    fn still_running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }

    fn notify_all_granted(&mut self) {
        // Guarded by `still_running`: `try_wait` only reaps on an
        // actual exit, so an unreaped child's pid cannot have been
        // recycled by the OS for an unrelated process yet. A child
        // that already exited on its own gets no signal — there is
        // nothing left to notify.
        if self.still_running() {
            // SAFETY: `kill` is `<signal.h>`'s well-known libSystem
            // function; `self.0.id()` is a real pid this process just
            // spawned and confirmed (via `still_running`, above) is
            // still live. A signal send to a live pid this process
            // owns has no further precondition.
            unsafe {
                kill(self.0.id() as i32, SIGUSR1);
            }
        }
    }

    fn terminate(&mut self) {
        // `kill` on an already-exited child returns `Err` (`InvalidInput`
        // on this platform) — harmless, there is nothing left to kill.
        // `wait` on one that already exited returns its cached exit
        // status immediately rather than blocking.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- resolve_from_exe_path (pure) --------------------------------

    #[test]
    fn resolve_from_exe_path_finds_the_helper_next_to_a_bundled_binary() {
        let exe = Path::new("/Applications/Polarize.app/Contents/MacOS/polarize");
        let resolved = resolve_from_exe_path(exe).expect("bundled layout must resolve");
        assert_eq!(
            resolved,
            Path::new("/Applications/Polarize.app/Contents/MacOS/PolarizeSetupHelper")
        );
    }

    #[test]
    fn resolve_from_exe_path_returns_none_for_a_bare_dev_binary() {
        let exe = Path::new("/Users/dev/polarize/target/debug/polarize");
        assert_eq!(resolve_from_exe_path(exe), None);
    }

    #[test]
    fn resolve_from_exe_path_returns_none_when_the_exe_has_no_parent() {
        let exe = Path::new("polarize");
        assert_eq!(resolve_from_exe_path(exe), None);
    }

    // ---- own_bundle_from_exe_path (pure) ------------------------------

    #[test]
    fn own_bundle_from_exe_path_finds_polarize_app_from_its_bundled_binary() {
        let exe = Path::new("/Applications/Polarize.app/Contents/MacOS/polarize");
        assert_eq!(
            own_bundle_from_exe_path(exe),
            Some(PathBuf::from("/Applications/Polarize.app"))
        );
    }

    #[test]
    fn own_bundle_from_exe_path_returns_none_for_a_bare_dev_binary() {
        let exe = Path::new("/Users/dev/polarize/target/debug/polarize");
        assert_eq!(own_bundle_from_exe_path(exe), None);
    }

    #[test]
    fn own_bundle_from_exe_path_returns_none_when_the_grandparent_does_not_end_in_dot_app() {
        let exe = Path::new("/Applications/NotAnApp/Contents/MacOS/polarize");
        assert_eq!(own_bundle_from_exe_path(exe), None);
    }

    #[test]
    fn own_bundle_from_exe_path_returns_none_when_the_exe_has_no_parent() {
        let exe = Path::new("polarize");
        assert_eq!(own_bundle_from_exe_path(exe), None);
    }

    // ---- locate_helper (env override path) ---------------------------

    /// Guards every test in this module that touches
    /// `HELPER_PATH_ENV_VAR`: `std::env::set_var`/`remove_var` are
    /// process-global, so concurrent `cargo test` threads touching the
    /// same variable would race without this.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn locate_helper_uses_the_env_override_when_it_points_at_a_real_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let stub =
            std::env::temp_dir().join(format!("polarize-setup-helper-stub-{}", std::process::id()));
        std::fs::write(&stub, b"#!/bin/sh\nexit 0\n").expect("write stub helper");

        // SAFETY: serialized by `ENV_LOCK`, and restored before the
        // guard drops.
        unsafe { std::env::set_var(HELPER_PATH_ENV_VAR, &stub) };
        let result = locate_helper();
        unsafe { std::env::remove_var(HELPER_PATH_ENV_VAR) };
        let _ = std::fs::remove_file(&stub);

        assert_eq!(result.unwrap(), stub);
    }

    #[test]
    fn locate_helper_reports_not_found_when_the_env_override_points_nowhere() {
        let _guard = ENV_LOCK.lock().unwrap();
        let missing = std::env::temp_dir().join("polarize-setup-helper-does-not-exist");

        // SAFETY: serialized by `ENV_LOCK`, and restored before the
        // guard drops.
        unsafe { std::env::set_var(HELPER_PATH_ENV_VAR, &missing) };
        let result = locate_helper();
        unsafe { std::env::remove_var(HELPER_PATH_ENV_VAR) };

        assert!(matches!(result, Err(LocateHelperError::NotFound(_))));
    }

    // ---- HelperChild over a real process ------------------------------

    /// PINV-64's own testable seam: a real, direct-spawned child gets
    /// killed and reaped, never left running or left as a zombie.
    #[test]
    fn terminate_kills_and_reaps_a_real_child_process_that_never_exits_on_its_own() {
        let mut child = HelperProcess(
            Command::new("/bin/sleep")
                .arg("1000")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn /bin/sleep"),
        );

        assert!(
            child.still_running(),
            "a freshly spawned long sleep must still be running"
        );

        child.terminate();

        assert!(
            !child.still_running(),
            "terminate must leave the child not running"
        );
    }

    /// PLZ-9's own testable seam: `notify_all_granted` sends `SIGUSR1`,
    /// whose default disposition is "terminate the process" — so a
    /// real child with no handler installed for it (exactly what a
    /// stale helper binary, or a plain `/bin/sleep` stand-in, looks
    /// like) exits from the signal alone. `terminate` afterward is then
    /// a no-op, same as it is for any already-exited child.
    #[test]
    fn notify_all_granted_signals_a_real_child_that_has_no_handler_installed() {
        let mut child = HelperProcess(
            Command::new("/bin/sleep")
                .arg("1000")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn /bin/sleep"),
        );

        assert!(
            child.still_running(),
            "a freshly spawned long sleep must still be running"
        );

        child.notify_all_granted();

        // Give the signal a moment to actually land before checking —
        // `still_running` calls `try_wait`, which does not block.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while child.still_running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert!(
            !child.still_running(),
            "SIGUSR1's default disposition must have terminated the child"
        );

        // `terminate` on an already-exited child is a documented no-op.
        child.terminate();
        assert!(!child.still_running());
    }

    #[test]
    fn notify_all_granted_is_a_no_op_on_a_child_that_already_exited() {
        let mut child = HelperProcess(
            Command::new("/usr/bin/true")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn /usr/bin/true"),
        );
        let _ = child.0.wait();

        // Must not panic or attempt to signal a pid that may have been
        // recycled by the OS for an unrelated process.
        child.notify_all_granted();

        assert!(!child.still_running());
    }

    #[test]
    fn terminate_is_a_no_op_on_a_child_that_already_exited() {
        let mut child = HelperProcess(
            Command::new("/usr/bin/true")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn /usr/bin/true"),
        );
        // Give it a moment to actually exit before terminate runs.
        let _ = child.0.wait();

        child.terminate();

        assert!(!child.still_running());
    }
}
