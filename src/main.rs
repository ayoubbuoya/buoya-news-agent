//! buoya-news-mcp entry point.
//!
//! Minimal slice: load config, open the SQLite store, and serve two MCP tools
//! (`get_briefing`, `search_news`) over stdio. RSS feeds are fetched lazily on
//! the first briefing call. The background scheduler and the remaining sources
//! and tools (§7–8 of the spec) come later.

// Tests are allowed unwrap/expect; the crate lints deny them elsewhere.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Some config fields and domain variants are defined ahead of the tasks that
// consume them (richer scoring, more sources). Lift this as they get wired in.
#![allow(dead_code)]

mod config;
mod db;
mod domain;
mod error;
mod fetchers;
mod ingest;
mod pipeline;
mod server;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::transport::stdio;

use config::AppConfig;
use db::Db;
use ingest::AppState;
use server::NewsServer;

const DB_PATH: &str = "data/buoya.db";

#[tokio::main]
async fn main() -> ExitCode {
    // Logs go to stderr; stdout is reserved for the MCP protocol.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let cfg = AppConfig::load(
        Path::new("config.default.toml"),
        Path::new("config.toml"),
    )?;

    if let Some(parent) = Path::new(DB_PATH).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = Arc::new(Db::open(DB_PATH)?);

    let http = reqwest::Client::builder()
        .user_agent(cfg.http.user_agent.clone())
        .timeout(Duration::from_millis(cfg.http.timeout_ms))
        .build()?;

    let state = AppState {
        db,
        config: Arc::new(cfg),
        http,
    };

    tracing::info!(
        feeds = state.config.sources.rss.len(),
        "starting buoya-news-mcp on stdio"
    );

    let service = NewsServer::new(state).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
