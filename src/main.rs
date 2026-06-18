//! buoya-news-agent entry point.
//!

// The lints table denies unwrap/expect crate-wide; tests are allowed to use them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Config fields and helpers are defined ahead of their consumers (scheduler,
// fetchers, scoring — BNM-3/6/7/10). Lift this once those tasks wire them in.
#![allow(dead_code)]

mod config;
mod core;
mod db;
mod embeddings;
mod error;
mod fetchers;
mod ingest;
mod llm;
mod tui;
mod types;

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use config::AppConfig;
use core::Core;

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

    // Build the core (shared state + background ingest/backfill tasks), then hand
    // it to a front-end adapter. Today that is always the TUI; later this is where
    // a mode flag selects the HTTP server, MCP server, or connectors.
    let core = Core::start(cfg).await?;

    tui::run(core).await
}
