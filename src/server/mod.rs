//! HTTP backend adapter: an actix-web daemon that exposes the core to a frontend.
//!
//! Two kinds of routes: plain JSON data routes over [`Core::repository`] (what a
//! UI needs without involving the LLM), and a streaming `POST /chat` that drives
//! the agent via [`Core::chat_stream`] and relays [`StreamEvent`]s as Server-Sent
//! Events. The shared [`Core`] lives in `web::Data` so every worker shares one
//! pool, embedder, and client.
//!
//! `GET /mcp` is a placeholder until the MCP adapter is mounted here (step 5).

use std::time::Duration;

use actix_web::error::ErrorInternalServerError;
use actix_web::{App, HttpResponse, HttpServer, Result as ActixResult, web};
use actix_web_lab::sse::{self, Sse};
use serde::Deserialize;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::core::Core;
use crate::core::llm::StreamEvent;
use crate::core::types::{ChatMessage, Role};

/// Default number of articles a list/search route returns when unspecified.
const DEFAULT_LIMIT: i64 = 20;
/// Hard cap so one request cannot ask for an unbounded result set.
const MAX_LIMIT: i64 = 50;

/// `serve` subcommand options.
#[derive(Debug, Clone, clap::Args)]
pub struct ServeArgs {
    /// Address to bind the HTTP server to.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Port to listen on.
    #[arg(long, default_value_t = 8080)]
    pub port: u16,
}

/// Run the HTTP backend until shut down.
pub async fn run(core: Core, args: ServeArgs) -> anyhow::Result<()> {
    let data = web::Data::new(core);
    let (host, port) = (args.host.clone(), args.port);
    tracing::info!("starting HTTP server on http://{host}:{port}");

    HttpServer::new(move || {
        App::new()
            .app_data(data.clone())
            .route("/health", web::get().to(health))
            .route("/articles", web::get().to(list_articles))
            .route("/articles/search", web::get().to(search_articles))
            .route("/articles/{id}", web::get().to(get_article))
            .route("/market/snapshot", web::get().to(market_snapshot))
            .route("/chat", web::post().to(chat))
            .route("/mcp", web::to(mcp_stub))
    })
    .bind((host.as_str(), port))?
    .run()
    .await?;

    Ok(())
}

async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))
}

/// Clamp a caller-provided limit into `1..=MAX_LIMIT`, defaulting when absent.
fn resolve_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    category: Option<String>,
    limit: Option<i64>,
}

/// `GET /articles?category=&limit=` — the most recent articles.
async fn list_articles(
    core: web::Data<Core>,
    query: web::Query<ListQuery>,
) -> ActixResult<HttpResponse> {
    let limit = resolve_limit(query.limit);
    let articles = core
        .repository()
        .list_recent(query.category.as_deref(), limit)
        .await
        .map_err(|e| ErrorInternalServerError(format!("{e:#}")))?;
    Ok(HttpResponse::Ok().json(articles))
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    /// When true, run meaning-based vector search; otherwise exact keyword search.
    #[serde(default)]
    semantic: bool,
    limit: Option<i64>,
}

/// `GET /articles/search?q=&semantic=&limit=` — keyword or semantic search.
async fn search_articles(
    core: web::Data<Core>,
    query: web::Query<SearchQuery>,
) -> ActixResult<HttpResponse> {
    let limit = resolve_limit(query.limit);
    let repo = core.repository();
    let articles = if query.semantic {
        repo.search_semantic(&query.q, limit).await
    } else {
        repo.search_keyword(&query.q, limit).await
    }
    .map_err(|e| ErrorInternalServerError(format!("{e:#}")))?;
    Ok(HttpResponse::Ok().json(articles))
}

/// `GET /articles/{id}` — the full record, or 404 if absent.
async fn get_article(core: web::Data<Core>, path: web::Path<i64>) -> ActixResult<HttpResponse> {
    let id = path.into_inner();
    match core
        .repository()
        .get_article(id)
        .await
        .map_err(|e| ErrorInternalServerError(format!("{e:#}")))?
    {
        Some(article) => Ok(HttpResponse::Ok().json(article)),
        None => Ok(HttpResponse::NotFound()
            .json(serde_json::json!({ "error": format!("no article with id {id}") }))),
    }
}

/// `GET /market/snapshot` — the latest daily snapshot per market source.
async fn market_snapshot(core: web::Data<Core>) -> ActixResult<HttpResponse> {
    let snapshots = core
        .repository()
        .market_snapshot()
        .await
        .map_err(|e| ErrorInternalServerError(format!("{e:#}")))?;
    Ok(HttpResponse::Ok().json(snapshots))
}

/// One message a client supplies in a chat request. Only role + content matter to
/// the agent; the rest of [`ChatMessage`] is DB bookkeeping the server fills in.
#[derive(Debug, Deserialize)]
struct ChatTurn {
    role: Role,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatRequest {
    messages: Vec<ChatTurn>,
}

/// `POST /chat` — drive one agent turn and stream the reply as SSE.
///
/// Event names the client can switch on: `token` (a text chunk to append),
/// `tool` (a human-readable tool-call label), `done` (turn finished), `error`
/// (failure message). The stream closes after `done` or `error`.
async fn chat(core: web::Data<Core>, body: web::Json<ChatRequest>) -> impl actix_web::Responder {
    let history: Vec<ChatMessage> = body
        .into_inner()
        .messages
        .into_iter()
        .map(|turn| ChatMessage {
            id: 0,
            session_id: String::new(),
            role: turn.role,
            content: turn.content,
            created_at: String::new(),
            tools_used: Vec::new(),
        })
        .collect();

    let stream = UnboundedReceiverStream::new(core.chat_stream(history)).map(|event| {
        let data = match event {
            StreamEvent::Token(text) => sse::Data::new(text).event("token"),
            StreamEvent::ToolCall(label) => sse::Data::new(label).event("tool"),
            StreamEvent::Done => sse::Data::new("").event("done"),
            StreamEvent::Error(message) => sse::Data::new(message).event("error"),
        };
        sse::Event::Data(data)
    });

    Sse::from_infallible_stream(stream).with_keep_alive(Duration::from_secs(15))
}

/// Placeholder for the MCP-over-HTTP endpoint mounted here in step 5.
async fn mcp_stub() -> HttpResponse {
    HttpResponse::NotImplemented()
        .json(serde_json::json!({ "error": "MCP endpoint not yet implemented" }))
}
