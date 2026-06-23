//! HTTP backend adapter: an actix-web daemon that exposes the core to a frontend.
//!
//! This module is wiring only — `ServeArgs`, [`run`], and the app factory. The
//! actual endpoints live under [`routes`] (one module per resource), the OpenAPI
//! document in [`openapi`], and shared error handling in [`error`].
//!
//! Routes fall into three groups: persisted **chat sessions** that mirror the TUI
//! (`/sessions`, `/sessions/{id}/chat`), read-only **data** routes over
//! [`Core::repository`] (`/articles`, `/market/snapshot`), and **system** routes
//! (`/health`). The agent's tools are also exposed over MCP at `/mcp` (see
//! [`mcp`]). The shared [`Core`] lives in `web::Data` so every worker shares one
//! pool, embedder, and client.
//!
//! Interactive API docs are served by Swagger UI at `/swagger-ui/`, backed by the
//! generated OpenAPI document at `/api-docs/openapi.json`.

mod error;
mod mcp;
mod openapi;
mod routes;

use actix_cors::Cors;
use actix_web::{App, HttpServer, web};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::connectors::telegram;
use crate::core::Core;
use openapi::ApiDoc;

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
    // Start outbound connectors before serving HTTP. They're pure background
    // consumers of the core (they subscribe to the ingest broadcast), so they run as
    // detached tasks alongside the web server. `resolve` returns `Some` only when the
    // connector is enabled and fully configured; otherwise we don't spawn anything.
    if let Some(tg) = telegram::TelegramConfig::resolve(&core.config) {
        tokio::spawn(telegram::run(core.clone(), tg));
    }

    // The MCP server reuses the agent's tools over Streamable HTTP at `/mcp`. Its
    // bearer token is a secret, so it comes from the environment (like the Telegram
    // token), not `ServeArgs`. With no token the endpoint is open — fine for a
    // loopback `serve`, but set `MCP_AUTH_TOKEN` before exposing it via a tunnel.
    let mcp_token = std::env::var("MCP_AUTH_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty());
    let mcp_data = web::Data::new(mcp::build_state(core.clone(), mcp_token.clone()));

    let data = web::Data::new(core);
    let (host, port) = (args.host.clone(), args.port);
    tracing::info!("starting HTTP server on http://{host}:{port}");
    tracing::info!("API docs available at http://{host}:{port}/swagger-ui/");
    if mcp_token.is_some() {
        tracing::info!("MCP (Streamable HTTP) at http://{host}:{port}/mcp — bearer auth enabled");
    } else {
        tracing::warn!(
            "MCP (Streamable HTTP) at http://{host}:{port}/mcp — no auth; set MCP_AUTH_TOKEN \
             before exposing it"
        );
    }

    HttpServer::new(move || {
        App::new()
            .app_data(data.clone())
            .app_data(mcp_data.clone())
            // Permissive CORS: this is a local, single-user backend, and a browser
            // frontend (dev server or separate origin) must be able to call it. If
            // this is ever exposed to untrusted networks, tighten to known origins.
            .wrap(Cors::permissive())
            .configure(routes::configure)
            .configure(mcp::configure)
            .service(
                SwaggerUi::new("/swagger-ui/{_:.*}")
                    .url("/api-docs/openapi.json", ApiDoc::openapi()),
            )
    })
    .bind((host.as_str(), port))?
    .run()
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated OpenAPI document covers every documented route and resolves
    /// the schemas they reference, so Swagger UI renders without dangling `$ref`s.
    #[test]
    fn openapi_document_lists_all_routes() {
        let doc = ApiDoc::openapi();
        let paths = &doc.paths.paths;

        for expected in [
            "/health",
            "/articles",
            "/articles/search",
            "/articles/{id}",
            "/market/snapshot",
            "/sessions",
            "/sessions/{id}",
            "/sessions/{id}/messages",
            "/sessions/{id}/chat",
            "/chat",
        ] {
            assert!(paths.contains_key(expected), "missing path: {expected}");
        }

        let components = doc.components.as_ref();
        assert!(components.is_some(), "components block should be present");
        if let Some(components) = components {
            for schema in [
                "ChatSession",
                "ChatMessage",
                "ArticleSummary",
                "Article",
                "SnapshotItem",
                "SessionChatRequest",
                "CreateSessionRequest",
            ] {
                assert!(
                    components.schemas.contains_key(schema),
                    "missing schema: {schema}"
                );
            }
        }
    }
}
