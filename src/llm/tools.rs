//! Tool definitions the LLM can call to read stored news articles, plus the
//! dispatch logic that runs a requested tool against the database.
//!
//! Tools follow the OpenAI function-calling shape: each has a JSON-Schema
//! parameter spec the model fills in, and [`execute`] runs the matching query
//! and returns a JSON string the model reads back as the tool result.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_openai::types::chat::{ChatCompletionTool, ChatCompletionTools, FunctionObject};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::core::repository::Repository;
use crate::embeddings::Embedder;

/// Default number of articles returned by list/search tools when the model does
/// not specify a limit.
const DEFAULT_LIMIT: i64 = 20;
/// Hard cap on rows returned so a single tool call cannot flood the context.
const MAX_LIMIT: i64 = 50;

/// The set of tools advertised to the model on every request.
pub fn tool_definitions() -> Vec<ChatCompletionTools> {
    vec![
        function_tool(
            "semantic_search",
            "Semantic (meaning-based) search over stored news articles using vector \
             similarity. Finds relevant articles even when they don't share the \
             exact words as the query. Prefer this for conceptual or topical \
             questions, e.g. \"regulatory risk for stablecoins\" or \"layer-2 \
             scaling progress\". For an exact ticker or proper name, prefer \
             search_articles instead. Lower distance means more relevant.",
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural-language description of what you're looking for."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of articles to return (1-50).",
                        "minimum": 1,
                        "maximum": MAX_LIMIT
                    }
                },
                "required": ["query"]
            }),
        ),
        function_tool(
            "search_articles",
            "Exact keyword/substring search over stored news articles. Matches the \
             query literally against article titles, summaries, and body content. \
             Best for exact tickers or proper names (e.g. \"HBAR\", \"Coinbase\"). \
             For conceptual or topical questions, prefer semantic_search.",
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keywords or phrase to search for, e.g. \"ethereum etf\"."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of articles to return (1-25).",
                        "minimum": 1,
                        "maximum": MAX_LIMIT
                    }
                },
                "required": ["query"]
            }),
        ),
        function_tool(
            "list_recent_articles",
            "List the most recently published stored articles, optionally \
             filtered by category. Use this when the user asks what's new or \
             what's happening in a given area.",
            json!({
                "type": "object",
                "properties": {
                    "category": {
                        "type": "string",
                        "description": "Restrict to a single category.",
                        "enum": ["crypto", "ai", "security", "market", "defi"]
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of articles to return (1-25).",
                        "minimum": 1,
                        "maximum": MAX_LIMIT
                    }
                },
                "required": []
            }),
        ),
        function_tool(
            "get_article",
            "Fetch the full stored record for a single article by its numeric \
             id, including the body content. Use this after search or list to \
             read an article in depth.",
            json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "integer",
                        "description": "The article id, as returned by search_articles or list_recent_articles."
                    }
                },
                "required": ["id"]
            }),
        ),
        function_tool(
            "get_market_snapshot",
            "Get the latest structured market snapshots: the crypto Fear & Greed \
             sentiment index, a top-coins-by-market-cap overview with 24h moves, \
             and total DeFi TVL by chain. Use this for questions about market \
             sentiment/mood, current prices or movers, or DeFi TVL — not \
             search_articles. Returns the most recent daily snapshot for each.",
            json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        ),
    ]
}

/// Build a single function tool from its name, description, and JSON-Schema
/// parameter object.
fn function_tool(name: &str, description: &str, parameters: Value) -> ChatCompletionTools {
    ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObject {
            name: name.to_string(),
            description: Some(description.to_string()),
            parameters: Some(parameters),
            strict: None,
        },
    })
}

/// Run the tool named `name` with the raw JSON `arguments` string the model
/// produced. Always returns a string for the model: on failure it returns a
/// JSON object with an `error` field rather than propagating, so a bad tool call
/// becomes feedback the model can recover from instead of aborting the turn.
pub async fn execute(
    pool: &SqlitePool,
    embedder: &Arc<Embedder>,
    name: &str,
    arguments: &str,
) -> String {
    let repo = Repository::new(pool.clone(), embedder.clone());
    let result = match name {
        "semantic_search" => semantic_search(&repo, arguments).await,
        "search_articles" => search_articles(&repo, arguments).await,
        "list_recent_articles" => list_recent_articles(&repo, arguments).await,
        "get_article" => get_article(&repo, arguments).await,
        "get_market_snapshot" => get_market_snapshot(&repo).await,
        other => Err(anyhow::anyhow!("unknown tool: {other}")),
    };

    match result {
        Ok(value) => value.to_string(),
        Err(e) => {
            tracing::warn!("tool {name} failed: {e:#}");
            json!({ "error": format!("{e:#}") }).to_string()
        }
    }
}

/// Parse the model-supplied argument string, tolerating the empty string that
/// some models send for tools with no required arguments.
fn parse_args(arguments: &str) -> Result<Value> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(trimmed).context("tool arguments were not valid JSON")
}

/// Clamp a caller-provided limit into `1..=MAX_LIMIT`, falling back to the
/// default when absent.
fn resolve_limit(args: &Value) -> i64 {
    args.get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT)
}

async fn semantic_search(repo: &Repository, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let query = require_query(&args)?;
    let limit = resolve_limit(&args);

    let articles = repo.search_semantic(query, limit).await?;
    Ok(json!({ "count": articles.len(), "articles": articles }))
}

async fn search_articles(repo: &Repository, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let query = require_query(&args)?;
    let limit = resolve_limit(&args);

    let articles = repo.search_keyword(query, limit).await?;
    Ok(json!({ "count": articles.len(), "articles": articles }))
}

async fn list_recent_articles(repo: &Repository, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let limit = resolve_limit(&args);
    let category = args.get("category").and_then(Value::as_str);

    let articles = repo.list_recent(category, limit).await?;
    Ok(json!({ "count": articles.len(), "articles": articles }))
}

async fn get_article(repo: &Repository, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let id = args
        .get("id")
        .and_then(Value::as_i64)
        .context("missing required integer `id` argument")?;

    match repo.get_article(id).await? {
        Some(article) => Ok(serde_json::to_value(article)?),
        None => Ok(json!({ "error": format!("no article with id {id}") })),
    }
}

async fn get_market_snapshot(repo: &Repository) -> Result<Value> {
    let snapshots = repo.market_snapshot().await?;
    Ok(json!({ "count": snapshots.len(), "snapshots": snapshots }))
}

/// Extract the required, non-empty `query` string argument.
fn require_query(args: &Value) -> Result<&str> {
    args.get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
        .context("missing required `query` argument")
}
