use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Crypto,
    Ai,
    Security,
    Market,
}

/// What a fetcher returns: minimal, source-shaped, not yet scored or stored.
#[derive(Debug, Clone)]
pub struct RawItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub category: Category,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub published_at: DateTime<Utc>,
}
