//! A2A subset + local-loop extensions, all over JSON-RPC 2.0.
//!
//! A2A subset implemented (v0):
//!   - `tasks/send`   — create a task
//!   - `tasks/get`    — fetch task state
//!   - `tasks/cancel` — cancel a task
//!
//! Local-loop extensions:
//!   - `tasks/list`        — list recent tasks (debugging)
//!   - `tasks/claim`       — atomic pending→claimed transition
//!   - `tasks/complete`    — mark a claimed task as completed with a result
//!   - `agents/heartbeat`  — register/refresh an agent's presence
//!   - `agents/list`       — list known agents

use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use crate::core::store::Store;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
}

pub fn router(store: Arc<Store>) -> Router {
    let state = AppState { store };
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
            Ok(serde_json::to_value(&task).unwrap())
        }
        "tasks/get" => {
            let p: IdParams = serde_json::from_value(params).map_err(MethodError::bad_params)?;
            let task = state.store.get_task(p.id).map_err(MethodError::internal)?;
            Ok(serde_json::to_value(&task).unwrap())
        }
        "tasks/list" => {
            let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let kind_filter = params
                .get("kind")
                .and_then(|v| v.as_str())
                .map(String::from);
            let priority_filter = params
                .get("priority")
                .and_then(|v| v.as_str())
                .map(String::from);
            let state_filter = params
                .get("state")
                .and_then(|v| v.as_str())
                .map(String::from);
            let mut tasks = state
                .store
                .list_tasks(limit)
                .map_err(MethodError::internal)?;
            if let Some(k) = kind_filter {
                tasks.retain(|t| t.kind == k);
            }
            if let Some(p) = priority_filter {
                tasks.retain(|t| t.priority == p);
            }
            if let Some(st) = state_filter {
                tasks.retain(|t| t.state.as_str() == st);
            }
            Ok(serde_json::to_value(&tasks).unwrap())
        }
        "tasks/cancel" => {
            let p: IdParams = serde_json::from_value(params).map_err(MethodError::bad_params)?;
            state
                .store
                .cancel_task(p.id)
                .map_err(MethodError::internal)?;
            Ok(json!({ "ok": true }))
        }
        "tasks/claim" => {
            let p: ClaimParams = serde_json::from_value(params).map_err(MethodError::bad_params)?;
            let task = state
                .store
                .claim_task(p.id, &p.agent_id)
                .map_err(MethodError::internal)?;
            match task {
                Some(t) => Ok(serde_json::to_value(&t).unwrap()),
                None => Err(MethodError::conflict(
                    "task is not claimable (already claimed or missing)",
                )),
            }
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
                    "task is not in 'claimed' state — was it cancelled or already completed?",
                ));
            }
            Ok(json!({ "ok": true }))
        }
        "agents/heartbeat" => {
            let p: HeartbeatParams =
                serde_json::from_value(params).map_err(MethodError::bad_params)?;
            state
                .store
                .heartbeat(&p.id, &p.name)
                .map_err(MethodError::internal)?;
            Ok(json!({ "ok": true }))
        }
        "agents/list" => {
            let agents = state.store.list_agents().map_err(MethodError::internal)?;
            Ok(serde_json::to_value(&agents).unwrap())
        }
        _ => Err(MethodError::not_found(method)),
    }
}

async fn agent_card() -> Json<Value> {
    Json(json!({
        "name": "coord",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "A local coordinator for parallel AI coding agents. A2A subset + local extensions.",
        "url": "/",
        "capabilities": {
            "streaming": false,
            "pushNotifications": false
        },
        "skills": [
            { "id": "tasks/send",     "name": "Create a task" },
            { "id": "tasks/get",      "name": "Get task state" },
            { "id": "tasks/cancel",   "name": "Cancel a task" },
            { "id": "tasks/claim",    "name": "Atomic claim (extension)" },
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
