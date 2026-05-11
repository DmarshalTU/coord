//! SQLite-backed state. All mutations go through atomic transactions so
//! that task claims are race-free across many connected agents.
//!
//! Optionally writes a human-readable markdown audit trail to a vault
//! directory on every state change. Drop the vault into Obsidian to get
//! a graph view of the project's nervous system for free.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use super::types::{
    Agent, Task, TaskState, DEFAULT_KIND, DEFAULT_LEASE_SECONDS, DEFAULT_PRIORITY,
    MAX_LEASE_SECONDS,
};
use crate::vault::Vault;

/// SQL-side filter applied to `list_tasks_filtered`. Each `Some` field
/// becomes a `WHERE` clause so the database returns only matching rows
/// rather than the most-recent N rows that we then re-filter in
/// memory. The latter silently drops matches when the bulletin is
/// busy — see the 0.4 SQL-pushdown work for the regression test.
#[derive(Default, Debug, Clone)]
pub struct TaskFilter {
    pub state: Option<String>,
    pub kind: Option<String>,
    pub priority: Option<String>,
}

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
        // Lease columns are nullable: unclaimed tasks have no lease, and
        // legacy rows from before the migration keep NULLs (the
        // reclaim sweep treats NULL as "no lease, don't touch").
        try_add_column(&conn, "tasks", "claimed_at", "TEXT")?;
        try_add_column(&conn, "tasks", "lease_until", "TEXT")?;
        // `first_seen` defaults to epoch on existing rows; the next heartbeat
        // for that agent will not overwrite it (see UPSERT in `heartbeat`).
        try_add_column(
            &conn,
            "agents",
            "first_seen",
            "TEXT NOT NULL DEFAULT '1970-01-01T00:00:00Z'",
        )?;
        // Index just `lease_until` so the periodic reclaim sweep is a
        // cheap range scan even when the bulletin has thousands of
        // completed/cancelled rows that don't carry a lease.
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tasks_lease_until ON tasks(lease_until)",
            [],
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
            claimed_at: None,
            lease_until: None,
        };
        self.vault_write(&task);
        Ok(task)
    }

    pub fn get_task(&self, id: Uuid) -> Result<Option<Task>> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT id, name, kind, priority, state, claimed_by, payload, result, \
                 created_at, updated_at, claimed_at, lease_until \
                 FROM tasks WHERE id = ?1",
                params![id.to_string()],
                row_to_task,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_tasks(&self, limit: usize) -> Result<Vec<Task>> {
        self.list_tasks_filtered(limit, &TaskFilter::default())
    }

    /// Filtered list. Filters are applied in SQL so callers receive
    /// matching rows up to `limit`, not just the N most-recent rows
    /// re-filtered in memory (which silently drops matches when the
    /// bulletin is busy).
    pub fn list_tasks_filtered(&self, limit: usize, filter: &TaskFilter) -> Result<Vec<Task>> {
        let conn = self.conn.lock();
        let mut sql = String::from(
            "SELECT id, name, kind, priority, state, claimed_by, payload, result, \
             created_at, updated_at, claimed_at, lease_until \
             FROM tasks",
        );
        let mut clauses: Vec<&'static str> = Vec::new();
        let mut bind: Vec<String> = Vec::new();
        if let Some(s) = &filter.state {
            clauses.push("state = ?");
            bind.push(s.clone());
        }
        if let Some(k) = &filter.kind {
            clauses.push("kind = ?");
            bind.push(k.clone());
        }
        if let Some(p) = &filter.priority {
            clauses.push("priority = ?");
            bind.push(p.clone());
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ?");
        let mut stmt = conn.prepare(&sql)?;
        let mut bind_refs: Vec<&dyn rusqlite::ToSql> =
            bind.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let limit_i = limit as i64;
        bind_refs.push(&limit_i);
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bind_refs), row_to_task)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Atomically transition a `pending` task to `claimed` for `agent_id`,
    /// granting a fresh lease of `lease_seconds` (clamped to
    /// `MAX_LEASE_SECONDS`). Returns the claimed task on success, or
    /// `None` if it was already claimed.
    pub fn claim_task(
        &self,
        id: Uuid,
        agent_id: &str,
        lease_seconds: Option<u64>,
    ) -> Result<Option<Task>> {
        let now = Utc::now();
        let lease = clamp_lease(lease_seconds.unwrap_or(DEFAULT_LEASE_SECONDS));
        let lease_until = now + Duration::seconds(lease as i64);
        let task_opt = {
            let conn = self.conn.lock();
            let updated = conn.execute(
                "UPDATE tasks SET state = 'claimed', claimed_by = ?1, claimed_at = ?2,
                                  lease_until = ?3, updated_at = ?2
                 WHERE id = ?4 AND state = 'pending'",
                params![
                    agent_id,
                    now.to_rfc3339(),
                    lease_until.to_rfc3339(),
                    id.to_string(),
                ],
            )?;
            if updated == 0 {
                None
            } else {
                Some(conn.query_row(
                    "SELECT id, name, kind, priority, state, claimed_by, payload, result, \
                     created_at, updated_at, claimed_at, lease_until \
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

    /// Push the lease forward on a currently-claimed task. Idempotent
    /// and race-safe: only the current claimer can extend, and only
    /// while the task is still `claimed`. Returns the new
    /// `lease_until` on success, or `None` if the agent no longer
    /// owns the claim (cancelled, completed, reclaimed, or another
    /// agent took over after a reclaim).
    pub fn extend_lease(
        &self,
        id: Uuid,
        agent_id: &str,
        lease_seconds: Option<u64>,
    ) -> Result<Option<DateTime<Utc>>> {
        let now = Utc::now();
        let lease = clamp_lease(lease_seconds.unwrap_or(DEFAULT_LEASE_SECONDS));
        let lease_until = now + Duration::seconds(lease as i64);
        let updated = {
            let conn = self.conn.lock();
            conn.execute(
                "UPDATE tasks SET lease_until = ?1, updated_at = ?2
                 WHERE id = ?3 AND state = 'claimed' AND claimed_by = ?4",
                params![
                    lease_until.to_rfc3339(),
                    now.to_rfc3339(),
                    id.to_string(),
                    agent_id,
                ],
            )?
        };
        if updated == 0 {
            return Ok(None);
        }
        if let Some(t) = self.get_task(id)? {
            self.vault_write(&t);
        }
        Ok(Some(lease_until))
    }

    /// Sweep claimed tasks whose lease has expired back to `pending`,
    /// clearing `claimed_by` and lease fields so another agent can pick
    /// them up. Called periodically by the daemon and on every
    /// `tasks/list` to keep the bulletin self-healing. Returns the
    /// reclaimed tasks (in their post-reclaim `pending` shape) so
    /// callers can log / write vault notes.
    pub fn reclaim_expired_leases(&self) -> Result<Vec<Task>> {
        let now = Utc::now();
        let reclaimed = {
            let conn = self.conn.lock();
            // Snapshot the to-be-reclaimed rows first so we can write
            // vault notes that record who abandoned what. The actual
            // state transition is a single UPDATE for atomicity.
            let mut stmt = conn.prepare(
                "SELECT id, name, kind, priority, state, claimed_by, payload, result, \
                 created_at, updated_at, claimed_at, lease_until \
                 FROM tasks \
                 WHERE state = 'claimed' AND lease_until IS NOT NULL AND lease_until < ?1",
            )?;
            let snapshot: Vec<Task> = stmt
                .query_map(params![now.to_rfc3339()], row_to_task)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if snapshot.is_empty() {
                Vec::new()
            } else {
                conn.execute(
                    "UPDATE tasks SET state = 'pending', claimed_by = NULL, \
                                       claimed_at = NULL, lease_until = NULL, \
                                       updated_at = ?1 \
                     WHERE state = 'claimed' AND lease_until IS NOT NULL AND lease_until < ?1",
                    params![now.to_rfc3339()],
                )?;
                // Re-fetch the post-reclaim shape (state='pending',
                // claimed_by=NULL) so the returned vec matches what
                // any subsequent `tasks/list` would show.
                let ids: Vec<String> = snapshot.iter().map(|t| t.id.to_string()).collect();
                let mut out = Vec::with_capacity(ids.len());
                for id in &ids {
                    let t = conn.query_row(
                        "SELECT id, name, kind, priority, state, claimed_by, payload, result, \
                         created_at, updated_at, claimed_at, lease_until \
                         FROM tasks WHERE id = ?1",
                        params![id],
                        row_to_task,
                    )?;
                    out.push((snapshot.iter().find(|s| &s.id.to_string() == id).cloned(), t));
                }
                out
            }
        };
        let mut returned = Vec::with_capacity(reclaimed.len());
        for (before, after) in reclaimed {
            if let Some(before) = before {
                if let Some(agent) = &before.claimed_by {
                    self.vault_agent_event(
                        agent,
                        &format!(
                            "abandoned [[{}]] (lease expired at {})",
                            task_slug(&before),
                            before
                                .lease_until
                                .map(|d| d.to_rfc3339())
                                .unwrap_or_else(|| "unknown".into()),
                        ),
                    );
                }
                tracing::info!(
                    task = %before.id,
                    agent = ?before.claimed_by,
                    "reclaimed expired lease"
                );
            }
            self.vault_write(&after);
            returned.push(after);
        }
        Ok(returned)
    }

    /// Mark a claimed task complete. Idempotent against races: only
    /// transitions when current state is `claimed`, so a `cancel` that
    /// sneaks in first is sticky. Clears the lease so the row no
    /// longer trips the reclaim sweep. Returns true on success.
    pub fn complete_task(&self, id: Uuid, result: serde_json::Value) -> Result<bool> {
        let now = Utc::now();
        let result_s = serde_json::to_string(&result)?;
        let updated = {
            let conn = self.conn.lock();
            conn.execute(
                "UPDATE tasks SET state = 'completed', result = ?1, lease_until = NULL,
                                  updated_at = ?2
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
                "UPDATE tasks SET state = 'cancelled', lease_until = NULL, updated_at = ?1
                 WHERE id = ?2",
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

/// Clamp a requested lease length to a sane range. Zero (and anything
/// suspiciously small) bumps up to a few seconds — otherwise an agent
/// could claim a task and have it auto-reclaimed before its next
/// instruction. The high end is capped to keep the bulletin healable
/// even when an agent forgets to extend.
fn clamp_lease(secs: u64) -> u64 {
    secs.clamp(1, MAX_LEASE_SECONDS)
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
    let claimed_at: Option<String> = row.get(10).ok();
    let lease_until: Option<String> = row.get(11).ok();
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
    let parse_opt_ts = |s: Option<String>| -> rusqlite::Result<Option<DateTime<Utc>>> {
        match s {
            Some(s) if !s.is_empty() => parse_ts(&s).map(Some),
            _ => Ok(None),
        }
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
        claimed_at: parse_opt_ts(claimed_at)?,
        lease_until: parse_opt_ts(lease_until)?,
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
    priority    TEXT NOT NULL DEFAULT 'normal',
    claimed_at  TEXT,
    lease_until TEXT
);
CREATE INDEX IF NOT EXISTS idx_tasks_state ON tasks(state);
CREATE INDEX IF NOT EXISTS idx_tasks_claimed_by ON tasks(claimed_by);
CREATE INDEX IF NOT EXISTS idx_tasks_kind_priority ON tasks(kind, priority);
CREATE INDEX IF NOT EXISTS idx_tasks_lease_until ON tasks(lease_until);

CREATE TABLE IF NOT EXISTS agents (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    last_seen  TEXT NOT NULL,
    first_seen TEXT NOT NULL DEFAULT '1970-01-01T00:00:00Z'
);
"#;
