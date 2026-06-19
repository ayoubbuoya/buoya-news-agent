//! Push-only Telegram connector.
//!
//! Subscribes to the core's ingest broadcast (Phase 1) and, for every newly-stored
//! article that passes the category filter, sends an alert to a configured Telegram
//! chat via the Bot API. It is a pure *consumer* of [`Core`]: it never feeds data
//! back in. Inbound chat ("DM the bot, the agent replies") is deliberately out of
//! scope — when added it becomes a second task in this file (a `poll_updates` loop
//! calling `Core::chat_stream`) with no core changes.
//!
//! Wiring lives in the `serve` daemon: [`TelegramConfig::resolve`] gates whether the
//! connector starts, and `server::run` spawns [`run`] when it returns `Some`.

use anyhow::Context;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;

use crate::core::Core;
use crate::core::config::AppConfig;
use crate::core::repository::ArticleSummary;
use crate::core::types::Category;

/// Telegram Bot API host. Factored out (and overridable on [`TelegramConfig`]) so
/// tests can point the client at a local mock server instead of the real API.
const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

/// Max characters of an article summary included in an alert, so a long lead doesn't
/// produce a wall-of-text message. Trimmed on a char boundary.
const SUMMARY_MAX_CHARS: usize = 300;

/// Fully-resolved, ready-to-run connector settings: the secret token (from the
/// environment) combined with the non-secret routing/filtering (from TOML). Built
/// only when everything required is present — see [`TelegramConfig::resolve`].
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    bot_token: String,
    chat_id: String,
    /// Category allowlist; empty means "send everything".
    categories: Vec<Category>,
    /// Bot API base URL. Defaults to [`TELEGRAM_API_BASE`]; overridden in tests.
    api_base: String,
}

impl TelegramConfig {
    /// Decide whether the Telegram connector should run, and if so build its config.
    ///
    /// Returns `None` (connector does not start) when any precondition is missing:
    /// the connector is disabled, the bot token env var is unset, or no destination
    /// chat is configured. A missing token while `enabled = true` is logged, since
    /// that's likely a misconfiguration the operator wants to know about. (An empty
    /// `chat_id` with `enabled = true` is already rejected by config validation, so
    /// it can't reach here.)
    pub fn resolve(config: &AppConfig) -> Option<Self> {
        let tg = &config.toml_config.connectors.telegram;
        if !tg.enabled {
            return None;
        }
        let Some(bot_token) = config.telegram_bot_token.clone() else {
            tracing::warn!(
                "connectors.telegram is enabled but TELEGRAM_BOT_TOKEN is unset; \
                 not starting the Telegram connector"
            );
            return None;
        };
        if tg.chat_id.trim().is_empty() {
            return None;
        }
        Some(Self {
            bot_token,
            chat_id: tg.chat_id.clone(),
            categories: tg.categories.clone(),
            api_base: TELEGRAM_API_BASE.to_string(),
        })
    }

    /// Whether an article should be alerted on, given the category allowlist. An
    /// empty allowlist passes everything; otherwise the article's category (stored
    /// as a lowercase string like `"defi"`) must match one of the configured
    /// categories.
    fn passes(&self, article: &ArticleSummary) -> bool {
        self.categories.is_empty()
            || self
                .categories
                .iter()
                .any(|c| category_db_str(*c) == article.category)
    }
}

/// Run the push loop until the ingest channel closes. Spawned as a background task
/// by the `serve` daemon. Each received batch is filtered and each surviving article
/// is sent as its own Telegram message.
///
/// Failures are logged and swallowed, never propagated: one failed send (or a slow
/// network) must not kill the connector or affect ingestion.
pub async fn run(core: Core, config: TelegramConfig) {
    let mut rx = core.subscribe_ingest();
    tracing::info!(
        "telegram connector started; alerting chat {} ({})",
        config.chat_id,
        describe_filter(&config.categories),
    );

    loop {
        match rx.recv().await {
            Ok(batch) => {
                for article in batch.iter().filter(|a| config.passes(a)) {
                    let text = format_article(article);
                    // Reuse the core's shared HTTP client (already configured with a
                    // timeout and user-agent) rather than building a new one.
                    if let Err(e) = send_message(&core.http_client, &config, &text).await {
                        tracing::error!("telegram sendMessage failed for {}: {e:#}", article.url);
                    }
                }
            }
            // The connector fell behind the channel's buffer and missed `n` batches.
            // This is the deliberately-lossy contract of the ingest broadcast: log it
            // and carry on rather than treat it as fatal.
            Err(RecvError::Lagged(n)) => {
                tracing::warn!("telegram connector lagged; skipped {n} ingest batches");
            }
            // The sender (held by `Core`) was dropped — the process is shutting down.
            Err(RecvError::Closed) => {
                tracing::info!("ingest channel closed; telegram connector stopping");
                break;
            }
        }
    }
}

/// JSON body for the Bot API `sendMessage` method.
#[derive(Serialize)]
struct SendMessage<'a> {
    chat_id: &'a str,
    text: &'a str,
    /// We format with a small subset of Telegram-flavored HTML (`<b>`), so the API
    /// must parse the message as HTML.
    parse_mode: &'a str,
}

/// POST one message to the Bot API. Returns an error on transport failure or any
/// non-2xx response (with the API's error body for diagnosis).
async fn send_message(
    http: &reqwest::Client,
    config: &TelegramConfig,
    text: &str,
) -> anyhow::Result<()> {
    let url = format!("{}/bot{}/sendMessage", config.api_base, config.bot_token);
    let body = SendMessage {
        chat_id: &config.chat_id,
        text,
        parse_mode: "HTML",
    };

    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("telegram request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        // `unwrap_or_default` (not `unwrap`) keeps us within the crate's no-unwrap
        // lint: if reading the error body fails too, we just report an empty one.
        let api_body = resp.text().await.unwrap_or_default();
        anyhow::bail!("telegram API returned {status}: {api_body}");
    }
    Ok(())
}

/// Render an article as a Telegram HTML message: bold title, a `category · source`
/// line, an optional trimmed summary, and the URL on its own line (Telegram unfurls
/// it into a link preview).
fn format_article(article: &ArticleSummary) -> String {
    let mut msg = format!("<b>{}</b>\n", escape_html(&article.title));
    msg.push_str(&format!(
        "{} · {}\n",
        escape_html(&article.category),
        escape_html(&article.source),
    ));

    if let Some(summary) = &article.summary {
        let trimmed = truncate_chars(summary, SUMMARY_MAX_CHARS);
        if !trimmed.is_empty() {
            msg.push_str(&format!("\n{}\n", escape_html(&trimmed)));
        }
    }

    msg.push_str(&format!("\n{}", escape_html(&article.url)));
    msg
}

/// Escape the three characters Telegram's HTML parse mode is sensitive to. Without
/// this, an `&` or `<` in a title would be rejected as malformed entities/tags.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Truncate to at most `max` characters on a char boundary (so we never split a
/// multi-byte UTF-8 character), appending an ellipsis when anything was cut.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The lowercase string a [`Category`] is stored as in the DB (e.g. `Defi` →
/// `"defi"`), matching how `ingest::store_items` writes it. Used to compare the
/// config allowlist against an article's stored category string.
fn category_db_str(category: Category) -> String {
    format!("{category:?}").to_lowercase()
}

/// Human-readable description of the category filter for the startup log line.
fn describe_filter(categories: &[Category]) -> String {
    if categories.is_empty() {
        "all categories".to_string()
    } else {
        let names: Vec<String> = categories.iter().map(|c| category_db_str(*c)).collect();
        format!("categories: {}", names.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(category: &str) -> ArticleSummary {
        ArticleSummary {
            id: 1,
            title: "Title <with> & special".to_string(),
            url: "https://example.com/a?x=1&y=2".to_string(),
            source: "src".to_string(),
            category: category.to_string(),
            summary: Some("A short summary.".to_string()),
            published_at: "2026-06-19T00:00:00Z".to_string(),
            distance: None,
        }
    }

    fn config(categories: Vec<Category>) -> TelegramConfig {
        TelegramConfig {
            bot_token: "token".to_string(),
            chat_id: "123".to_string(),
            categories,
            api_base: TELEGRAM_API_BASE.to_string(),
        }
    }

    #[test]
    fn empty_allowlist_passes_everything() {
        let cfg = config(vec![]);
        assert!(cfg.passes(&summary("defi")));
        assert!(cfg.passes(&summary("ai")));
    }

    #[test]
    fn allowlist_filters_by_category() {
        let cfg = config(vec![Category::Defi]);
        assert!(cfg.passes(&summary("defi")));
        assert!(!cfg.passes(&summary("ai")));
    }

    #[test]
    fn category_db_str_matches_stored_form() {
        // Must match `format!("{:?}", category).to_lowercase()` used at ingest time.
        assert_eq!(category_db_str(Category::Defi), "defi");
        assert_eq!(category_db_str(Category::Ai), "ai");
    }

    #[test]
    fn format_escapes_html_and_includes_key_fields() {
        let msg = format_article(&summary("defi"));
        assert!(msg.contains("&lt;with&gt;"), "angle brackets escaped: {msg}");
        assert!(msg.contains("&amp;"), "ampersand escaped: {msg}");
        assert!(msg.contains("https://example.com/a?x=1&amp;y=2"), "url present: {msg}");
        assert!(msg.contains("src"), "source present: {msg}");
        // The raw, unescaped form must not survive.
        assert!(!msg.contains("<with>"));
    }

    #[test]
    fn truncate_adds_ellipsis_only_when_cut() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
    }
}
