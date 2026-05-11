//! Server-side actions: start the daemon, run the MCP bridge, print
//! version metadata. The CLI in [`crate::cli`] wraps these.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use coord::a2a;
use coord::core::store::Store;
use coord::mcp::Bridge;

/// How often the background ticker calls `reclaim_expired_leases`.
/// Short enough that a dead agent doesn't hold a task for long;
/// long enough that the sweep is cheap on a busy bulletin.
const RECLAIM_TICK: Duration = Duration::from_secs(30);

pub async fn serve(addr: SocketAddr, db: Option<PathBuf>, vault: Option<PathBuf>) -> Result<()> {
    let db_path = db.unwrap_or_else(default_db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    tracing::info!(?db_path, ?vault, "opening state");
    let store = Arc::new(Store::open_with_vault(&db_path, vault)?);
    let bus = a2a::ChangeBus::new();
    let app = a2a::router_with_bus(store.clone(), bus.clone());

    // Background reclaim sweep. Lazy reclaim on `tasks/list` keeps the
    // bulletin self-healing for active readers, but the ticker covers
    // the case where nobody is reading: a stuck task should still
    // un-stick within `RECLAIM_TICK`. Each sweep notifies the bus so
    // any long-pollers wake up immediately when a reclaim happens.
    let ticker_store = store.clone();
    let ticker_bus = bus.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(RECLAIM_TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match ticker_store.reclaim_expired_leases() {
                Ok(reclaimed) if !reclaimed.is_empty() => {
                    tracing::info!(
                        count = reclaimed.len(),
                        "reclaim_ticker: returned expired claims to pending"
                    );
                    ticker_bus.notify();
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "reclaim_ticker failed"),
            }
        }
    });

    tracing::info!(%addr, "coord listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

pub async fn mcp(url: String) -> Result<()> {
    Bridge::new(url).run().await
}

pub fn print_version() {
    println!("coord {}", env!("CARGO_PKG_VERSION"));
    println!("a2a   subset (tasks/send, tasks/get, tasks/cancel)");
    println!(
        "ext   tasks/claim, tasks/extend, tasks/reclaim, tasks/complete, tasks/list (long-poll)"
    );
    println!("mcp   bridge   (use `coord mcp` for stdio)");
    println!("vault markdown (use `coord serve --vault PATH` to enable)");
    println!("kinds task | bug | feature | decision | ack | knowledge | build");
    println!("prio  low | normal | high | urgent");
    println!("lease 300s default · 3600s max · auto-reclaim every 30s");
}

fn default_db_path() -> PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("dev", "coord", "coord") {
        return dirs.data_dir().join("state.db");
    }
    PathBuf::from(".coord/state.db")
}
