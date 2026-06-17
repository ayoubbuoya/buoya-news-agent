//! MCP server exposing the minimal slice's two tools over stdio:
//! `get_briefing` and `search_news`. Both read from the DB; `get_briefing`
//! triggers a lazy ingest first if the store looks stale.

use chrono::{Duration, Utc};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use crate::domain::{Category, NewsItem};
use crate::ingest::{self, AppState};

#[derive(Clone)]
pub struct NewsServer {
    state: AppState,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BriefingArgs {
    /// Lookback window: "24h" or "7d". Defaults to "24h".
    #[serde(default)]
    pub period: Option<String>,
    /// Optional topic filter: "crypto", "ai", "security", or "market".
    #[serde(default)]
    pub topic: Option<String>,
    /// Max items to return (default 15).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// Full-text query over item titles.
    pub query: String,
    /// Max items to return (default 20).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Serialize)]
struct Envelope {
    count: usize,
    items: Vec<NewsItem>,
}

#[tool_router]
impl NewsServer {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "get_briefing",
        description = "Ranked digest of recent crypto/AI/security news. Refreshes from sources if data is stale."
    )]
    async fn get_briefing(
        &self,
        Parameters(args): Parameters<BriefingArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Lazy refresh: fetch if the store is empty or older than 6 hours.
        let stale = match self.state.db.last_fetched_at() {
            Ok(Some(ts)) => Utc::now() - ts > Duration::hours(6),
            Ok(None) => true,
            Err(_) => false,
        };
        if stale {
            ingest::run(&self.state).await;
        }

        let period = args.period.as_deref().unwrap_or("24h");
        let since = Utc::now()
            - match period {
                "7d" => Duration::days(7),
                _ => Duration::hours(24),
            };
        let category = args.topic.as_deref().and_then(Category::parse);
        let limit = args.limit.unwrap_or(15).min(50);

        let items = self
            .state
            .db
            .recent(since, category, limit)
            .map_err(internal)?;
        envelope(items)
    }

    #[tool(
        name = "search_news",
        description = "Full-text search over stored news item titles."
    )]
    async fn search_news(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = args.limit.unwrap_or(20).min(50);
        let items = self
            .state
            .db
            .search(&args.query, limit)
            .map_err(internal)?;
        envelope(items)
    }
}

/// Serialize items as JSON text content for the MCP client.
fn envelope(items: Vec<NewsItem>) -> Result<CallToolResult, ErrorData> {
    let payload = Envelope {
        count: items.len(),
        items,
    };
    let json = serde_json::to_string(&payload)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

#[tool_handler]
impl ServerHandler for NewsServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Aggregated crypto and AI news. Use get_briefing for a digest of recent items, \
             or search_news to look up a topic."
                .into(),
        );
        info
    }
}

fn internal(e: crate::error::DbError) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}
