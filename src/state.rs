use std::sync::Arc;

use async_openai::config::OpenAIConfig;
use sqlx::SqlitePool;

use crate::config::AppConfig;
use crate::embeddings::Embedder;

#[derive(Debug, Clone)]
pub struct AppState {
    pub http_client: reqwest::Client,
    pub config: Arc<AppConfig>,
    pub db_pool: SqlitePool,
    pub llm_client: async_openai::Client<OpenAIConfig>,
    pub embedder: Arc<Embedder>,
}