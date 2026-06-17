//! Ingest orchestration: fetch every enabled RSS feed, score each item, and
//! store it. One feed failing is logged and skipped, never aborting the run.

use std::sync::Arc;

use crate::config::AppConfig;
use crate::db::Db;
use crate::fetchers;
use crate::pipeline::{self, Keyword};

/// Shared, cheaply-cloneable handle passed to the MCP server and ingest.
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub config: Arc<AppConfig>,
    pub http: reqwest::Client,
}

/// Fetch all enabled RSS feeds and persist new items. Returns the number of
/// newly-stored items.
pub async fn run(state: &AppState) -> usize {
    let keywords: Vec<Keyword<'_>> = state
        .config
        .scoring
        .keywords
        .iter()
        .map(|k| Keyword {
            term: &k.term,
            weight: k.weight,
        })
        .collect();

    let mut stored = 0usize;
    for source in &state.config.sources.rss {
        match fetchers::fetch_rss(&state.http, source).await {
            Ok(raws) => {
                for raw in &raws {
                    let (item, canonical) = pipeline::build(raw, &keywords);
                    match state.db.upsert(&item, &canonical) {
                        Ok(true) => stored += 1,
                        Ok(false) => {}
                        Err(e) => tracing::warn!(source = %source.name, error = %e, "store failed"),
                    }
                }
                tracing::info!(source = %source.name, fetched = raws.len(), "feed ingested");
            }
            Err(e) => tracing::warn!(source = %source.name, error = %e, "feed fetch failed"),
        }
    }
    tracing::info!(stored, "ingest run complete");
    stored
}
