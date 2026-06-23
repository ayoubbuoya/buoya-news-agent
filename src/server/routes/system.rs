//! System routes: liveness. The MCP endpoint is mounted separately by
//! [`crate::server::mcp`], not here.

use actix_web::{HttpResponse, web};
use serde::Serialize;
use utoipa::ToSchema;

/// Liveness response for [`health`].
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// Always `"ok"` when the server is reachable.
    #[schema(example = "ok")]
    pub status: String,
}

/// Liveness probe.
#[utoipa::path(
    get,
    path = "/health",
    tag = "system",
    responses(
        (status = 200, description = "Server is up", body = HealthResponse),
    )
)]
pub async fn health() -> HttpResponse {
    HttpResponse::Ok().json(HealthResponse {
        status: "ok".to_string(),
    })
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(health));
}
