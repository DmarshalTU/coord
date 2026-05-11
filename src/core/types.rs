//! Wire types shared between the HTTP layer and the store.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Pending,
    Claimed,
    Completed,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "claimed" => Self::Claimed,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

/// Free-form task category. Common values: `task` (default), `bug`,
/// `feature`, `decision`, `ack`, `knowledge`, `build`. The set is open
/// so callers can add new kinds without a server upgrade.
pub const DEFAULT_KIND: &str = "task";

/// Free-form priority. Common values: `low`, `normal` (default), `high`,
/// `urgent`. The TUI colors rows based on these strings; unknown values
/// render as default.
pub const DEFAULT_PRIORITY: &str = "normal";

/// Default lease length for a fresh claim, in seconds. Short on purpose:
/// the cost of "too short" is one extra `tasks/extend` RPC per minute
/// (cheap), the cost of "too long" is a stuck task blocking the
/// bulletin for the whole window. Callers can override per-claim and
/// operators can override the daemon default via `COORD_DEFAULT_LEASE`.
pub const DEFAULT_LEASE_SECONDS: u64 = 300;

/// Hard ceiling on a single lease. Prevents agents from accidentally
/// requesting hours-long leases that would defeat the auto-reclaim
/// loop. Callers that need this much wall-clock should re-extend.
pub const MAX_LEASE_SECONDS: u64 = 3_600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub priority: String,
    pub state: TaskState,
    pub claimed_by: Option<String>,
    pub payload: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the current claim was last (re-)granted. `None` for tasks
    /// that have never been claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<DateTime<Utc>>,
    /// Wall-clock deadline after which a claim is considered abandoned
    /// and the task is eligible for auto-reclaim back to `pending`.
    /// `None` for unclaimed tasks and for legacy rows from before
    /// the lease migration ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// First time this agent ID heartbeated against this database. Used
    /// to render uptime in the TUI (a monotonically growing counter).
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub current_task: Option<Uuid>,
}
