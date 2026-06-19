//! Post-ingest editorial agent: from a batch of newly-ingested articles, pick the
//! few genuinely worth a reader's attention.
//!
//! A plain LLM call — no tools, no streaming. Given each candidate's metadata, the
//! model returns the ids it judges significant plus a one-line reason for each.
//! Push connectors (e.g. Telegram) use it to turn a raw ingest batch into a short,
//! high-signal digest instead of alerting on everything.

use std::collections::HashSet;

use anyhow::{Context, Result};
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionRequestSystemMessage, ChatCompletionRequestUserMessage,
    CreateChatCompletionRequestArgs,
};
use serde::{Deserialize, Serialize};

use crate::core::repository::ArticleSummary;

/// One article the editor judged worth reading, paired with its rationale.
#[derive(Debug, Clone)]
pub struct Pick {
    pub article: ArticleSummary,
    /// One-line reason the editor flagged this article.
    pub reason: String,
}

/// A candidate as presented to the model: just the fields it needs to judge
/// significance, keyed by `id` so a pick maps back to the full article.
#[derive(Serialize)]
struct Candidate<'a> {
    id: i64,
    title: &'a str,
    source: &'a str,
    category: &'a str,
    summary: &'a str,
}

/// One entry of the model's reply: a chosen id and why. Extra fields are ignored.
#[derive(Deserialize)]
struct Selection {
    id: i64,
    #[serde(default)]
    reason: String,
}

/// Build the editor's instructions, naming the reader's watchlist and the cap.
fn system_prompt(watchlist: &[String], max: usize) -> String {
    let watch = if watchlist.is_empty() {
        "none specified".to_string()
    } else {
        watchlist.join(", ")
    };
    format!(
        "You are the editor of a personal crypto, DeFi, AI, and security news feed. \
         From a list of newly-ingested articles, select ONLY the ones genuinely \
         worth a busy reader's time: major market-moving events, significant \
         protocol or model launches, security incidents and exploits, regulatory \
         actions, and anything notable about the reader's watchlist ({watch}). \
         Skip routine price commentary, low-substance posts, opinion fluff, and \
         near-duplicates. Be strict — selecting nothing is a valid answer. Choose \
         at most {max} articles. \
         Respond with ONLY a JSON array of objects, each \
         {{\"id\": <number>, \"reason\": \"<short phrase, ~50 words max, why it \
         matters>\"}}, ordered most to least important. No prose, no code fences."
    )
}

/// Ask the model which of `articles` are worth reading. Returns the picks in the
/// model's importance order, mapped back to full articles, de-duplicated and capped
/// at `max`. An empty input (or `max == 0`) short-circuits without an LLM call.
pub async fn select_worthwhile(
    client: &Client<OpenAIConfig>,
    model: &str,
    watchlist: &[String],
    articles: &[ArticleSummary],
    max: usize,
) -> Result<Vec<Pick>> {
    if articles.is_empty() || max == 0 {
        return Ok(Vec::new());
    }

    let candidates: Vec<Candidate> = articles
        .iter()
        .map(|a| Candidate {
            id: a.id,
            title: &a.title,
            source: &a.source,
            category: &a.category,
            summary: a.summary.as_deref().unwrap_or(""),
        })
        .collect();

    let candidates_json =
        serde_json::to_string(&candidates).context("failed to serialize curation candidates")?;

    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages([
            ChatCompletionRequestSystemMessage::from(system_prompt(watchlist, max)).into(),
            ChatCompletionRequestUserMessage::from(format!(
                "Newly ingested articles:\n{candidates_json}"
            ))
            .into(),
        ])
        .build()
        .context("failed to build curation request")?;

    let response = client
        .chat()
        .create(request)
        .await
        .context("curation request failed")?;

    let content = response
        .choices
        .first()
        .and_then(|choice| choice.message.content.clone())
        .unwrap_or_default();

    let selections = parse_selections(&content)?;
    Ok(resolve_picks(selections, articles, max))
}

/// Map model selections back to full articles, preserving the model's order,
/// dropping unknown/duplicate ids, and capping at `max`.
fn resolve_picks(selections: Vec<Selection>, articles: &[ArticleSummary], max: usize) -> Vec<Pick> {
    let mut picks = Vec::new();
    let mut seen = HashSet::new();
    for sel in selections {
        if picks.len() >= max {
            break;
        }
        if !seen.insert(sel.id) {
            continue;
        }
        if let Some(article) = articles.iter().find(|a| a.id == sel.id) {
            picks.push(Pick {
                article: article.clone(),
                reason: sel.reason,
            });
        }
    }
    picks
}

/// Parse the model's reply into selections, tolerating code fences or stray prose
/// around the JSON array by slicing from the first `[` to the last `]`.
fn parse_selections(content: &str) -> Result<Vec<Selection>> {
    let json = extract_json_array(content).context("model reply contained no JSON array")?;
    serde_json::from_str(json).context("failed to parse curation reply as JSON")
}

/// The substring from the first `[` to the last `]`, or `None` if absent.
fn extract_json_array(s: &str) -> Option<&str> {
    let start = s.find('[')?;
    let end = s.rfind(']')?;
    (end > start).then(|| &s[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn article(id: i64) -> ArticleSummary {
        ArticleSummary {
            id,
            title: format!("Article {id}"),
            url: format!("https://example.com/{id}"),
            source: "src".to_string(),
            category: "crypto".to_string(),
            summary: None,
            published_at: "2026-06-19T00:00:00Z".to_string(),
            distance: None,
        }
    }

    #[test]
    fn extracts_array_from_fenced_reply() {
        let reply = "```json\n[{\"id\": 1, \"reason\": \"big\"}]\n```";
        assert_eq!(
            extract_json_array(reply),
            Some("[{\"id\": 1, \"reason\": \"big\"}]")
        );
    }

    #[test]
    fn no_array_returns_none() {
        assert_eq!(extract_json_array("sorry, nothing notable"), None);
    }

    #[test]
    fn resolve_preserves_order_drops_unknown_and_dedupes() {
        let articles = vec![article(1), article(2), article(3)];
        let selections = vec![
            Selection {
                id: 3,
                reason: "third".into(),
            },
            Selection {
                id: 99,
                reason: "missing".into(),
            },
            Selection {
                id: 1,
                reason: "first".into(),
            },
            Selection {
                id: 1,
                reason: "dup".into(),
            },
        ];
        let picks = resolve_picks(selections, &articles, 5);
        let ids: Vec<i64> = picks.iter().map(|p| p.article.id).collect();
        assert_eq!(ids, vec![3, 1]);
        assert_eq!(picks[0].reason, "third");
    }

    #[test]
    fn resolve_caps_at_max() {
        let articles = vec![article(1), article(2), article(3)];
        let selections = vec![
            Selection {
                id: 1,
                reason: String::new(),
            },
            Selection {
                id: 2,
                reason: String::new(),
            },
            Selection {
                id: 3,
                reason: String::new(),
            },
        ];
        assert_eq!(resolve_picks(selections, &articles, 2).len(), 2);
    }
}
