//! Blocking JSON-RPC 2.0 client for the `coord` daemon. Used by every
//! client subcommand. The async surface is intentionally not exposed
//! here — the CLI runs on a `spawn_blocking` worker.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub struct Client {
    pub url: String,
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn new(url: String) -> Self {
        Self {
            url,
            http: reqwest::blocking::Client::new(),
        }
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = Req {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        };
        let resp: Resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .with_context(|| format!("POST {}", self.url))?
            .json()
            .context("decode response")?;
        if let Some(err) = resp.error {
            return Err(anyhow!("coord error {}: {}", err.code, err.message));
        }
        resp.result.ok_or_else(|| anyhow!("empty response"))
    }
}

#[derive(Serialize)]
struct Req<'a> {
    jsonrpc: &'a str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Deserialize)]
struct Resp {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcErr>,
}

#[derive(Deserialize)]
struct RpcErr {
    code: i32,
    message: String,
}
