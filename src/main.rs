//! buoya-news-agent entry point.
//!

// The lints table denies unwrap/expect crate-wide; tests are allowed to use them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Config fields and helpers are defined ahead of their consumers (scheduler,
// fetchers, scoring — BNM-3/6/7/10). Lift this once those tasks wire them in.
#![allow(dead_code)]

mod config;
mod error;
mod fetchers;
mod llm;
mod state;
mod types;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_openai::{Client, config::OpenAIConfig};
use config::AppConfig;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let cfg = AppConfig::load(Path::new("config.default.toml"));

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_millis(cfg.toml_config.http.timeout_ms))
        .user_agent(&cfg.toml_config.http.user_agent)
        .build()?;

    let llm_config = OpenAIConfig::new()
        .with_api_key(&cfg.ai_api_key)
        .with_api_base(&cfg.ai_base_url);

    let llm_client = Client::with_config(llm_config);

    let app_state = state::AppState {
        http_client,
        llm_client,
        config: Arc::new(cfg),
    };

    println!("App state: {app_state:?}");

    // Test the fetch rss source
    let raw_items = fetchers::rss::fetch_rss_source(
        &app_state.http_client,
        &app_state.config.toml_config.sources.rss[0],
    )
    .await?;

    tracing::info!("First Rss Feed Raw Items: {raw_items:?}");

    let llm_model = "openai/gpt-oss-20b:free";

    let prompt = "What happened in crypto today ?";

    let response = llm::prompt(&app_state, prompt, llm_model).await?;

    tracing::info!("LLM Response: {response}");

    Ok(())
}
