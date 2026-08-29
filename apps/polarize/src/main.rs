//! `polarize`: a stdio MCP server that automates real native macOS AppKit
//! applications.
//!
//! This binary is a thin wiring layer: MCP tool calls delegate to
//! `polarize-core` (pure logic) and `polarize-macos` (real framework
//! calls) — see [`server::PolarizeServer`]. It carries minimal logic of
//! its own: constructing the server, attaching it to the stdio
//! transport, and running it until the client disconnects.
//!
//! ## Why this is not `#[tokio::main]`
//!
//! `#[tokio::main]` parks the real OS main thread inside tokio's own
//! executor. `NSWorkspace`'s `runningApplications` and
//! `frontmostApplication` only refresh on a turn of that thread's
//! `CFRunLoop`. Apple documents this policy for `NSRunningApplication`.
//! See `polarize_macos::runloop` for the mechanism. This binary
//! therefore builds its own `tokio` runtime. It runs the MCP server on
//! that runtime. That frees the real main thread, which then runs
//! [`polarize_macos::runloop::run_main_until_stopped`] — see PINV-42.

mod server;

use std::sync::mpsc;

use rmcp::ServiceExt;
use rmcp::transport::stdio;

use server::PolarizeServer;

/// Sets up `tracing`: plain text to stderr, level controlled by
/// `RUST_LOG`, `info` by default.
///
/// Stderr, never stdout. Stdout carries the MCP JSON-RPC stream
/// itself; any other byte on it corrupts the protocol. Enabling span
/// close events means every `#[tracing::instrument]`-annotated tool
/// call in `server.rs` logs its own duration for free.
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .init();
}

/// Logs one line per private `SkyLight.framework` symbol, naming it and
/// whether it resolved on this machine. This makes the fallback state
/// PINV-46 requires observable without a debugger.
///
/// `polarize-macos` does not depend on `tracing` itself. This binary
/// already owns the tracing setup, so it reads
/// [`SkylightSymbols::resolution_summary`](polarize_macos::skylight_ffi::SkylightSymbols::resolution_summary)
/// and logs it here instead.
fn log_skylight_symbol_resolution() {
    for (symbol, resolved) in polarize_macos::skylight_ffi::symbols().resolution_summary() {
        tracing::info!(symbol, resolved, "SkyLight symbol resolution");
    }
}

/// Logs this process's own TCC "responsible process" state. This
/// makes PINV-52 observable without a debugger. It plays the same
/// role `log_skylight_symbol_resolution` plays for PINV-46.
fn log_responsibility_state() {
    tracing::info!(
        state = %polarize_macos::self_responsibility::responsibility_summary(),
        "TCC responsible-process state"
    );
}

/// Logs whether this process's own code signature can see the shared
/// App Group container. This makes PINV-52's still-open question
/// about `polarize`'s own entitlements observable without a debugger.
/// It plays the same role `log_responsibility_state` plays for the
/// responsible-process question.
fn log_app_group_state() {
    tracing::info!(
        state = %polarize_macos::app_group::container_summary(),
        "shared App Group container state"
    );
}

/// Respawns this process disclaimed when
/// [`polarize_macos::self_responsibility::should_respawn_disclaimed`]
/// says it is warranted. So `polarize` becomes its own TCC-responsible
/// process, instead of inheriting whatever launched it (PINV-52).
///
/// A successful respawn never returns. The respawned child inherits
/// this process's stdio. This process blocks on it, then exits with
/// its exit status. This only returns when no respawn was warranted.
/// It also returns when the respawn's own setup failed, before a
/// child ever existed. The caller then keeps running the MCP server
/// in this process. That is exactly as if no respawn had been
/// attempted.
fn respawn_disclaimed_if_warranted() {
    if !polarize_macos::self_responsibility::should_respawn_disclaimed() {
        return;
    }
    tracing::info!("respawning disclaimed to become our own TCC-responsible process");
    if let Err(err) = polarize_macos::self_responsibility::respawn_self_disclaimed() {
        tracing::warn!("disclaimed self-respawn failed, continuing in this process: {err}");
    }
}

fn main() {
    init_tracing();

    // Must happen before any other thread starts anywhere in this
    // process — see `polarize_macos::self_responsibility`'s own doc
    // comment for why. This runs before the `--request-permissions`
    // branch below too, not just the server path. PINV-52's own
    // follow-up note explains why both need it. A successful respawn
    // re-execs with the same argv. So `--request-permissions` still
    // sees its own flag and target. It just runs from the
    // now-self-responsible child.
    log_responsibility_state();
    log_app_group_state();
    respawn_disclaimed_if_warranted();

    if std::env::args().nth(1).as_deref() == Some("--request-permissions") {
        let automation_target = std::env::args()
            .nth(2)
            .unwrap_or_else(|| AUTOMATION_BOOTSTRAP_TARGET.to_string());
        request_permissions(&automation_target);
        return;
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!("{err}");
            std::process::exit(1);
        }
    };

    // Must happen on this thread, before the run loop starts: see
    // `polarize_macos::workspace_activation`.
    polarize_macos::workspace_activation::activate();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");
    log_skylight_symbol_resolution();

    let (result_tx, result_rx) = mpsc::channel::<Result<(), String>>();
    let server_task = runtime.spawn(async { run().await.map_err(|err| err.to_string()) });
    // A separate task, not the server task itself: a panic inside
    // `run()` would otherwise stay inside `server_task`'s `JoinHandle`
    // forever unread, and `run_main_until_stopped` would then block on
    // a run loop nothing will ever stop.
    runtime.spawn(async move {
        let result = match server_task.await {
            Ok(result) => result,
            Err(join_err) => Err(format!("the MCP server task panicked: {join_err}")),
        };
        let _ = result_tx.send(result);
        polarize_macos::runloop::stop_main();
    });

    polarize_macos::runloop::run_main_until_stopped();

    // The server task always sends before it stops the run loop, so
    // this is only ever empty if the channel itself was dropped
    // without sending — nothing left to report in that case.
    match result_rx.try_recv() {
        Ok(Err(err)) => {
            tracing::error!("{err}");
            std::process::exit(1);
        }
        Ok(Ok(())) => tracing::info!("stdio client disconnected, shutting down"),
        Err(_) => {}
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let server = PolarizeServer::default();
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

/// The default bootstrap target when `--request-permissions` names
/// none. Every Mac has Finder, so this always resolves.
const AUTOMATION_BOOTSTRAP_TARGET: &str = "Finder";

/// One-time setup: shows the system Accessibility, Screen Recording,
/// and Automation consent alerts for this exact binary, so their
/// grants survive future `just build` re-signs. Not part of the MCP
/// server's own startup path — see `polarize_macos::permission_bootstrap`
/// for why this exists.
///
/// Automation is granted per (this process, target app) pair — see
/// PINV-44. `run_applescript` itself never raises a new target's
/// consent dialog: `preflight_automation` (`applescript.rs`) checks
/// with `ask_user_if_needed: false` and returns an error before
/// `osascript` ever runs, so a script aimed at an unauthorized app
/// always refuses locally, silently, with no dialog. `automation_target`
/// names which app this call bootstraps; run this once per target app
/// `run_applescript` needs to reach, e.g.
/// `./target/debug/polarize --request-permissions Messages`.
///
/// # PINV-57
///
/// The three real-prompt calls below always run first, for every
/// permission, before this function may even consider launching the
/// guided helper. See `polarize_core::bootstrap::needed_permissions`.
fn request_permissions(automation_target: &str) {
    println!("Requesting Accessibility permission...");
    let accessibility = polarize_macos::permission_bootstrap::request_accessibility();
    println!("  Accessibility trusted: {accessibility}");

    println!("Requesting Screen Recording permission...");
    let screen_recording = polarize_macos::permission_bootstrap::request_screen_recording();
    println!("  Screen Recording trusted: {screen_recording}");

    println!("Requesting Automation permission for {automation_target}...");
    let automation = polarize_macos::permission_bootstrap::request_automation(automation_target);
    println!("  Automation ({automation_target}): {automation:?}");

    let needed = polarize_core::bootstrap::needed_permissions(
        accessibility,
        screen_recording,
        automation,
        automation_target,
    );

    if needed.is_empty() {
        // Every real prompt already succeeded — nothing to hand off to
        // the helper. See PINV-57's fast path.
        return;
    }

    launch_helper_and_wait(automation_target, needed);
}

/// Locates and spawns the guided permission helper for `needed`, waits
/// for grants (or the helper's own exit, or a deadline) via
/// `polarize_core::bootstrap::wait_for_grants_or_close`, and prints the
/// final report from that same wait's last read (PINV-65).
///
/// A helper that cannot be located or spawned is a graceful,
/// non-fatal condition (AC 4): this warns and falls straight through to
/// the final report, built from one more non-prompting re-read, instead
/// of aborting `--request-permissions` itself.
fn launch_helper_and_wait(
    automation_target: &str,
    needed: Vec<polarize_core::bootstrap::NeededPermission>,
) {
    let helper_exe = match polarize_macos::setup_helper::locate_helper() {
        Ok(path) => path,
        Err(err) => {
            println!("\nCould not locate the guided permission helper: {err}");
            print_current_report(automation_target);
            return;
        }
    };

    let own_bundle = polarize_macos::setup_helper::own_bundle_path();
    let own_bundle_arg = own_bundle.as_deref().and_then(|path| path.to_str());
    let args = polarize_core::bootstrap::helper_args(&needed, own_bundle_arg);
    let permission_names = needed
        .iter()
        .map(describe_needed_permission)
        .collect::<Vec<_>>()
        .join(", ");

    let mut child = match polarize_macos::setup_helper::spawn_helper(&helper_exe, &args) {
        Ok(child) => {
            println!(
                "\nStill missing: {permission_names}. Opening the guided setup helper \
                 ({})...",
                helper_exe.display()
            );
            child
        }
        Err(err) => {
            println!("\nCould not launch the guided permission helper: {err}");
            print_current_report(automation_target);
            return;
        }
    };

    let clock = polarize_core::wait::SystemClock::new();
    let sleeper = polarize_core::bootstrap::SystemSleeper;
    let target = automation_target.to_string();
    let result = polarize_core::bootstrap::wait_for_grants_or_close(
        &mut child,
        &clock,
        &sleeper,
        polarize_core::bootstrap::DEFAULT_WAIT_DEADLINE_MS,
        polarize_core::bootstrap::DEFAULT_WAIT_POLL_INTERVAL_MS,
        || poll_needed_permissions(&target),
    );

    match result.outcome {
        polarize_core::bootstrap::WaitOutcome::AllGranted => {
            println!("\nAll requested permissions are now granted.");
        }
        polarize_core::bootstrap::WaitOutcome::HelperExited => {
            println!("\nThe guided setup helper window closed.");
        }
        polarize_core::bootstrap::WaitOutcome::TimedOut => {
            println!("\nTimed out waiting for the guided setup helper.");
        }
    }
    print_final_report_from_needed(automation_target, &result.still_needed);
}

/// Re-reads every real permission state through the non-prompting
/// checks, and returns what `needed_permissions` still finds missing.
/// This is the `poll` closure `wait_for_grants_or_close` drives — see
/// PINV-56 (never a prompting call here) and PINV-65 (this exact read
/// backs both the helper's early close and the terminal's report).
fn poll_needed_permissions(
    automation_target: &str,
) -> Vec<polarize_core::bootstrap::NeededPermission> {
    let accessibility = polarize_macos::permission_bootstrap::check_accessibility();
    let screen_recording = polarize_macos::permission_bootstrap::check_screen_recording();
    let automation = polarize_macos::permission_bootstrap::check_automation(automation_target);
    polarize_core::bootstrap::needed_permissions(
        accessibility,
        screen_recording,
        automation,
        automation_target,
    )
}

fn describe_needed_permission(permission: &polarize_core::bootstrap::NeededPermission) -> String {
    use polarize_core::bootstrap::NeededPermission;
    match permission {
        NeededPermission::Accessibility => "Accessibility".to_string(),
        NeededPermission::ScreenRecording => "Screen Recording".to_string(),
        NeededPermission::Automation { target } => format!("Automation ({target})"),
    }
}

/// Prints the final report from one fresh non-prompting re-read. Used
/// on the graceful launch-failure path (AC 4), where no wait loop ever
/// ran to produce a `still_needed` set of its own.
fn print_current_report(automation_target: &str) {
    let needed = poll_needed_permissions(automation_target);
    print_final_report_from_needed(automation_target, &needed);
}

/// Prints the closing "run again" hint from a `still_needed` set,
/// exactly as returned by the same read that decided the wait loop's
/// outcome — never a second, independent read (PINV-65). An empty set
/// prints nothing further: everything is granted.
fn print_final_report_from_needed(
    automation_target: &str,
    still_needed: &[polarize_core::bootstrap::NeededPermission],
) {
    if still_needed.is_empty() {
        return;
    }
    let missing = still_needed
        .iter()
        .map(describe_needed_permission)
        .collect::<Vec<_>>()
        .join(", ");
    println!("\nStill missing: {missing}.");
    print_retry_hint(automation_target);
}

fn print_retry_hint(automation_target: &str) {
    println!(
        "\nIf a system dialog appeared, approve it, then run \
         `./target/debug/polarize --request-permissions {automation_target}` again to \
         confirm.\n\
         `run_applescript` against any other app (Mail, Safari, Music, Notes, …) needs \
         its own bootstrap run first — \
         `./target/debug/polarize --request-permissions <App Name>` — \
         Automation is granted per target app, not once for the whole binary."
    );
}
