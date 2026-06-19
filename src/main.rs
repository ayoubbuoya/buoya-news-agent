//! buoya-news-agent entry point.
//!

// The lints table denies unwrap/expect crate-wide; tests are allowed to use them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Config fields and helpers are defined ahead of their consumers (scheduler,
// fetchers, scoring — BNM-3/6/7/10). Lift this once those tasks wire them in.
#![allow(dead_code)]

mod core;
mod server;
mod tui;

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use core::Core;
use core::config::AppConfig;
use server::ServeArgs;

/// buoya-news-agent: a local news agent over crypto, DeFi, AI, security, and markets.
#[derive(Debug, Parser)]
#[command(name = "buoya", version, about)]
struct Cli {
    #[command(subcommand)]
    mode: Option<Mode>,
}

/// Which surface to run. Defaults to the interactive TUI when omitted.
#[derive(Debug, Subcommand)]
enum Mode {
    /// Run the interactive terminal chat UI (default).
    Tui,
    /// Run the HTTP backend: REST + SSE for a frontend, and MCP at /mcp.
    Serve(ServeArgs),
}

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
    let cli = Cli::parse();

    let cfg = AppConfig::load(Path::new("config.default.toml"))?;

    // Build the core (shared state + background ingest/backfill tasks) once, then
    // hand it to the selected front-end adapter.
    let core = Core::start(cfg).await?;

    match cli.mode {
        None | Some(Mode::Tui) => tui::run(core).await,
        Some(Mode::Serve(args)) => server::run(core, args).await,
    }
}
