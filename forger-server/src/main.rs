use anyhow::Result;
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7420);
    // Loopback only. PORT is honored for the port number, never for binding
    // 0.0.0.0 — this process has no auth.
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let workspace = std::env::var("FORGER_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    forger_server::run(addr, workspace).await
}
