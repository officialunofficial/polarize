//! [`AppleScriptRunner`] over the `osascript` and `sdef` subprocesses,
//! plus the Automation permission preflight.
//!
//! ## Why a subprocess, and not `NSAppleScript`
//!
//! `objc2-foundation` exposes `NSAppleScript`, so a native binding is
//! possible. `polarize` runs `osascript` in a child process instead.
//! AppleScript can hang forever on a modal dialog, and it leaks memory
//! in the process that hosts it. A child process contains both: the
//! runner kills it at its deadline, and the kernel reclaims everything
//! it held. `polarize` speaks MCP over stdio, so one blocked script
//! inside the server process would stall every later tool call.
//!
//! ## What the source travels on
//!
//! The script goes to `osascript` on stdin, not through `-e`. A long or
//! quote-heavy script through `-e` is a quoting hazard, and a shell-free
//! `Command` still has to build one argument out of it.
//!
//! ## Errors never carry the script
//!
//! No error this module builds holds the script source as written; it
//! passes through [`polarize_core::script::redact_source`] first. A
//! script often carries a password. See PINV-22.
//!
//! ## What is verified here
//!
//! Nothing in this module runs in CI. It is compile-checked only, on
//! `aarch64-apple-darwin`. The pure halves it depends on — the error
//! mapping, the `sdef` scan, the timeout clamp, and the redaction — all
//! live in `polarize-core` and have real unit tests. See the "Testing
//! harness" section of `docs/INVARIANTS.md`.

use std::ffi::c_void;
use std::ptr;
use std::time::Duration;

use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind};
use polarize_core::process;
use polarize_core::schema::AppIdentifier;
use polarize_core::script::{AutomationCheck, automation_check_from_status};
use polarize_core::traits::{AppSdef, AppleScriptRunner, ScriptOutcome};

/// How long `sdef` may take. It reads one bundle and prints XML, so it
/// needs far less time than a script that drives an app.
const SDEF_TIMEOUT_MS: u64 = 10_000;

/// Builds the four-character code Apple Events uses as a type tag.
const fn four_cc(code: &[u8; 4]) -> u32 {
    ((code[0] as u32) << 24) | ((code[1] as u32) << 16) | ((code[2] as u32) << 8) | (code[3] as u32)
}

/// `typeApplicationBundleID` — an address descriptor that names an app
/// by its bundle id.
const TYPE_APPLICATION_BUNDLE_ID: u32 = four_cc(b"bund");

/// `typeWildCard` — "any event class" and "any event id".
const TYPE_WILD_CARD: u32 = four_cc(b"****");

/// Carbon's `AEDesc`. `polarize` only ever passes one back to the
/// Apple Event Manager, so the fields stay opaque here.
#[repr(C)]
struct AEDesc {
    descriptor_type: u32,
    data_handle: *mut c_void,
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    /// Builds an `AEDesc` from raw bytes. Returns `noErr` (0) on
    /// success.
    fn AECreateDesc(
        type_code: u32,
        data_ptr: *const c_void,
        data_size: isize,
        result: *mut AEDesc,
    ) -> i16;

    /// Frees an `AEDesc` built by [`AECreateDesc`].
    fn AEDisposeDesc(desc: *mut AEDesc) -> i16;

    /// Asks whether this process may send Apple Events to `target`.
    /// `ask_user_if_needed` is a `Boolean`, so it is one byte.
    fn AEDeterminePermissionToAutomateTarget(
        target: *const AEDesc,
        event_class: u32,
        event_id: u32,
        ask_user_if_needed: u8,
    ) -> i32;
}

/// Asks macOS whether this process may automate the app with this
/// bundle id.
///
/// The call never prompts: `ask_user_if_needed` is `0`. A "not asked
/// yet" state therefore comes back as its own status code, which
/// [`automation_check_from_status`] maps to
/// [`polarize_core::permission::PermissionState::NotDetermined`]. A
/// descriptor that fails to build leaves the answer
/// [`AutomationCheck::Inconclusive`], so a broken preflight never
/// blocks a script that would otherwise run. See PINV-21.
pub fn automation_check(bundle_id: &str) -> AutomationCheck {
    let bytes = bundle_id.as_bytes();
    let mut descriptor = AEDesc {
        descriptor_type: 0,
        data_handle: ptr::null_mut(),
    };
    // SAFETY: `bytes` outlives the call, and `AECreateDesc` copies the
    // bytes it is given. `descriptor` is a live, writable `AEDesc`.
    let created = unsafe {
        AECreateDesc(
            TYPE_APPLICATION_BUNDLE_ID,
            bytes.as_ptr().cast::<c_void>(),
            bytes.len() as isize,
            &mut descriptor,
        )
    };
    if created != 0 {
        return AutomationCheck::Inconclusive;
    }
    // SAFETY: `descriptor` holds a descriptor `AECreateDesc` just built,
    // and the call only reads it.
    let status = unsafe {
        AEDeterminePermissionToAutomateTarget(&descriptor, TYPE_WILD_CARD, TYPE_WILD_CARD, 0)
    };
    // SAFETY: the descriptor is still live, and nothing uses it after
    // this point.
    unsafe { AEDisposeDesc(&mut descriptor) };
    automation_check_from_status(status)
}

/// Runs one command through [`polarize_core::process`], and shapes its
/// result into the trait's [`ScriptOutcome`].
///
/// The deadline logic lives in `polarize-core` because it is pure
/// `std::process` with no macOS API in it, and because the hang it
/// prevents needs a real subprocess to demonstrate — which
/// `polarize-macos` cannot do in CI. See PINV-25.
///
/// Truncated output folds into `timed_out`. A reader this runner had to
/// abandon means some process outlived the deadline holding the pipe,
/// which is the same fact the caller acts on.
fn run(
    program: &str,
    args: &[&str],
    stdin_data: Option<&str>,
    timeout_ms: u64,
) -> Result<ScriptOutcome, PolarizeError> {
    let outcome = process::run(program, args, stdin_data, Duration::from_millis(timeout_ms))
        .map_err(PolarizeError::Platform)?;
    Ok(ScriptOutcome {
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out || outcome.output_truncated,
    })
}

/// `AppleScriptRunner` implementation over `osascript` and `sdef`.
#[derive(Debug, Default)]
pub struct MacAppleScriptRunner;

impl MacAppleScriptRunner {
    /// Refuses the run when macOS already says this process may not
    /// automate `target_app`.
    ///
    /// The check needs the target's bundle id, so it first resolves the
    /// name against the running apps. An app that is not running gets
    /// no preflight: AppleScript may launch it, and the Apple Event
    /// Manager reports only "no such process" until it does. The
    /// `osascript` error mapping (PINV-21) still catches a refusal in
    /// that case, one step later.
    fn preflight_automation(&self, target_app: &str) -> Result<(), PolarizeError> {
        // The caller may name the app either way, so try both fields.
        // `find_matching_app_index` prefers the bundle id and falls back
        // to the name (PINV-5).
        let identifier = AppIdentifier {
            bundle_id: Some(target_app.to_string()),
            app_name: Some(target_app.to_string()),
        };
        let Ok(running) = crate::window::resolve_running_app(Some(&identifier)) else {
            return Ok(());
        };
        let Some(bundle_id) = running.bundleIdentifier() else {
            return Ok(());
        };
        match automation_check(&bundle_id.to_string()) {
            AutomationCheck::Refused(state) => {
                Err(PolarizeError::Permission(PermissionError::NotGranted {
                    kind: PermissionKind::Automation,
                    state,
                }))
            }
            AutomationCheck::Permitted | AutomationCheck::Inconclusive => Ok(()),
        }
    }
}

/// Shows the system Automation consent dialog for one target app. It
/// reports the resulting permission state. Behind
/// `polarize --request-permissions`; see PINV-44.
///
/// Deliberately does **not** call [`preflight_automation`] first. A
/// `NotDetermined` refusal there is exactly the state this function
/// exists to move past. It also does not call
/// `AEDeterminePermissionToAutomateTarget` with
/// `ask_user_if_needed: true`. That call has a long-standing,
/// Apple-acknowledged hang bug (Apple Developer Forums thread 666528).
/// It is worse against a target that is not already running. A real,
/// harmless script send is what reliably raises the same dialog
/// instead.
///
/// That send goes through
/// [`crate::disclaimed_spawn::send_disclaimed_bootstrap_script`], not a
/// plain `osascript` subprocess. Automation permission is not checked
/// against whichever process literally sends the Apple Event — macOS
/// climbs to the nearest ancestor process it considers "responsible",
/// and neither an interactive shell nor an MCP server's own process
/// tree gives this binary's embedded identity a place to stop that
/// climb. Disclaiming the spawned `osascript`'s responsibility makes it
/// responsible for itself instead. Whether that actually changes which
/// identity macOS checks, or which dialog it raises, is unverified —
/// see `docs/INVARIANTS.md`.
///
/// [`preflight_automation`]: MacAppleScriptRunner::preflight_automation
///
/// How long to give `open -g -a` before the script send below. A
/// freshly-launched app is not always ready to receive an Apple Event
/// the instant `open` returns; one second is a generous, arbitrary
/// margin, not a measured minimum.
const AUTOMATION_BOOTSTRAP_LAUNCH_SETTLE_MS: u64 = 1_000;

pub fn request_automation(target_app_name: &str) -> AutomationCheck {
    // Best-effort: an app already running just gets activated again,
    // and a script send to one that never launches still surfaces as
    // `procNotFound`, which `automation_check` below reports honestly.
    let _ = std::process::Command::new("open")
        .args(["-g", "-a", target_app_name])
        .status();
    std::thread::sleep(Duration::from_millis(AUTOMATION_BOOTSTRAP_LAUNCH_SETTLE_MS));

    let _ = crate::disclaimed_spawn::send_disclaimed_bootstrap_script(target_app_name);

    let identifier = AppIdentifier {
        bundle_id: Some(target_app_name.to_string()),
        app_name: Some(target_app_name.to_string()),
    };
    let Ok(running) = crate::window::resolve_running_app(Some(&identifier)) else {
        return AutomationCheck::Inconclusive;
    };
    let Some(bundle_id) = running.bundleIdentifier() else {
        return AutomationCheck::Inconclusive;
    };
    automation_check(&bundle_id.to_string())
}

impl AppleScriptRunner for MacAppleScriptRunner {
    fn run_script(
        &self,
        source: &str,
        target_app: Option<&str>,
        timeout_ms: u64,
    ) -> Result<ScriptOutcome, PolarizeError> {
        if let Some(app) = target_app {
            self.preflight_automation(app)?;
        }
        // With no file argument and no `-e`, `osascript` reads the whole
        // script from stdin.
        run("osascript", &[], Some(source), timeout_ms)
    }

    fn app_sdef(&self, app: &AppIdentifier) -> Result<AppSdef, PolarizeError> {
        let running = crate::window::resolve_running_app(Some(app))?;
        let app_name = running
            .localizedName()
            .map(|name| name.to_string())
            .unwrap_or_else(|| "unnamed app".to_string());
        let bundle_url = running.bundleURL().ok_or_else(|| {
            PolarizeError::Platform(format!("{app_name} reports no bundle location"))
        })?;
        let bundle_path = bundle_url
            .path()
            .ok_or_else(|| {
                PolarizeError::Platform(format!("{app_name} has a bundle location with no path"))
            })?
            .to_string();

        let outcome = run("sdef", &[bundle_path.as_str()], None, SDEF_TIMEOUT_MS)?;
        if outcome.timed_out {
            return Err(PolarizeError::Platform(format!(
                "sdef timed out after {SDEF_TIMEOUT_MS} ms for {app_name}"
            )));
        }
        if outcome.exit_code != Some(0) {
            let message = outcome.stderr.trim();
            // `sdef` exits non-zero when an app publishes no scripting
            // dictionary at all, which is the common case.
            return Err(PolarizeError::Platform(format!(
                "sdef found no scripting dictionary for {app_name}: {message}"
            )));
        }
        Ok(AppSdef {
            app_name,
            xml: outcome.stdout,
        })
    }

    fn request_automation(&self, target_app_name: &str) -> AutomationCheck {
        request_automation(target_app_name)
    }
}
