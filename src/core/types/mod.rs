use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Crypto,
    Ai,
    Security,
    Market,
    Defi,
}

/// Author of a chat message. Persisted as a lowercase string in `chat_messages.role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system" => Ok(Role::System),
            "user" => Ok(Role::User),
            "assistant" => Ok(Role::Assistant),
            other => Err(format!("unknown role: {other}")),
        }
    }
}

/// A saved chat conversation. One row in `chat_sessions`.
#[derive(Debug, Clone)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A single message within a session. One row in `chat_messages`.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub id: i64,
    pub session_id: String,
    pub role: Role,
    pub content: String,
    pub created_at: String,
    /// Human-readable labels for any tools the assistant invoked while producing
    /// this message. Shown in the UI for transparency and persisted to the DB
    /// (as a JSON array in `chat_messages.tool_calls`).
    pub tools_used: Vec<String>,
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
