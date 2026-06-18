//! Tool definitions the LLM can call to read stored news articles, plus the
//! dispatch logic that runs a requested tool against the database.
//!
//! Tools follow the OpenAI function-calling shape: each has a JSON-Schema
//! parameter spec the model fills in, and [`execute`] runs the matching query
//! and returns a JSON string the model reads back as the tool result.

use anyhow::{Context, Result};
use async_openai::types::chat::{ChatCompletionTool, ChatCompletionTools, FunctionObject};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};

/// Default number of articles returned by list/search tools when the model does
/// not specify a limit.
const DEFAULT_LIMIT: i64 = 20;
/// Hard cap on rows returned so a single tool call cannot flood the context.
const MAX_LIMIT: i64 = 50;

/// The set of tools advertised to the model on every request.
pub fn tool_definitions() -> Vec<ChatCompletionTools> {
    vec![
        function_tool(
            "search_articles",
            "Full-text search over stored news articles. Matches the query \
             against article titles, summaries, and body content. Use this to \
             answer questions about a topic, company, or token mentioned in the \
             news.",
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
pub async fn execute(pool: &SqlitePool, name: &str, arguments: &str) -> String {
    let result = match name {
        "search_articles" => search_articles(pool, arguments).await,
        "list_recent_articles" => list_recent_articles(pool, arguments).await,
        "get_article" => get_article(pool, arguments).await,
        "get_market_snapshot" => get_market_snapshot(pool).await,
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

async fn search_articles(pool: &SqlitePool, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
        .context("missing required `query` argument")?;
    let limit = resolve_limit(&args);

    let pattern = format!("%{}%", query);
    let rows = sqlx::query(
        "SELECT id, title, url, source, category, summary, published_at
         FROM articles
         WHERE title LIKE ? OR summary LIKE ? OR content LIKE ?
         ORDER BY published_at DESC
         LIMIT ?",
    )
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("failed to search articles")?;

    let articles: Vec<Value> = rows.iter().map(article_summary).collect();
    Ok(json!({ "count": articles.len(), "articles": articles }))
}

async fn list_recent_articles(pool: &SqlitePool, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let limit = resolve_limit(&args);
    let category = args.get("category").and_then(Value::as_str);

    let rows = match category {
        Some(category) => {
            sqlx::query(
                "SELECT id, title, url, source, category, summary, published_at
                 FROM articles
                 WHERE category = ?
                 ORDER BY published_at DESC
                 LIMIT ?",
            )
            .bind(category)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query(
                "SELECT id, title, url, source, category, summary, published_at
                 FROM articles
                 ORDER BY published_at DESC
                 LIMIT ?",
            )
            .bind(limit)
            .fetch_all(pool)
            .await
        }
    }
    .context("failed to list recent articles")?;

    let articles: Vec<Value> = rows.iter().map(article_summary).collect();
    Ok(json!({ "count": articles.len(), "articles": articles }))
}

async fn get_article(pool: &SqlitePool, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let id = args
        .get("id")
        .and_then(Value::as_i64)
        .context("missing required integer `id` argument")?;

    let row = sqlx::query(
        "SELECT id, title, url, source, category, summary, content, published_at
         FROM articles
         WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("failed to fetch article")?;

    match row {
        Some(row) => Ok(json!({
            "id": row.get::<i64, _>("id"),
            "title": row.get::<String, _>("title"),
            "url": row.get::<String, _>("url"),
            "source": row.get::<String, _>("source"),
            "category": row.get::<String, _>("category"),
            "summary": row.get::<Option<String>, _>("summary"),
            "content": row.get::<Option<String>, _>("content"),
            "published_at": row.get::<String, _>("published_at"),
        })),
        None => Ok(json!({ "error": format!("no article with id {id}") })),
    }
}

/// The synthesized snapshot sources, fetched by [`get_market_snapshot`]. Each is
/// written once per day by its fetcher, so the newest row is today's reading.
const SNAPSHOT_SOURCES: [&str; 3] = ["fear-greed", "coingecko", "defillama"];

async fn get_market_snapshot(pool: &SqlitePool) -> Result<Value> {
    let mut snapshots: Vec<Value> = Vec::with_capacity(SNAPSHOT_SOURCES.len());

    for source in SNAPSHOT_SOURCES {
        let row = sqlx::query(
            "SELECT id, title, url, source, category, summary, content, published_at
             FROM articles
             WHERE source = ?
             ORDER BY published_at DESC
             LIMIT 1",
        )
        .bind(source)
        .fetch_optional(pool)
        .await
        .with_context(|| format!("failed to fetch latest {source} snapshot"))?;

        if let Some(row) = row {
            snapshots.push(json!({
                "id": row.get::<i64, _>("id"),
                "title": row.get::<String, _>("title"),
                "source": row.get::<String, _>("source"),
                "category": row.get::<String, _>("category"),
                "summary": row.get::<Option<String>, _>("summary"),
                "content": row.get::<Option<String>, _>("content"),
                "published_at": row.get::<String, _>("published_at"),
            }));
        }
    }

    Ok(json!({ "count": snapshots.len(), "snapshots": snapshots }))
}

/// Project a row into the compact shape used by list/search results: enough for
/// the model to cite or decide whether to fetch the full article, without the
/// heavy `content` column.
fn article_summary(row: &sqlx::sqlite::SqliteRow) -> Value {
    json!({
        "id": row.get::<i64, _>("id"),
        "title": row.get::<String, _>("title"),
        "url": row.get::<String, _>("url"),
        "source": row.get::<String, _>("source"),
        "category": row.get::<String, _>("category"),
        "summary": row.get::<Option<String>, _>("summary"),
        "published_at": row.get::<String, _>("published_at"),
    })
}
