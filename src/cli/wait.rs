//! `coord wait` — block until a matching task appears, sending
//! heartbeats while waiting so the caller stays "active" in the TUI.
//!
//! Implementation: long-poll. We call `tasks/list` with `wait_ms` set
//! and the daemon holds the request open until a matching task lands
//! or the per-call window expires. We re-call in a loop, beating in
//! between, until the outer `--timeout` elapses or a match is found.
//! Substring-on-name and `--ignore-id` filtering still happen
//! client-side because they're not SQL-pushable.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::json;

use super::client::Client;

/// Per-call long-poll window. Sized so the daemon side stays well
/// under its 60s cap, leaves room for a heartbeat between calls, and
/// recovers reasonably quickly from a transient list failure.
const PER_CALL_WAIT_MS: u64 = 25_000;

#[derive(clap::Args)]
pub struct WaitArgs {
    /// Heartbeat as this agent ID while waiting.
    #[arg(long = "as")]
    pub agent_id: String,
    /// Display name for the heartbeat.
    #[arg(long, default_value = "watcher")]
    pub name: String,
    /// Match only tasks of this kind (bug, feature, ack, decision, ...).
    #[arg(long)]
    pub kind: Option<String>,
    /// Match only tasks in this state, or `any` to skip the filter. When
    /// omitted the default is kind-aware: announcement kinds (ack,
    /// knowledge, decision) match `completed` because they're
    /// completed-on-creation; everything else matches `pending`.
    #[arg(long)]
    pub state: Option<String>,
    /// Match priority. Repeat or comma-separate (e.g. `--priority urgent,high`).
    #[arg(long, value_delimiter = ',')]
    pub priority: Vec<String>,
    /// Substring match against the task name (case-insensitive).
    #[arg(long = "name-contains")]
    pub name_contains: Option<String>,
    /// Comma-separated UUIDs to ignore (deduplication across calls).
    #[arg(long = "ignore-id", value_delimiter = ',')]
    pub ignore_ids: Vec<String>,
    /// Max wait time in seconds. Exit code 2 on timeout.
    #[arg(long, default_value_t = 600)]
    pub timeout: u64,
    /// Heartbeat interval in seconds.
    #[arg(long, default_value_t = 15)]
    pub heartbeat: u64,
}

pub fn run(client: &Client, args: &WaitArgs) -> Result<()> {
    let needle = args.name_contains.as_deref().map(str::to_lowercase);
    let priority_filter: HashSet<&str> = args.priority.iter().map(String::as_str).collect();
    let ignore: HashSet<&str> = args.ignore_ids.iter().map(String::as_str).collect();
    let started = Instant::now();
    let timeout = Duration::from_secs(args.timeout);
    let mut last_heartbeat = Instant::now() - Duration::from_secs(args.heartbeat + 1);

    let effective_state: Option<String> = match args.state.as_deref() {
        Some("any") => None,
        Some(s) => Some(s.to_string()),
        None => Some(default_state_for_kind(args.kind.as_deref()).to_string()),
    };

    loop {
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            eprintln!(
                "coord wait: timeout after {}s, no matching task",
                args.timeout
            );
            std::process::exit(2);
        }

        if last_heartbeat.elapsed() >= Duration::from_secs(args.heartbeat) {
            let _ = client.call(
                "agents/heartbeat",
                json!({ "id": args.agent_id, "name": args.name }),
            );
            last_heartbeat = Instant::now();
        }

        // Cap the long-poll window so we still heartbeat on schedule
        // and so we can re-check the outer timeout. The daemon already
        // caps wait_ms at 60s; we stay below that.
        let remaining_ms = timeout.saturating_sub(elapsed).as_millis() as u64;
        let wait_ms = remaining_ms.min(PER_CALL_WAIT_MS);

        let mut params = json!({ "limit": 200, "wait_ms": wait_ms });
        if let Some(s) = &effective_state {
            params["state"] = json!(s);
        }
        if let Some(k) = &args.kind {
            params["kind"] = json!(k);
        }

        match client.call("tasks/list", params) {
            Ok(v) => {
                if let Some(arr) = v.as_array() {
                    for t in arr {
                        let id = t.get("id").and_then(|s| s.as_str()).unwrap_or("");
                        if ignore.contains(id) {
                            continue;
                        }
                        if !priority_filter.is_empty() {
                            let p = t
                                .get("priority")
                                .and_then(|s| s.as_str())
                                .unwrap_or("normal");
                            if !priority_filter.contains(p) {
                                continue;
                            }
                        }
                        if let Some(needle) = &needle {
                            let n = t
                                .get("name")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            if !n.contains(needle) {
                                continue;
                            }
                        }
                        println!("{}", serde_json::to_string_pretty(t)?);
                        return Ok(());
                    }
                }
                // Empty array or no client-side match: loop back and
                // long-poll again. No sleep needed — the server held
                // the connection for us.
            }
            Err(e) => {
                eprintln!("coord wait: list failed ({e}) — retrying in 1s");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn default_state_for_kind(kind: Option<&str>) -> &'static str {
    match kind {
        Some("ack") | Some("knowledge") | Some("decision") => "completed",
        _ => "pending",
    }
}
