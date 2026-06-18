//! The agent's tools, plus the registry that binds each tool's advertised
//! metadata to the handler that runs it.
//!
//! A single [`registry`] is the source of truth. The OpenAI function-calling
//! adapter ([`tool_definitions`]) and the neutral [`tool_infos`] view — for other
//! surfaces such as an MCP server — are both derived from it, and [`execute`]
//! dispatches by looking a tool up in the same list. Adding a tool is one registry
//! entry plus its handler; there is no second place to keep in sync.
//!
//! Each tool follows the OpenAI function-calling shape: a JSON-Schema parameter
//! spec the caller fills in, and a handler that runs the matching query and
//! returns a JSON value the model reads back as the tool result.

use std::future::Future;
use std::pin::Pin;
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

/// A boxed, borrowing future returned by a tool handler.
type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

/// One callable tool: the metadata advertised to a model plus the handler that
/// runs it against the repository.
struct Tool {
    name: &'static str,
    description: &'static str,
    /// Builds the JSON-Schema describing the tool's parameters.
    parameters: fn() -> Value,
    /// Runs the tool with the raw JSON argument string the caller produced.
    handler: for<'a> fn(&'a Repository, &'a str) -> ToolFuture<'a>,
}

/// The single source of truth for the agent's tools. The OpenAI schema, the
/// neutral info view, and dispatch all derive from this list.
fn registry() -> Vec<Tool> {
    vec![
        Tool {
            name: "semantic_search",
            description: "Semantic (meaning-based) search over stored news articles using vector \
                 similarity. Finds relevant articles even when they don't share the \
                 exact words as the query. Prefer this for conceptual or topical \
                 questions, e.g. \"regulatory risk for stablecoins\" or \"layer-2 \
                 scaling progress\". For an exact ticker or proper name, prefer \
                 search_articles instead. Lower distance means more relevant.",
            parameters: || {
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
                })
            },
            handler: |repo, args| Box::pin(semantic_search(repo, args)),
        },
        Tool {
            name: "search_articles",
            description: "Exact keyword/substring search over stored news articles. Matches the \
                 query literally against article titles, summaries, and body content. \
                 Best for exact tickers or proper names (e.g. \"HBAR\", \"Coinbase\"). \
                 For conceptual or topical questions, prefer semantic_search.",
            parameters: || {
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
                })
            },
            handler: |repo, args| Box::pin(search_articles(repo, args)),
        },
        Tool {
            name: "list_recent_articles",
            description: "List the most recently published stored articles, optionally \
                 filtered by category. Use this when the user asks what's new or \
                 what's happening in a given area.",
            parameters: || {
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
                })
            },
            handler: |repo, args| Box::pin(list_recent_articles(repo, args)),
        },
        Tool {
            name: "get_article",
            description: "Fetch the full stored record for a single article by its numeric \
                 id, including the body content. Use this after search or list to \
                 read an article in depth.",
            parameters: || {
                json!({
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "integer",
                            "description": "The article id, as returned by search_articles or list_recent_articles."
                        }
                    },
                    "required": ["id"]
                })
            },
            handler: |repo, args| Box::pin(get_article(repo, args)),
        },
        Tool {
            name: "get_market_snapshot",
            description: "Get the latest structured market snapshots: the crypto Fear & Greed \
                 sentiment index, a top-coins-by-market-cap overview with 24h moves, \
                 and total DeFi TVL by chain. Use this for questions about market \
                 sentiment/mood, current prices or movers, or DeFi TVL — not \
                 search_articles. Returns the most recent daily snapshot for each.",
            parameters: || {
                json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })
            },
            handler: |repo, args| Box::pin(get_market_snapshot(repo, args)),
        },
    ]
}

/// Neutral, transport-agnostic description of a tool, for advertising to any
/// surface (the OpenAI adapter, an MCP server, …). Handlers are not exposed here;
/// run a tool through [`execute`].
pub struct ToolInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// The tools' metadata, independent of any particular wire format.
pub fn tool_infos() -> Vec<ToolInfo> {
    registry()
        .into_iter()
        .map(|tool| ToolInfo {
            name: tool.name,
            description: tool.description,
            parameters: (tool.parameters)(),
        })
        .collect()
}

/// The set of tools advertised to the model, in OpenAI function-calling shape.
pub fn tool_definitions() -> Vec<ChatCompletionTools> {
    tool_infos()
        .into_iter()
        .map(|info| {
            ChatCompletionTools::Function(ChatCompletionTool {
                function: FunctionObject {
                    name: info.name.to_string(),
                    description: Some(info.description.to_string()),
                    parameters: Some(info.parameters),
                    strict: None,
                },
            })
        })
        .collect()
}

/// Run the tool named `name` with the raw JSON `arguments` string the caller
/// produced. Always returns a string: on failure it returns a JSON object with an
/// `error` field rather than propagating, so a bad tool call becomes feedback the
/// model can recover from instead of aborting the turn.
pub async fn execute(
    pool: &SqlitePool,
    embedder: &Arc<Embedder>,
    name: &str,
    arguments: &str,
) -> String {
    let repo = Repository::new(pool.clone(), embedder.clone());
    let result = match registry().into_iter().find(|tool| tool.name == name) {
        Some(tool) => (tool.handler)(&repo, arguments).await,
        None => Err(anyhow::anyhow!("unknown tool: {name}")),
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

/// Extract the required, non-empty `query` string argument.
fn require_query(args: &Value) -> Result<&str> {
    args.get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
        .context("missing required `query` argument")
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

/// Takes the standard handler arguments for registry uniformity; this tool has no
/// parameters, so `_arguments` is ignored.
async fn get_market_snapshot(repo: &Repository, _arguments: &str) -> Result<Value> {
    let snapshots = repo.market_snapshot().await?;
    Ok(json!({ "count": snapshots.len(), "snapshots": snapshots }))
}
