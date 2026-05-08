//! Client-side subcommands. They all talk to a running `coord serve`
//! over JSON-RPC. The server-side commands (`serve`, `mcp`, `version`)
//! live next to this module in [`crate::server`].

use anyhow::{anyhow, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::server;

mod client;
mod format;
mod init;
mod tui;
mod wait;

use client::Client;

#[derive(Subcommand)]
pub enum Cmd {
    // ---------------- server ----------------
    /// Run the coord daemon (HTTP JSON-RPC + A2A surface).
    Serve(ServeArgs),
    /// Run a stdio MCP bridge for IDE clients (Claude Code, Cursor, ...).
    Mcp(McpArgs),
    /// Print version and protocol info.
    Version,
    /// Scaffold a project for use with coord (drops .mcp.json + AGENTS.md).
    Init(init::InitArgs),

    // ---------------- client ----------------
    /// One-shot summary of agents and tasks.
    Status,
    /// Live TUI of agents and tasks (htop-style).
    Top {
        /// Render one frame to stdout and exit. Used by snapshot tests.
        #[arg(long)]
        once: bool,
    },
    /// List recent tasks.
    Tasks {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// List known agents.
    Agents,
    /// Create a task.
    Send {
        name: String,
        /// JSON payload to attach to the task.
        #[arg(long)]
        payload: Option<String>,
        /// Task category. Common values: task, bug, feature, decision,
        /// ack, knowledge, build.
        #[arg(long, default_value = "task")]
        kind: String,
        /// Priority. Common values: low, normal, high, urgent.
        #[arg(long, default_value = "normal")]
        priority: String,
    },
    /// Atomically claim a pending task as `agent_id`.
    Claim {
        id: Uuid,
        #[arg(long = "as")]
        agent_id: String,
    },
    /// Mark a claimed task as completed with an optional JSON result.
    Complete {
        id: Uuid,
        #[arg(long)]
        result: Option<String>,
    },
    /// Cancel a task.
    Cancel { id: Uuid },
    /// Send a heartbeat for an agent. Accepts the ID positionally or as
    /// `--as <ID>` (matches `claim` and `wait`).
    Heartbeat {
        agent_id_pos: Option<String>,
        #[arg(long = "as")]
        agent_id_flag: Option<String>,
        #[arg(long, default_value = "shell-agent")]
        name: String,
    },
    /// Block until a task matching the filter appears, then print it as
    /// JSON. Sends heartbeats while waiting so the caller stays "active"
    /// in `coord top`. This is the primitive that lets a Claude tab be
    /// a watcher with one chat message.
    Wait(wait::WaitArgs),
}

/// Server-side arg structs are inlined here so `coord` exposes one flat
/// subcommand list (`coord serve`, `coord top`, ...) instead of two.
#[derive(clap::Args)]
pub struct ServeArgs {
    #[arg(long, env = "COORD_ADDR", default_value = "127.0.0.1:7777")]
    pub addr: std::net::SocketAddr,
    #[arg(long, env = "COORD_DB")]
    pub db: Option<std::path::PathBuf>,
    #[arg(long, env = "COORD_VAULT")]
    pub vault: Option<std::path::PathBuf>,
}

#[derive(clap::Args)]
pub struct McpArgs {
    #[arg(long, env = "COORD_URL", default_value = "http://127.0.0.1:7777/")]
    pub url: String,
}

pub async fn dispatch(url: String, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Serve(a) => server::serve(a.addr, a.db, a.vault).await,
        Cmd::Mcp(a) => server::mcp(a.url).await,
        Cmd::Version => {
            server::print_version();
            Ok(())
        }
        Cmd::Init(args) => init::run(&args),
        other => {
            // Client subcommands use a blocking JSON-RPC client; run them
            // off the async runtime so reqwest::blocking is happy.
            tokio::task::spawn_blocking(move || run_client(url, other)).await?
        }
    }
}

fn run_client(url: String, cmd: Cmd) -> Result<()> {
    let client = Client::new(url);
    match cmd {
        Cmd::Status => format::print_status(&client),
        Cmd::Top { once } => {
            if once {
                tui::render_once(&client)
            } else {
                tui::run(&client)
            }
        }
        Cmd::Tasks { limit } => {
            let tasks = client.call("tasks/list", json!({ "limit": limit }))?;
            format::print_tasks(&tasks);
            Ok(())
        }
        Cmd::Agents => {
            let agents = client.call("agents/list", json!({}))?;
            format::print_agents(&agents);
            Ok(())
        }
        Cmd::Send {
            name,
            payload,
            kind,
            priority,
        } => {
            let payload: Option<Value> = payload.map(|s| serde_json::from_str(&s)).transpose()?;
            let task = client.call(
                "tasks/send",
                json!({ "name": name, "kind": kind, "priority": priority, "payload": payload }),
            )?;
            println!("{}", serde_json::to_string_pretty(&task)?);
            Ok(())
        }
        Cmd::Claim { id, agent_id } => {
            let task = client.call("tasks/claim", json!({ "id": id, "agentId": agent_id }))?;
            println!("{}", serde_json::to_string_pretty(&task)?);
            Ok(())
        }
        Cmd::Complete { id, result } => {
            let result: Option<Value> = result.map(|s| serde_json::from_str(&s)).transpose()?;
            let resp = client.call("tasks/complete", json!({ "id": id, "result": result }))?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            Ok(())
        }
        Cmd::Cancel { id } => {
            let resp = client.call("tasks/cancel", json!({ "id": id }))?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            Ok(())
        }
        Cmd::Heartbeat {
            agent_id_pos,
            agent_id_flag,
            name,
        } => {
            let agent_id = agent_id_flag.or(agent_id_pos).ok_or_else(|| {
                anyhow!("agent ID required: pass it as a positional argument or with --as")
            })?;
            let resp = client.call("agents/heartbeat", json!({ "id": agent_id, "name": name }))?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            Ok(())
        }
        Cmd::Wait(args) => wait::run(&client, &args),
        Cmd::Serve(_) | Cmd::Mcp(_) | Cmd::Version | Cmd::Init(_) => {
            unreachable!("handled in dispatch")
        }
    }
}
