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
