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

/// Overrides where [`locate_helper`] looks, bypassing the bundle-relative
/// resolution entirely. Set this for a dev build (`target/debug/polarize`
/// has no `Contents/Resources/…` sibling to find) or for a test that
/// wants a stub binary in place of the real Swift helper.
pub const HELPER_PATH_ENV_VAR: &str = "POLARIZE_SETUP_HELPER";

/// Where the helper's executable sits, relative to `Polarize.app`'s
/// `Contents/` directory. Matches the `justfile`'s `bundle-app` recipe
/// (`helper_bundle`).
const HELPER_RELATIVE_TO_CONTENTS: &str =
    "Resources/PolarizeSetupHelper.app/Contents/MacOS/PolarizeSetupHelper";

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

/// `terminate` sends `SIGKILL` (via [`Child::kill`]) and reaps the
/// process with [`Child::wait`]. A plain-window skeleton has no
/// in-progress state a forceful kill could corrupt, so there is no
/// SIGTERM-then-SIGKILL ladder here; a future helper with real UI state
/// to save on exit would need one.
impl HelperChild for HelperProcess {
    fn still_running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
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
            Path::new(
                "/Applications/Polarize.app/Contents/Resources/PolarizeSetupHelper.app/Contents/MacOS/PolarizeSetupHelper"
            )
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
