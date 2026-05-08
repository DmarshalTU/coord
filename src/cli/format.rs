//! Plain-text printers for `status`, `tasks`, `agents`. The fancy
//! ratatui rendering lives in [`super::tui`].

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::client::Client;

pub fn print_status(client: &Client) -> Result<()> {
    let agents = client.call("agents/list", json!({}))?;
    let tasks = client.call("tasks/list", json!({ "limit": 100 }))?;
    let agent_count = agents.as_array().map_or(0, |a| a.len());
    let task_count = tasks.as_array().map_or(0, |a| a.len());
    let pending = count_state(&tasks, "pending");
    let claimed = count_state(&tasks, "claimed");
    println!("coord ({})", client.url);
    println!("  agents     : {agent_count}");
    println!("  tasks      : {task_count} (pending: {pending}, claimed: {claimed})");
    Ok(())
}

pub fn print_tasks(value: &Value) {
    let Some(arr) = value.as_array() else { return };
    println!(
        "{:<10}  {:<10}  {:<8}  {:<10}  {:<20}  NAME",
        "ID", "STATE", "PRIO", "KIND", "CLAIMED_BY"
    );
    for t in arr {
        let id: String = t
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(8).collect())
            .unwrap_or_default();
        let state = t.get("state").and_then(|v| v.as_str()).unwrap_or("");
        let prio = t
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("normal");
        let kind = t.get("kind").and_then(|v| v.as_str()).unwrap_or("task");
        let claimed = t.get("claimed_by").and_then(|v| v.as_str()).unwrap_or("-");
        let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
        println!("{id:<10}  {state:<10}  {prio:<8}  {kind:<10}  {claimed:<20}  {name}");
    }
}

pub fn print_agents(value: &Value) {
    let Some(arr) = value.as_array() else { return };
    let now = Utc::now();
    println!("{:<22}  {:<22}  {:<8}  STATUS", "ID", "NAME", "AGE");
    for a in arr {
        let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let last = a.get("last_seen").and_then(|v| v.as_str()).unwrap_or("");
        let age = parse_age(last, now);
        let age_str = age.map(human_age).unwrap_or_else(|| "-".into());
        let status = match age {
            Some(s) if s <= super::tui::IDLE_AGENT_AFTER_SECS => "active",
            Some(s) if s <= super::tui::STALE_AGENT_AFTER_SECS => "idle",
            Some(_) => "stale",
            None => "?",
        };
        println!("{id:<22}  {name:<22}  {age_str:<8}  {status}");
    }
}

fn count_state(tasks: &Value, state: &str) -> usize {
    tasks.as_array().map_or(0, |a| {
        a.iter()
            .filter(|t| t.get("state").and_then(|s| s.as_str()) == Some(state))
            .count()
    })
}

/// RFC3339 → seconds-since-now. Returns None if the timestamp is unparseable.
pub(super) fn parse_age(ts: &str, now: DateTime<Utc>) -> Option<f64> {
    let parsed = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    let delta = now.signed_duration_since(parsed);
    Some(delta.num_milliseconds() as f64 / 1000.0)
}

/// Compact age: `4.2s`, `1m12s`, `3h05m`, `2d04h`.
pub(super) fn human_age(secs: f64) -> String {
    let s = secs.max(0.0);
    if s < 10.0 {
        format!("{s:>4.1}s")
    } else if s < 60.0 {
        format!("{:>4.0}s", s)
    } else if s < 3600.0 {
        let m = (s / 60.0) as u64;
        let r = (s as u64) % 60;
        format!("{m}m{r:02}s")
    } else if s < 86_400.0 {
        let h = (s / 3600.0) as u64;
        let m = ((s as u64) % 3600) / 60;
        format!("{h}h{m:02}m")
    } else {
        let d = (s / 86_400.0) as u64;
        let h = ((s as u64) % 86_400) / 3600;
        format!("{d}d{h:02}h")
    }
}
