//! buoya-news-agent entry point.
//!

// The lints table denies unwrap/expect crate-wide; tests are allowed to use them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Config fields and helpers are defined ahead of their consumers (scheduler,
// fetchers, scoring — BNM-3/6/7/10). Lift this once those tasks wire them in.
#![allow(dead_code)]

mod config;
mod db;
mod error;
mod fetchers;
mod ingest;
mod llm;
mod state;
mod tui;
mod types;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_openai::{Client, config::OpenAIConfig};
use config::AppConfig;

#[tokio::main]
async fn main() -> ExitCode {
    // The TUI owns the terminal, so logs must go to a file instead of stdout/stderr,
    // otherwise tracing output would corrupt the rendered screen.
    let _log_guard = match init_logging() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("failed to initialize logging: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Route logs to `data/agent.log`. Returns a guard that must stay alive for the
/// duration of the program so buffered logs are flushed.
fn init_logging() -> Result<tracing_appender::non_blocking::WorkerGuard> {
    std::fs::create_dir_all("data").context("failed to create data directory")?;
    let file_appender = tracing_appender::rolling::never("data", "agent.log");
    let (writer, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .init();

    Ok(guard)
}

async fn run() -> Result<()> {
    let cfg = AppConfig::load(Path::new("config.default.toml"))?;

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_millis(cfg.toml_config.http.timeout_ms))
        .user_agent(&cfg.toml_config.http.user_agent)
        .build()?;

    let llm_config = OpenAIConfig::new()
        .with_api_key(&cfg.ai_api_key)
        .with_api_base(&cfg.ai_base_url);

    let llm_client = Client::with_config(llm_config);

    // Init db
    let db_pool = db::init_db().await?;

    let app_state = state::AppState {
        http_client,
        llm_client,
        db_pool,
        config: Arc::new(cfg),
    };

    // Refresh the news in the background so the UI opens immediately.
    let ingest_state = app_state.clone();
    tokio::spawn(async move {
        let new_stored = ingest::run(&ingest_state).await;
        tracing::info!("Ingested {} new items", new_stored);
    });

    // Hand control to the chat TUI.
    tui::run(app_state).await
}
