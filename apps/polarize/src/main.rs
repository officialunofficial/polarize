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
    let server = PolarizeServer::default();
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
