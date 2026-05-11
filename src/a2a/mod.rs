//! JSON-RPC 2.0 surface with an A2A-compatible subset plus local-loop
//! extensions for the bulletin-board model.
//!
//! A2A-compatible methods (subset of A2A v1.0):
//!   - `tasks/send`   — create a task
//!   - `tasks/get`    — fetch task state
//!   - `tasks/cancel` — cancel a task
//!
//! Local-loop extensions (the actual point of coord):
//!   - `tasks/list`        — list tasks with SQL-side filters + optional long-poll
//!   - `tasks/claim`       — atomic pending→claimed transition with a lease
//!   - `tasks/extend`      — push the lease forward (idempotent)
//!   - `tasks/reclaim`     — sweep expired leases back to pending
//!   - `tasks/complete`    — mark a claimed task as completed with a result
//!   - `agents/heartbeat`  — register/refresh an agent's presence
//!   - `agents/list`       — list known agents

use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::core::store::{Store, TaskFilter};

/// Bumped whenever a task or agent row changes state. Wait-style
/// callers subscribe to this channel and unblock the instant a
/// state change might affect their filter — no more 2-second polling.
/// Capacity is small on purpose: lagging receivers re-poll once, then
/// resume waiting on the next event.
#[derive(Clone)]
pub struct ChangeBus {
    tx: broadcast::Sender<()>,
}

impl ChangeBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self { tx }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.tx.subscribe()
    }
    /// Best-effort notify. Returns the receiver count for tests and
    /// observability; an error here just means there are no waiters.
    pub fn notify(&self) -> usize {
        self.tx.send(()).unwrap_or(0)
    }
}

impl Default for ChangeBus {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub bus: ChangeBus,
}

pub fn router(store: Arc<Store>) -> Router {
    router_with_bus(store, ChangeBus::new())
}

pub fn router_with_bus(store: Arc<Store>, bus: ChangeBus) -> Router {
    let state = AppState { store, bus };
    Router::new()
        .route("/", post(handle_rpc))
        .route("/.well-known/agent.json", axum::routing::get(agent_card))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

async fn handle_rpc(
    State(state): State<AppState>,
    Json(req): Json<RpcRequest>,
) -> Json<RpcResponse> {
    let id = req.id.clone();
    let result = dispatch(&state, &req.method, req.params).await;
    Json(match result {
        Ok(value) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(value),
            error: None,
        },
        Err(err) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code: err.code,
                message: err.message,
            }),
        },
    })
}

async fn dispatch(state: &AppState, method: &str, params: Value) -> Result<Value, MethodError> {
    match method {
        "tasks/send" => {
            let p: TaskSendParams =
                serde_json::from_value(params).map_err(MethodError::bad_params)?;
            let task = state
                .store
                .create_task_full(
                    &p.name,
                    p.kind
                        .as_deref()
                        .unwrap_or(crate::core::types::DEFAULT_KIND),
                    p.priority
                        .as_deref()
                        .unwrap_or(crate::core::types::DEFAULT_PRIORITY),
                    p.payload.unwrap_or(Value::Null),
                )
                .map_err(MethodError::internal)?;
            state.bus.notify();
            Ok(serde_json::to_value(&task).unwrap())
        }
        "tasks/get" => {
            let p: IdParams = serde_json::from_value(params).map_err(MethodError::bad_params)?;
            let task = state.store.get_task(p.id).map_err(MethodError::internal)?;
            Ok(serde_json::to_value(&task).unwrap())
        }
        "tasks/list" => tasks_list(state, params).await,
        "tasks/cancel" => {
            let p: IdParams = serde_json::from_value(params).map_err(MethodError::bad_params)?;
            state
                .store
                .cancel_task(p.id)
                .map_err(MethodError::internal)?;
            state.bus.notify();
            Ok(json!({ "ok": true }))
        }
        "tasks/claim" => {
            let p: ClaimParams = serde_json::from_value(params).map_err(MethodError::bad_params)?;
            let task = state
                .store
                .claim_task(p.id, &p.agent_id, p.lease_seconds)
                .map_err(MethodError::internal)?;
            match task {
                Some(t) => {
                    state.bus.notify();
                    Ok(serde_json::to_value(&t).unwrap())
                }
                None => Err(MethodError::conflict(
                    "task is not claimable (already claimed or missing)",
                )),
            }
        }
        "tasks/extend" => {
            let p: ExtendParams =
                serde_json::from_value(params).map_err(MethodError::bad_params)?;
            match state
                .store
                .extend_lease(p.id, &p.agent_id, p.lease_seconds)
                .map_err(MethodError::internal)?
            {
                Some(until) => Ok(json!({ "ok": true, "lease_until": until.to_rfc3339() })),
                None => Err(MethodError::conflict(
                    "lease cannot be extended (not claimed by this agent, or already \
                     completed / cancelled / reclaimed)",
                )),
            }
        }
        "tasks/reclaim" => {
            let reclaimed = state
                .store
                .reclaim_expired_leases()
                .map_err(MethodError::internal)?;
            if !reclaimed.is_empty() {
                state.bus.notify();
            }
            Ok(serde_json::to_value(&reclaimed).unwrap())
        }
        "tasks/complete" => {
            let p: CompleteParams =
                serde_json::from_value(params).map_err(MethodError::bad_params)?;
            let ok = state
                .store
                .complete_task(p.id, p.result.unwrap_or(Value::Null))
                .map_err(MethodError::internal)?;
            if !ok {
                return Err(MethodError::conflict(
                    "task is not in 'claimed' state — was it cancelled, already completed, \
                     or reclaimed after its lease expired?",
                ));
            }
            state.bus.notify();
            Ok(json!({ "ok": true }))
        }
        "agents/heartbeat" => {
            let p: HeartbeatParams =
                serde_json::from_value(params).map_err(MethodError::bad_params)?;
            state
                .store
                .heartbeat(&p.id, &p.name)
                .map_err(MethodError::internal)?;
            // Heartbeats don't change task state, so they intentionally
            // do not poke the change bus — that would wake every
            // waiter every few seconds for no useful reason.
            Ok(json!({ "ok": true }))
        }
        "agents/list" => {
            let agents = state.store.list_agents().map_err(MethodError::internal)?;
            Ok(serde_json::to_value(&agents).unwrap())
        }
        _ => Err(MethodError::not_found(method)),
    }
}

/// `tasks/list` with optional long-poll. If `wait_ms` is set and the
/// initial query returns no rows, the call subscribes to the change
/// bus and re-queries on the first state change (or on timeout). The
/// reclaim sweep runs first so a freshly-expired claim shows up as
/// pending without callers having to poke `tasks/reclaim` manually.
async fn tasks_list(state: &AppState, params: Value) -> Result<Value, MethodError> {
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
    let filter = TaskFilter {
        kind: params
            .get("kind")
            .and_then(|v| v.as_str())
            .map(String::from),
        priority: params
            .get("priority")
            .and_then(|v| v.as_str())
            .map(String::from),
        state: params
            .get("state")
            .and_then(|v| v.as_str())
            .map(String::from),
    };
    let wait_ms = params.get("wait_ms").and_then(|v| v.as_u64()).unwrap_or(0);

    // Lazy self-heal: expired claims surface as `pending` without a
    // separate operator action. Cheap (indexed range scan) and bounded
    // by the bus capacity above.
    let _ = state.store.reclaim_expired_leases();

    let tasks = state
        .store
        .list_tasks_filtered(limit, &filter)
        .map_err(MethodError::internal)?;
    if wait_ms == 0 || !tasks.is_empty() {
        return Ok(serde_json::to_value(&tasks).unwrap());
    }

    // Empty initial result + caller wants to wait: subscribe to the
    // bus, then re-query on the first change-event or on timeout.
    // Cap the per-call wait at 60s so a stuck client can't pin a
    // request handler forever and so the heartbeat ceiling stays
    // predictable.
    let wait = Duration::from_millis(wait_ms.min(60_000));
    let mut rx = state.bus.subscribe();
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(json!([]));
        }
        let recv = tokio::time::timeout(remaining, rx.recv()).await;
        match recv {
            // Either a state change happened, or our subscription
            // lagged. Both cases want a fresh query — if there's still
            // nothing, fall back to waiting again until the deadline.
            Ok(Ok(())) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                let _ = state.store.reclaim_expired_leases();
                let tasks = state
                    .store
                    .list_tasks_filtered(limit, &filter)
                    .map_err(MethodError::internal)?;
                if !tasks.is_empty() {
                    return Ok(serde_json::to_value(&tasks).unwrap());
                }
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => return Ok(json!([])),
            Err(_) => return Ok(json!([])),
        }
    }
}

async fn agent_card() -> Json<Value> {
    Json(json!({
        "name": "coord",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Local coordinator for parallel AI coding agents. JSON-RPC 2.0 with \
                        an A2A-compatible tasks/send · tasks/get · tasks/cancel subset, plus \
                        local-loop extensions: atomic claim with leases, blocking long-poll \
                        on tasks/list, agent presence.",
        "url": "/",
        "capabilities": {
            "streaming": false,
            "pushNotifications": false,
            "longPoll": true,
            "leasedClaims": true
        },
        "compatibility": {
            "a2a": {
                "version": "subset",
                "notes": "Implements tasks/send, tasks/get, tasks/cancel from A2A v1.0. \
                          Streamable HTTP, signed Agent Cards, and push notifications are \
                          not implemented."
            }
        },
        "skills": [
            { "id": "tasks/send",     "name": "Create a task" },
            { "id": "tasks/get",      "name": "Get task state" },
            { "id": "tasks/cancel",   "name": "Cancel a task" },
            { "id": "tasks/list",     "name": "List / long-poll tasks (extension)" },
            { "id": "tasks/claim",    "name": "Atomic leased claim (extension)" },
            { "id": "tasks/extend",   "name": "Extend a claim's lease (extension)" },
            { "id": "tasks/reclaim",  "name": "Sweep expired leases (extension)" },
            { "id": "tasks/complete", "name": "Complete a claimed task (extension)" }
        ]
    }))
}

#[derive(Debug, Deserialize)]
struct TaskSendParams {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    payload: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct IdParams {
    id: Uuid,
}

#[derive(Debug, Deserialize)]
struct ClaimParams {
    id: Uuid,
    #[serde(rename = "agentId")]
    agent_id: String,
    /// Optional lease length for this claim, in seconds. Defaults to
    /// `DEFAULT_LEASE_SECONDS` and is clamped to `MAX_LEASE_SECONDS`
    /// inside the store. Pass a longer lease if you know your step is
    /// going to take a while; otherwise extend periodically.
    #[serde(default, rename = "leaseSeconds")]
    lease_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ExtendParams {
    id: Uuid,
    #[serde(rename = "agentId")]
    agent_id: String,
    #[serde(default, rename = "leaseSeconds")]
    lease_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CompleteParams {
    id: Uuid,
    #[serde(default)]
    result: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct HeartbeatParams {
    id: String,
    name: String,
}

#[derive(Debug)]
struct MethodError {
    code: i32,
    message: String,
}

impl MethodError {
    fn bad_params<E: std::fmt::Display>(e: E) -> Self {
        Self {
            code: -32602,
            message: format!("invalid params: {e}"),
        }
    }
    fn not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }
    fn internal<E: std::fmt::Display>(e: E) -> Self {
        Self {
            code: -32000,
            message: format!("internal error: {e}"),
        }
    }
    fn conflict(msg: &str) -> Self {
        Self {
            code: -32001,
            message: msg.into(),
        }
    }
}
