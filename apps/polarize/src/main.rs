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

fn main() {
    init_tracing();

    if std::env::args().nth(1).as_deref() == Some("--request-permissions") {
        request_permissions();
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

/// The one target every Mac has, that `run_applescript` is documented
/// to reach. See PINV-44: Automation is granted per (this process,
/// target app) pair, so this only ever bootstraps Finder — a caller
/// scripting Mail, Safari, Music, or Notes still needs its own first
/// real script send (through `run_applescript` itself) to raise that
/// target's own consent dialog.
const AUTOMATION_BOOTSTRAP_TARGET: &str = "Finder";

/// One-time setup: shows the system Accessibility, Screen Recording,
/// and Automation consent alerts for this exact binary, so their
/// grants survive future `just build` re-signs. Not part of the MCP
/// server's own startup path — see `polarize_macos::permission_bootstrap`
/// for why this exists.
fn request_permissions() {
    println!("Requesting Accessibility permission...");
    let accessibility = polarize_macos::permission_bootstrap::request_accessibility();
    println!("  Accessibility trusted: {accessibility}");

    println!("Requesting Screen Recording permission...");
    let screen_recording = polarize_macos::permission_bootstrap::request_screen_recording();
    println!("  Screen Recording trusted: {screen_recording}");

    println!("Requesting Automation permission for {AUTOMATION_BOOTSTRAP_TARGET}...");
    let automation =
        polarize_macos::permission_bootstrap::request_automation(AUTOMATION_BOOTSTRAP_TARGET);
    println!("  Automation ({AUTOMATION_BOOTSTRAP_TARGET}): {automation:?}");

    if !accessibility
        || !screen_recording
        || automation != polarize_core::script::AutomationCheck::Permitted
    {
        println!(
            "\nIf a system dialog appeared, approve it, then run \
             `./target/debug/polarize --request-permissions` again to confirm.\n\
             `run_applescript` against any other app (Mail, Safari, Music, Notes, …) \
             still needs its own first real call to raise that app's own consent dialog — \
             Automation is granted per target app, not once for the whole binary."
        );
    }
}
