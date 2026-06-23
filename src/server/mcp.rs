//! The MCP server: exposes the agent's tools over the **Streamable HTTP**
//! transport so any MCP client can drive them.
//!
//! One server, many clients. A single Streamable HTTP endpoint is what every
//! target consumes — Claude Code, Kilo Code, Cursor and other local dev tools
//! connect to it directly; Claude.ai and ChatGPT reach it through a public tunnel
//! (Cloudflare/ngrok) that forwards the bearer token. The tools themselves are the
//! same registry the chat loop uses ([`crate::core::llm::tools`]): the model-facing
//! OpenAI adapter and this MCP surface are two views of one source of truth.
//!
//! ## Why a bridge
//!
//! rmcp's [`StreamableHttpService`] is built on the `http` 1.x ecosystem, while
//! actix-web is still on `http` 0.2 — their `Method`/`Uri`/`HeaderName` types are
//! distinct. Rather than stand up a second HTTP server on its own port, we mount
//! the rmcp service behind one actix handler ([`handle`]) and translate at the
//! boundary. The translation goes through `&str`/`&[u8]`, which both `http`
//! versions agree on, and the body (`bytes::Bytes`) is unified, so it streams
//! through without copying. This keeps the whole backend on a single origin and
//! the `/mcp` path the rest of the app already documents.
//!
//! ## Auth
//!
//! When `MCP_AUTH_TOKEN` is set, every request must carry
//! `Authorization: Bearer <token>`; this is the gate a tunnel forwards. When it's
//! unset the endpoint is open — fine for a loopback-only `serve`, but set the
//! token before exposing it.

use std::convert::Infallible;
use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, web};
use bytes::Bytes;
use futures::StreamExt;
use http_body_util::{BodyStream, Full};
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::LocalSessionManager,
};
use serde_json::Value;

use crate::core::Core;
use crate::core::llm::tools;

/// The concrete rmcp service type we mount: our handler over an in-process,
/// single-node session manager.
type Service = StreamableHttpService<BuoyaMcp, LocalSessionManager>;

/// MCP request handler: advertises and runs the agent's tools against the core.
///
/// Cloned per session by the service factory; clones share the core's pool,
/// embedder, and clients, so a handler is cheap to make.
#[derive(Clone)]
struct BuoyaMcp {
    core: Core,
}

impl ServerHandler for BuoyaMcp {
    fn get_info(&self) -> InitializeResult {
        let mut server_info = Implementation::from_build_env();
        server_info.name = "buoya-news-agent".to_string();
        server_info.title = Some("Buoya News Agent".to_string());
        server_info.version = env!("CARGO_PKG_VERSION").to_string();
        server_info.description =
            Some("News agent over a local crypto/AI/security article store with market \
                  snapshots. Tools: semantic and keyword article search, recent-article \
                  listing, full-article fetch, and a structured market snapshot."
                .to_string());

        let mut info = InitializeResult::new(ServerCapabilities::builder().enable_tools().build());
        info.protocol_version = ProtocolVersion::LATEST;
        info.server_info = server_info;
        info.instructions = Some(
            "Use semantic_search for conceptual/topical questions and search_articles for \
             exact tickers or names. list_recent_articles answers \"what's new\"; get_article \
             reads one in full by id; get_market_snapshot returns sentiment, top movers, and \
             DeFi TVL."
                .to_string(),
        );
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        // Derived from the same registry the chat loop advertises, so the two
        // surfaces never drift.
        let tools = tools::tool_infos()
            .into_iter()
            .map(|info| {
                let schema = info
                    .parameters
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                Tool::new(info.name, info.description, schema)
            })
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // `execute` is total: on a bad call it returns a JSON `{ "error": ... }`
        // string rather than failing, mirroring the chat loop's behaviour. We pass
        // that straight back as tool content so the client/model can react to it.
        let arguments = request
            .arguments
            .map(Value::Object)
            .unwrap_or_else(|| Value::Object(Default::default()))
            .to_string();
        let repository = self.core.repository();
        let result = tools::execute(&repository, &request.name, &arguments).await;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
}

/// Shared `/mcp` state: the mounted rmcp service plus the optional bearer token
/// every request must match when set.
pub struct McpState {
    service: Service,
    auth_token: Option<String>,
}

/// Build the `/mcp` state from the core. `auth_token` comes from `MCP_AUTH_TOKEN`;
/// when `None`, the endpoint is unauthenticated.
pub fn build_state(core: Core, auth_token: Option<String>) -> McpState {
    // Host validation is rmcp's anti-DNS-rebinding guard; it only allows loopback
    // hosts by default, which would 403 every request arriving through a tunnel.
    // We disable it because the bearer token (and the tunnel) are the real gate.
    let config = StreamableHttpServerConfig::default().disable_allowed_hosts();
    let service = StreamableHttpService::new(
        move || Ok(BuoyaMcp { core: core.clone() }),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    McpState {
        service,
        auth_token,
    }
}

/// Single actix handler for every `/mcp` method (POST/GET/DELETE). Checks the
/// bearer token, then translates the request into rmcp's `http` 1.x world, runs
/// it, and streams the response (JSON or SSE) back through actix.
async fn handle(
    req: HttpRequest,
    body: web::Bytes,
    state: web::Data<McpState>,
) -> Result<HttpResponse, actix_web::Error> {
    if let Some(expected) = &state.auth_token {
        let presented = req
            .headers()
            .get(actix_web::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if presented != Some(expected.as_str()) {
            return Ok(HttpResponse::Unauthorized()
                .insert_header(("WWW-Authenticate", "Bearer"))
                .json(serde_json::json!({ "error": "missing or invalid bearer token" })));
        }
    }

    // actix (http 0.2) -> rmcp (http 1.x). Method/URI/headers convert through
    // their string/byte forms, which are version-agnostic; the body is `bytes::Bytes`,
    // already shared between both stacks.
    let mut builder = http::Request::builder()
        .method(req.method().as_str())
        .uri(req.uri().to_string());
    for (name, value) in req.headers() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let http_request = builder
        .body(Full::new(body))
        .map_err(actix_web::error::ErrorInternalServerError)?;

    let http_response = state.service.handle(http_request).await;

    // rmcp (http 1.x) -> actix (http 0.2), same string/byte bridge in reverse.
    let (parts, response_body) = http_response.into_parts();
    let status = actix_web::http::StatusCode::from_u16(parts.status.as_u16())
        .map_err(actix_web::error::ErrorInternalServerError)?;
    let mut response = HttpResponse::build(status);
    for (name, value) in parts.headers.iter() {
        // The body is streamed, so let actix own framing; copying these would clash
        // with the chunked transfer it sets up.
        if name == http::header::CONTENT_LENGTH || name == http::header::TRANSFER_ENCODING {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            actix_web::http::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            actix_web::http::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            response.insert_header((name, value));
        }
    }

    // Stream the rmcp body (single JSON frame, or a live SSE stream in stateful
    // mode) out through actix. Non-data frames (trailers) are dropped.
    let stream = BodyStream::new(response_body).filter_map(|frame| async move {
        match frame {
            Ok(frame) => frame.into_data().ok().map(Ok::<Bytes, Infallible>),
            Err(err) => Some(Err(err)),
        }
    });
    Ok(response.streaming(stream))
}

/// Register `/mcp` for every method on the app's service config.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/mcp", web::to(handle));
}
