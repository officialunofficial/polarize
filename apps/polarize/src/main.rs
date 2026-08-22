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

    if std::env::args().nth(1).as_deref() == Some("--request-permissions") {
        let automation_target = std::env::args()
            .nth(2)
            .unwrap_or_else(|| AUTOMATION_BOOTSTRAP_TARGET.to_string());
        request_permissions(&automation_target);
        return;
    }

    // Must happen before the runtime below starts any other thread —
    // see `polarize_macos::self_responsibility`'s own doc comment for
    // why. This is the server-run path only; `--request-permissions`
    // above already returned, so its own bootstrap send (PINV-51)
    // stays untouched by this.
    log_responsibility_state();
    log_app_group_state();
    respawn_disclaimed_if_warranted();

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

    if !accessibility
        || !screen_recording
        || automation != polarize_core::script::AutomationCheck::Permitted
    {
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
}
