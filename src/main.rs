//! `coord` — a local coordinator for parallel AI coding agents.
//!
//! One binary, two roles. `coord serve` is the long-lived daemon (HTTP
//! JSON-RPC + A2A surface). `coord mcp` is a stdio MCP bridge that IDEs
//! (Claude Code, Cursor, Codex, ...) point at. Every other subcommand
//! is a client that talks to a running daemon.

use anyhow::Result;
use clap::Parser;

mod cli;
mod server;

#[derive(Parser)]
#[command(
    name = "coord",
    version,
    about = "A local coordinator for parallel AI coding agents"
)]
struct Cli {
    /// JSON-RPC URL of the daemon. Used by every client subcommand.
    #[arg(
        long,
        env = "COORD_URL",
        default_value = "http://127.0.0.1:7777/",
        global = true
    )]
    url: String,

    #[command(subcommand)]
    command: cli::Cmd,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("coord=info,tower_http=info")
            }),
        )
        .init();

    let parsed = Cli::parse();
    cli::dispatch(parsed.url, parsed.command).await
}
