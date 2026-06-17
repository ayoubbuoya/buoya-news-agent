//! Core domain types shared across fetchers, pipeline, and tools.
//!
//! This is the minimal-slice subset of §5 of the spec: enough to fetch RSS,
//! store items, score them, and answer `get_briefing` / `search_news`. Richer
//! signals (HN points, loss amounts, dedup groups) are added by later tasks.

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

impl Category {
    /// Stable lowercase token used in the DB and config.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Crypto => "crypto",
            Category::Ai => "ai",
            Category::Security => "security",
            Category::Market => "market",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "crypto" => Some(Category::Crypto),
            "ai" => Some(Category::Ai),
            "security" => Some(Category::Security),
            "market" => Some(Category::Market),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Notable,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Notable => "notable",
            Severity::Critical => "critical",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "info" => Some(Severity::Info),
            "notable" => Some(Severity::Notable),
            "critical" => Some(Severity::Critical),
            _ => None,
        }
    }
}

/// What a fetcher returns: minimal, source-shaped, not yet scored or stored.
#[derive(Debug, Clone)]
pub struct RawItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub category: Category,
    pub published_at: DateTime<Utc>,
}

/// A scored item ready to persist or return to the client.
#[derive(Debug, Clone, Serialize)]
pub struct NewsItem {
    pub id: String,
    pub title: String,
    pub url: String,
    pub source: String,
    pub category: Category,
    pub published_at: DateTime<Utc>,
    pub fetched_at: DateTime<Utc>,
    pub severity: Severity,
    pub score: f64,
}
