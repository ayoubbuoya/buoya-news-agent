//! buoya-news-agent entry point.
//!

// The lints table denies unwrap/expect crate-wide; tests are allowed to use them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Config fields and helpers are defined ahead of their consumers (scheduler,
// fetchers, scoring — BNM-3/6/7/10). Lift this once those tasks wire them in.
#![allow(dead_code)]

mod config;
mod db;
mod embeddings;
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

/// Register the sqlite-vec extension so every SQLite connection sqlx opens has the
/// `vec0` virtual table and `vec_*` functions available. Must run before any
/// connection/pool is created.
fn register_sqlite_vec() {
    // SAFETY: `sqlite3_auto_extension` registers the sqlite-vec entrypoint for all
    // connections opened later by this process's SQLite. Called exactly once, before
    // the pool is built. The transmute adapts sqlite-vec's init fn to the C ABI
    // signature sqlite expects, which is the documented registration pattern.
    #[allow(unsafe_code)]
    unsafe {
        libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut libsqlite3_sys::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const libsqlite3_sys::sqlite3_api_routines,
            ) -> std::os::raw::c_int,
        >(sqlite_vec::sqlite3_vec_init as *const ())));
    }
}

async fn run() -> Result<()> {
    let cfg = AppConfig::load(Path::new("config.default.toml"))?;

    register_sqlite_vec();

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

    // Load the embedding model off the async runtime; on first run this downloads
    // weights (~130 MB) and caches them on disk.
    tracing::info!("loading embedding model (first run downloads weights)…");
    let embedder = tokio::task::spawn_blocking(embeddings::Embedder::load)
        .await
        .context("embedder load task panicked")??;

    let app_state = state::AppState {
        http_client,
        llm_client,
        db_pool,
        config: Arc::new(cfg),
        embedder: Arc::new(embedder),
    };

    // Backfill embeddings for any articles indexed before semantic search existed.
    // Runs in the background so the UI opens immediately; no-op once caught up.
    let backfill_state = app_state.clone();
    tokio::spawn(async move {
        ingest::backfill_embeddings(&backfill_state).await;
    });

    // Refresh the news in the background so the UI opens immediately, then keep
    // re-ingesting on the configured interval.
    let ingest_state = app_state.clone();

    let ingest_period =
        Duration::from_secs(ingest_state.config.toml_config.general.ingest_interval_secs);

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(ingest_period);
        // Default `MissedTickBehavior::Burst` would fire back-to-back if a run
        // overruns the period; skip stale ticks so we resume on schedule instead.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            // First tick completes immediately, so ingest runs once at startup.
            ticker.tick().await;
            let new_stored = ingest::run(&ingest_state).await;
            tracing::info!("Ingested {} new items", new_stored);
        }
    });

    // Hand control to the chat TUI.
    tui::run(app_state).await
}
