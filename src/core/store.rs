//! SQLite-backed state. All mutations go through atomic transactions so
//! that task claims are race-free across many connected agents.
//!
//! Optionally writes a human-readable markdown audit trail to a vault
//! directory on every state change. Drop the vault into Obsidian to get
//! a graph view of the project's nervous system for free.

use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use super::types::{Agent, Task, TaskState, DEFAULT_KIND, DEFAULT_PRIORITY};
use crate::vault::Vault;

pub struct Store {
    conn: Mutex<Connection>,
    vault: Option<Arc<Vault>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_vault(path, None)
    }

    pub fn open_with_vault(path: &Path, vault_dir: Option<PathBuf>) -> Result<Self> {
        let conn = Connection::open(path).context("open sqlite")?;
        conn.execute_batch(SCHEMA)?;
        // WAL keeps short writes from blocking concurrent readers, which
        // matters when N agents poll for tasks in tight loops.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Forward-compatible additive migrations. SQLite errors with
        // "duplicate column name" if these have already run; we swallow
        // those specifically and let real errors surface.
        try_add_column(&conn, "tasks", "kind", "TEXT NOT NULL DEFAULT 'task'")?;
        try_add_column(&conn, "tasks", "priority", "TEXT NOT NULL DEFAULT 'normal'")?;
        // `first_seen` defaults to epoch on existing rows; the next heartbeat
        // for that agent will not overwrite it (see UPSERT in `heartbeat`).
        try_add_column(
            &conn,
            "agents",
            "first_seen",
            "TEXT NOT NULL DEFAULT '1970-01-01T00:00:00Z'",
        )?;

        let vault = vault_dir
            .map(|p| -> Result<Arc<Vault>> { Ok(Arc::new(Vault::open(p)?)) })
            .transpose()?;
        Ok(Self {
            conn: Mutex::new(conn),
            vault,
        })
    }

    pub fn create_task(&self, name: &str, payload: serde_json::Value) -> Result<Task> {
        self.create_task_full(name, DEFAULT_KIND, DEFAULT_PRIORITY, payload)
    }

    pub fn create_task_full(
        &self,
        name: &str,
        kind: &str,
        priority: &str,
        payload: serde_json::Value,
    ) -> Result<Task> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let payload_s = serde_json::to_string(&payload)?;
        // Announcement-kind tasks (ack / knowledge / decision) are
        // publications, not work to be claimed — they're "done" the
        // instant they're posted. Other kinds (bug / feature / task)
        // need someone to pick them up, so they start pending.
        let initial_state = default_state_for_kind(kind);
        {
            let conn = self.conn.lock();
            conn.execute(
                "INSERT INTO tasks (id, name, kind, priority, state, payload, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    id.to_string(),
                    name,
                    kind,
                    priority,
                    initial_state.as_str(),
                    payload_s,
                    now.to_rfc3339(),
                ],
            )?;
        }
        let task = Task {
            id,
            name: name.into(),
            kind: kind.into(),
            priority: priority.into(),
            state: initial_state,
            claimed_by: None,
            payload,
            result: None,
            created_at: now,
            updated_at: now,
        };
        self.vault_write(&task);
        Ok(task)
    }

    pub fn get_task(&self, id: Uuid) -> Result<Option<Task>> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT id, name, kind, priority, state, claimed_by, payload, result, created_at, updated_at
                 FROM tasks WHERE id = ?1",
                params![id.to_string()],
                row_to_task,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_tasks(&self, limit: usize) -> Result<Vec<Task>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, priority, state, claimed_by, payload, result, created_at, updated_at
             FROM tasks ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], row_to_task)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Atomically transition a `pending` task to `claimed` for `agent_id`.
    /// Returns the task on success or `None` if it was already claimed.
    pub fn claim_task(&self, id: Uuid, agent_id: &str) -> Result<Option<Task>> {
        let now = Utc::now();
        let task_opt = {
            let conn = self.conn.lock();
            let updated = conn.execute(
                "UPDATE tasks SET state = 'claimed', claimed_by = ?1, updated_at = ?2
                 WHERE id = ?3 AND state = 'pending'",
                params![agent_id, now.to_rfc3339(), id.to_string()],
            )?;
            if updated == 0 {
                None
            } else {
                Some(conn.query_row(
                    "SELECT id, name, kind, priority, state, claimed_by, payload, result, created_at, updated_at
                     FROM tasks WHERE id = ?1",
                    params![id.to_string()],
                    row_to_task,
                )?)
            }
        };
        if let Some(t) = &task_opt {
            self.vault_write(t);
            self.vault_agent_event(agent_id, &format!("claimed [[{}]]", task_slug(t)));
        }
        Ok(task_opt)
    }

    /// Mark a claimed task complete. Idempotent against races: only
    /// transitions when current state is `claimed`, so a `cancel` that
    /// sneaks in first is sticky. Returns true on success.
    pub fn complete_task(&self, id: Uuid, result: serde_json::Value) -> Result<bool> {
        let now = Utc::now();
        let result_s = serde_json::to_string(&result)?;
        let updated = {
            let conn = self.conn.lock();
            conn.execute(
                "UPDATE tasks SET state = 'completed', result = ?1, updated_at = ?2
                 WHERE id = ?3 AND state = 'claimed'",
                params![result_s, now.to_rfc3339(), id.to_string()],
            )?
        };
        if updated > 0 {
            if let Some(t) = self.get_task(id)? {
                self.vault_write(&t);
                if let Some(agent) = &t.claimed_by {
                    self.vault_agent_event(agent, &format!("completed [[{}]]", task_slug(&t)));
                }
            }
        }
        Ok(updated > 0)
    }

    pub fn cancel_task(&self, id: Uuid) -> Result<()> {
        let now = Utc::now();
        {
            let conn = self.conn.lock();
            conn.execute(
                "UPDATE tasks SET state = 'cancelled', updated_at = ?1 WHERE id = ?2",
                params![now.to_rfc3339(), id.to_string()],
            )?;
        }
        if let Some(t) = self.get_task(id)? {
            self.vault_write(&t);
        }
        Ok(())
    }

    pub fn heartbeat(&self, agent_id: &str, name: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        {
            let conn = self.conn.lock();
            // first_seen is set on initial insert and *never* updated,
            // so the TUI can render a stable uptime per agent. last_seen
            // and name update on every beat as before.
            conn.execute(
                "INSERT INTO agents (id, name, last_seen, first_seen) VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen, name = excluded.name",
                params![agent_id, name, now],
            )?;
            // Backfill: if this agent existed before the migration ran (so
            // first_seen got set to epoch), promote it to last_seen. After
            // this one-shot upgrade it stays put.
            conn.execute(
                "UPDATE agents SET first_seen = last_seen
                 WHERE id = ?1 AND first_seen = '1970-01-01T00:00:00Z'",
                params![agent_id],
            )?;
        }
        // We deliberately do not write a vault note for every heartbeat — that
        // would explode the agent log with noise. Heartbeats only refresh
        // the last_seen field, which the next claim/complete event will pick
        // up when it rewrites the agent note.
        if let Some(v) = &self.vault {
            let _ = v.touch_agent(agent_id, name);
        }
        Ok(())
    }

    pub fn list_agents(&self) -> Result<Vec<Agent>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT a.id, a.name, a.last_seen, a.first_seen,
                    (SELECT t.id FROM tasks t WHERE t.claimed_by = a.id AND t.state = 'claimed' LIMIT 1)
             FROM agents a ORDER BY a.last_seen DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let last_seen: String = row.get(2)?;
                let last_seen = chrono::DateTime::parse_from_rfc3339(&last_seen)
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?
                    .with_timezone(&Utc);
                let first_seen: String = row.get(3)?;
                let first_seen = chrono::DateTime::parse_from_rfc3339(&first_seen)
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?
                    .with_timezone(&Utc);
                let current: Option<String> = row.get(4)?;
                let current_task = current.as_deref().and_then(|s| Uuid::parse_str(s).ok());
                Ok(Agent {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    first_seen,
                    last_seen,
                    current_task,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn vault_write(&self, task: &Task) {
        if let Some(v) = &self.vault {
            // Resolve any UUID-typed payload fields to their target
            // task's slug so the vault can render real wikilinks (and
            // Obsidian's graph view shows actual edges).
            let relations = self.collect_relations(task);
            // Vault writes are best-effort: a failed disk write must not
            // fail a coordination call. Log and move on.
            if let Err(e) = v.write_task(task, &relations) {
                tracing::warn!(error = %e, task = %task.id, "vault: failed to write task note");
            }
        }
    }

    /// Scan a task's payload for fields that name another task by UUID.
    /// Resolve each one to (label, target_slug) so the vault can render
    /// `## Related` wikilinks. Unknown / unresolvable UUIDs are skipped
    /// silently — vault rendering must never block coordination.
    fn collect_relations(&self, task: &Task) -> Vec<(String, String)> {
        const RELATION_KEYS: &[&str] = &[
            "fixed_bug_id",
            "related_ack",
            "related_bug",
            "bug_id",
            "ack_id",
            "blocked_by",
            "follows_up",
        ];
        let mut out = Vec::new();
        for key in RELATION_KEYS {
            if let Some(uuid_str) = task.payload.get(key).and_then(|v| v.as_str()) {
                if let Ok(uuid) = Uuid::parse_str(uuid_str) {
                    if let Ok(Some(target)) = self.get_task(uuid) {
                        out.push(((*key).to_string(), task_slug(&target)));
                    }
                }
            }
        }
        out
    }

    fn vault_agent_event(&self, agent_id: &str, event: &str) {
        if let Some(v) = &self.vault {
            if let Err(e) = v.append_agent_event(agent_id, event) {
                tracing::warn!(error = %e, agent = agent_id, "vault: failed to append agent event");
            }
        }
    }
}

/// Map a task `kind` to its initial state on creation. Announcement
/// kinds are completed-on-create — they're publications, not work to
/// be picked up. Work kinds (bug, feature, task) start pending so an
/// agent can claim them.
fn default_state_for_kind(kind: &str) -> TaskState {
    match kind {
        "ack" | "knowledge" | "decision" => TaskState::Completed,
        _ => TaskState::Pending,
    }
}

/// Human/wikilink-friendly slug for a task — used as the markdown
/// filename and the wikilink target in agent notes.
pub(crate) fn task_slug(task: &Task) -> String {
    let stamp = task.created_at.format("%Y-%m-%d-%H%M");
    let name = slugify(&task.name);
    format!("{stamp}-{name}")
}

pub(crate) fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("untitled");
    }
    out
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let id: String = row.get(0)?;
    let id = Uuid::parse_str(&id).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let kind: String = row.get(2)?;
    let priority: String = row.get(3)?;
    let state: String = row.get(4)?;
    let state = TaskState::parse(&state).unwrap_or(TaskState::Pending);
    let payload: String = row.get(6)?;
    let payload: serde_json::Value =
        serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
    let result: Option<String> = row.get(7)?;
    let result = result.and_then(|s| serde_json::from_str(&s).ok());
    let created_at: String = row.get(8)?;
    let updated_at: String = row.get(9)?;
    let parse_ts = |s: &str| -> rusqlite::Result<chrono::DateTime<Utc>> {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
    };
    Ok(Task {
        id,
        name: row.get(1)?,
        kind,
        priority,
        state,
        claimed_by: row.get(5)?,
        payload,
        result,
        created_at: parse_ts(&created_at)?,
        updated_at: parse_ts(&updated_at)?,
    })
}

fn try_add_column(conn: &Connection, table: &str, column: &str, def: &str) -> Result<()> {
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {def}");
    match conn.execute(&sql, []) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
            if msg.contains("duplicate column name") =>
        {
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    state       TEXT NOT NULL,
    claimed_by  TEXT,
    payload     TEXT NOT NULL,
    result      TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    kind        TEXT NOT NULL DEFAULT 'task',
    priority    TEXT NOT NULL DEFAULT 'normal'
);
CREATE INDEX IF NOT EXISTS idx_tasks_state ON tasks(state);
CREATE INDEX IF NOT EXISTS idx_tasks_claimed_by ON tasks(claimed_by);
CREATE INDEX IF NOT EXISTS idx_tasks_kind_priority ON tasks(kind, priority);

CREATE TABLE IF NOT EXISTS agents (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    last_seen  TEXT NOT NULL,
    first_seen TEXT NOT NULL DEFAULT '1970-01-01T00:00:00Z'
);
"#;
