//! `polarize`: a stdio MCP server that automates real native macOS AppKit
//! applications.
//!
//! This binary is a thin wiring layer: MCP tool calls delegate to
//! `polarize-core` (pure logic) and `polarize-macos` (real framework
//! calls) — see [`server::PolarizeServer`]. It carries minimal logic of
//! its own: constructing the server, attaching it to the stdio
//! transport, and running it until the client disconnects.

mod server;

use rmcp::ServiceExt;
use rmcp::transport::stdio;

use server::PolarizeServer;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("polarize: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--request-permissions") {
        request_permissions();
        return Ok(());
    }

    let server = PolarizeServer::default();
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

/// One-time setup: shows the system Accessibility and Screen Recording
/// consent alerts for this exact binary, so their grants survive future
/// `just build` re-signs. Not part of the MCP server's own startup path
/// — see `polarize_macos::permission_bootstrap` for why this exists.
fn request_permissions() {
    println!("Requesting Accessibility permission...");
    let accessibility = polarize_macos::permission_bootstrap::request_accessibility();
    println!("  Accessibility trusted: {accessibility}");

    println!("Requesting Screen Recording permission...");
    let screen_recording = polarize_macos::permission_bootstrap::request_screen_recording();
    println!("  Screen Recording trusted: {screen_recording}");

    if !accessibility || !screen_recording {
        println!(
            "\nIf a system dialog appeared, approve it, then run \
             `./target/debug/polarize --request-permissions` again to confirm."
        );
    }
}
