//! Server-side actions: start the daemon, run the MCP bridge, print
//! version metadata. The CLI in [`crate::cli`] wraps these.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use coord::a2a;
use coord::core::store::Store;
use coord::mcp::Bridge;

pub async fn serve(addr: SocketAddr, db: Option<PathBuf>, vault: Option<PathBuf>) -> Result<()> {
    let db_path = db.unwrap_or_else(default_db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    tracing::info!(?db_path, ?vault, "opening state");
    let store = Arc::new(Store::open_with_vault(&db_path, vault)?);
    let app = a2a::router(store);

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
    println!("a2a   subset 0.1");
    println!("mcp   bridge   (use `coord mcp` for stdio)");
    println!("vault markdown (use `coord serve --vault PATH` to enable)");
    println!("kinds task | bug | feature | decision | ack | knowledge | build");
    println!("prio  low | normal | high | urgent");
}

fn default_db_path() -> PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("dev", "coord", "coord") {
        return dirs.data_dir().join("state.db");
    }
    PathBuf::from(".coord/state.db")
}
