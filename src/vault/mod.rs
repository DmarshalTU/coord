//! Markdown vault writer.
//!
//! Writes a human-readable audit trail of every task and agent into a
//! configurable directory. Drop the directory into Obsidian to get a
//! graph view of the project's nervous system for free — no UI,
//! no server, no special tooling required.
//!
//! Layout:
//!   .agentd-vault/
//!   ├── README.md
//!   ├── tasks/
//!   │   └── 2026-05-07-1432-bug-validate-token.md
//!   ├── agents/
//!   │   └── feature-a-v1.2.md
//!   └── decisions/
//!       └── 2026-05-07-jwt-not-sessions.md   (mirror of decision-kind tasks)
//!
//! Wikilinks (`[[...]]`) are used to cross-reference agents and tasks so
//! Obsidian's graph and backlink panes light up automatically.

use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::core::store::{slugify, task_slug};
use crate::core::types::Task;

pub struct Vault {
    root: PathBuf,
    /// Per-agent lock so concurrent claims/completes don't tear the
    /// agent log file. Tasks are write-whole-file so they don't need this.
    agent_locks: Mutex<HashMap<String, ()>>,
}

impl Vault {
    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("tasks")).context("create tasks/ dir")?;
        fs::create_dir_all(root.join("agents")).context("create agents/ dir")?;
        fs::create_dir_all(root.join("decisions")).context("create decisions/ dir")?;
        let me = Self {
            root,
            agent_locks: Mutex::new(HashMap::new()),
        };
        me.write_readme()?;
        Ok(me)
    }

    /// Write the task's markdown note. `relations` is `(label,
    /// target_slug)` pairs to render as wikilinks under `## Related`,
    /// driven by the store's UUID-typed payload-field resolution. Pass
    /// an empty slice if you don't have relations yet — the section
    /// just won't render.
    pub fn write_task(&self, task: &Task, relations: &[(String, String)]) -> Result<()> {
        let body = render_task_md(task, relations);
        let slug = task_slug(task);
        let path = self.root.join("tasks").join(format!("{slug}.md"));
        atomic_write(&path, body.as_bytes())?;

        // Mirror decision-kind tasks into decisions/ so they're easy to browse.
        if task.kind == "decision" {
            let dpath = self.root.join("decisions").join(format!("{slug}.md"));
            atomic_write(&dpath, body.as_bytes())?;
        }
        Ok(())
    }

    /// Initialize an agent note on first heartbeat. Idempotent.
    pub fn touch_agent(&self, id: &str, name: &str) -> Result<()> {
        let _guard = self.lock_agent(id);
        let path = self.agent_path(id);
        if path.exists() {
            return Ok(());
        }
        let body = format!(
            "---\nid: {id}\nname: {name}\nfirst_seen: {now}\n---\n\n# {id}\n\n## Activity\n",
            id = id,
            name = name,
            now = Utc::now().to_rfc3339(),
        );
        atomic_write(&path, body.as_bytes())
    }

    /// Append a one-line activity event to the agent's note.
    pub fn append_agent_event(&self, id: &str, event: &str) -> Result<()> {
        let _guard = self.lock_agent(id);
        let path = self.agent_path(id);
        if !path.exists() {
            // Initialize a stub if we somehow append before heartbeat.
            self.touch_agent(id, id)?;
        }
        let line = format!(
            "- `{ts}` {event}\n",
            ts = Utc::now().format("%Y-%m-%d %H:%M:%S"),
            event = event
        );
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("open {path:?}"))?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }

    fn lock_agent(&self, id: &str) -> AgentGuard<'_> {
        // We use the per-agent map merely as a presence record; the
        // outer Mutex serializes any agent's writes. Good enough for the
        // expected handful-of-agents case.
        let mut map = self.agent_locks.lock();
        map.entry(id.into()).or_insert(());
        AgentGuard { _map: map }
    }

    fn agent_path(&self, id: &str) -> PathBuf {
        self.root.join("agents").join(format!("{}.md", slugify(id)))
    }

    fn write_readme(&self) -> Result<()> {
        let path = self.root.join("README.md");
        if path.exists() {
            return Ok(());
        }
        let body = r#"# agentd vault

Auto-generated audit trail of every task and agent in this project.

- **`tasks/`** — one note per task, frontmatter + payload + result
- **`agents/`** — one note per agent, with their activity log
- **`decisions/`** — mirrors of every `kind=decision` task for easy browsing

Open this directory in Obsidian (no plugins needed) and hit `Cmd+G` for
the graph view to see who's working on what across all sessions.

Wikilinks (`[[...]]`) cross-reference agents and tasks. Backlinks pane
shows everywhere a task or agent is mentioned.
"#;
        atomic_write(&path, body.as_bytes())
    }
}

/// Agent map guard. Holding it serializes file writes for that agent.
struct AgentGuard<'a> {
    _map: parking_lot::MutexGuard<'a, HashMap<String, ()>>,
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("md.tmp");
    fs::write(&tmp, contents).with_context(|| format!("write {tmp:?}"))?;
    fs::rename(&tmp, path).with_context(|| format!("rename {tmp:?} -> {path:?}"))?;
    Ok(())
}

fn render_task_md(task: &Task, relations: &[(String, String)]) -> String {
    let mut s = String::new();
    s.push_str("---\n");
    s.push_str(&format!("id: {}\n", task.id));
    s.push_str(&format!("kind: {}\n", task.kind));
    s.push_str(&format!("priority: {}\n", task.priority));
    s.push_str(&format!("state: {}\n", task.state.as_str()));
    if let Some(by) = &task.claimed_by {
        s.push_str(&format!("claimed_by: {by}\n"));
    }
    s.push_str(&format!("created: {}\n", task.created_at.to_rfc3339()));
    s.push_str(&format!("updated: {}\n", task.updated_at.to_rfc3339()));
    if let Some(refs) = task.payload.get("file_refs").and_then(|v| v.as_array()) {
        let names: Vec<String> = refs
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if !names.is_empty() {
            s.push_str(&format!("file_refs: [{}]\n", names.join(", ")));
        }
    }
    s.push_str("---\n\n");
    s.push_str(&format!("# {}: {}\n\n", task.kind, task.name));

    // Render relations as wikilinks BEFORE the payload so Obsidian's
    // graph picks them up immediately and the human reader sees the
    // cross-references at a glance.
    if !relations.is_empty() {
        s.push_str("## Related\n\n");
        for (label, target_slug) in relations {
            s.push_str(&format!("- `{label}` → [[{target_slug}]]\n"));
        }
        s.push('\n');
    }

    if !task.payload.is_null() {
        s.push_str("## Payload\n\n```json\n");
        s.push_str(
            &serde_json::to_string_pretty(&task.payload)
                .unwrap_or_else(|_| task.payload.to_string()),
        );
        s.push_str("\n```\n\n");
    }
    if let Some(by) = &task.claimed_by {
        s.push_str(&format!("## Claimed by\n[[{}]]\n\n", slugify(by)));
    }
    if let Some(result) = &task.result {
        s.push_str("## Result\n\n```json\n");
        s.push_str(&serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string()));
        s.push_str("\n```\n");
    }
    s
}
