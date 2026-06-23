//! The agent's tools, plus the registry that binds each tool's advertised
//! metadata to the handler that runs it.
//!
//! A single [`registry`] is the source of truth. The OpenAI function-calling
//! adapter ([`tool_definitions`]) and the neutral [`tool_infos`] view — for other
//! surfaces such as an MCP server — are both derived from it, and [`execute`]
//! dispatches by looking a tool up in the same list. Adding a tool is one registry
//! entry plus its handler; there is no second place to keep in sync.
//!
//! Each tool follows the OpenAI function-calling shape: a JSON-Schema parameter
//! spec the caller fills in, and a handler that runs the matching query and
//! returns a JSON value the model reads back as the tool result.

use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use async_openai::types::chat::{ChatCompletionTool, ChatCompletionTools, FunctionObject};
use serde::Serialize;
use serde_json::{Value, json};

use crate::core::repository::{ArticleSummary, DerivativesRow, Repository};

/// Default number of articles returned by list/search tools when the model does
/// not specify a limit.
const DEFAULT_LIMIT: i64 = 20;
/// Hard cap on rows returned so a single tool call cannot flood the context.
const MAX_LIMIT: i64 = 50;

/// A boxed, borrowing future returned by a tool handler.
type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

/// One callable tool: the metadata advertised to a model plus the handler that
/// runs it against the repository.
struct Tool {
    name: &'static str,
    description: &'static str,
    /// Builds the JSON-Schema describing the tool's parameters.
    parameters: fn() -> Value,
    /// Runs the tool with the raw JSON argument string the caller produced.
    handler: for<'a> fn(&'a Repository, &'a str) -> ToolFuture<'a>,
}

/// The single source of truth for the agent's tools. The OpenAI schema, the
/// neutral info view, and dispatch all derive from this list.
fn registry() -> Vec<Tool> {
    vec![
        Tool {
            name: "semantic_search",
            description: "Semantic (meaning-based) search over stored news articles using vector \
                 similarity. Finds relevant articles even when they don't share the \
                 exact words as the query. Prefer this for conceptual or topical \
                 questions, e.g. \"regulatory risk for stablecoins\" or \"layer-2 \
                 scaling progress\". For an exact ticker or proper name, prefer \
                 search_articles instead. Lower distance means more relevant.",
            parameters: || {
                json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural-language description of what you're looking for."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of articles to return (1-50).",
                            "minimum": 1,
                            "maximum": MAX_LIMIT
                        }
                    },
                    "required": ["query"]
                })
            },
            handler: |repo, args| Box::pin(semantic_search(repo, args)),
        },
        Tool {
            name: "search_articles",
            description: "Exact keyword/substring search over stored news articles. Matches the \
                 query literally against article titles, summaries, and body content. \
                 Best for exact tickers or proper names (e.g. \"HBAR\", \"Coinbase\"). \
                 For conceptual or topical questions, prefer semantic_search.",
            parameters: || {
                json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Keywords or phrase to search for, e.g. \"ethereum etf\"."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of articles to return (1-25).",
                            "minimum": 1,
                            "maximum": MAX_LIMIT
                        }
                    },
                    "required": ["query"]
                })
            },
            handler: |repo, args| Box::pin(search_articles(repo, args)),
        },
        Tool {
            name: "list_recent_articles",
            description: "List the most recently published stored articles, optionally \
                 filtered by category. Use this when the user asks what's new or \
                 what's happening in a given area.",
            parameters: || {
                json!({
                    "type": "object",
                    "properties": {
                        "category": {
                            "type": "string",
                            "description": "Restrict to a single category.",
                            "enum": ["crypto", "ai", "security", "market", "defi"]
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of articles to return (1-25).",
                            "minimum": 1,
                            "maximum": MAX_LIMIT
                        }
                    },
                    "required": []
                })
            },
            handler: |repo, args| Box::pin(list_recent_articles(repo, args)),
        },
        Tool {
            name: "get_article",
            description: "Fetch the full stored record for a single article by its numeric \
                 id, including the body content. Use this after search or list to \
                 read an article in depth.",
            parameters: || {
                json!({
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "integer",
                            "description": "The article id, as returned by search_articles or list_recent_articles."
                        }
                    },
                    "required": ["id"]
                })
            },
            handler: |repo, args| Box::pin(get_article(repo, args)),
        },
        Tool {
            name: "get_market_snapshot",
            description: "Get the latest structured market snapshots: the crypto Fear & Greed \
                 sentiment index, a top-coins-by-market-cap overview with 24h moves, \
                 and total DeFi TVL by chain. Use this for questions about market \
                 sentiment/mood, current prices or movers, or DeFi TVL — not \
                 search_articles. Returns the most recent daily snapshot for each.",
            parameters: || {
                json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })
            },
            handler: |repo, args| Box::pin(get_market_snapshot(repo, args)),
        },
        Tool {
            name: "get_derivatives",
            description: "Get crypto perpetual-futures derivatives metrics that market makers \
                 watch, per tracked symbol (e.g. BTCUSDT, HBARUSDT): open interest \
                 (contracts and USD notional, plus its 24h % change), funding rate, mark \
                 price, the global long/short account ratio (retail crowd), the taker \
                 buy/sell volume ratio (aggressive order flow; >1 = net buying), and the \
                 top-trader long/short ratio by position (smart-money positioning). With \
                 no arguments, returns the latest reading for every tracked symbol. Pass \
                 `symbol` to get that symbol's recent history instead (newest first) for \
                 trend questions like \"is funding rising on ETH?\" or \"is open interest \
                 building on HBAR?\". Use this for positioning, leverage, order-flow, and \
                 funding questions — not get_market_snapshot (spot prices/sentiment/TVL) \
                 or search_articles.",
            parameters: || {
                json!({
                    "type": "object",
                    "properties": {
                        "symbol": {
                            "type": "string",
                            "description": "Optional exchange symbol (e.g. \"HBARUSDT\"). When set, \
                                returns this symbol's recent history newest-first; when omitted, \
                                returns the latest reading for all tracked symbols."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "When `symbol` is set, how many historical readings to \
                                return (1-50).",
                            "minimum": 1,
                            "maximum": MAX_LIMIT
                        }
                    },
                    "required": []
                })
            },
            handler: |repo, args| Box::pin(get_derivatives(repo, args)),
        },
        Tool {
            name: "analyze_coin",
            description: "Assemble a full, grounded market-analysis dossier for one crypto coin \
                 by pulling every signal stored about it: its perpetual-futures \
                 derivatives (open interest + 24h change, funding regime, retail vs \
                 top-trader positioning, taker order flow, 24h price move), market-wide \
                 sentiment, and the most relevant recent news articles. Returns raw \
                 numbers PLUS pre-computed `signals` (each a metric reading paired with a \
                 plain-English interpretation), and a `data_gaps` list noting anything \
                 missing. Call this when the user asks for an analysis, outlook, or \
                 \"what's going on with\" a specific coin (e.g. \"analyze HBAR\", \
                 \"ETH outlook\"). Then write the analysis strictly from this evidence: \
                 a directional read with a confidence level, the key drivers each tied to \
                 a specific number or article, the risks and any conflicting signals, and \
                 explicit caveats for whatever is in data_gaps. Do not invent figures.",
            parameters: || {
                json!({
                    "type": "object",
                    "properties": {
                        "coin": {
                            "type": "string",
                            "description": "Coin name, ticker, or perpetual symbol — e.g. \"HBAR\", \
                                \"hedera\", or \"HBARUSDT\". Resolved to a Binance USDT perpetual \
                                for derivatives and used as a search term for news."
                        }
                    },
                    "required": ["coin"]
                })
            },
            handler: |repo, args| Box::pin(analyze_coin(repo, args)),
        },
    ]
}

/// Neutral, transport-agnostic description of a tool, for advertising to any
/// surface (the OpenAI adapter, an MCP server, …). Handlers are not exposed here;
/// run a tool through [`execute`].
pub struct ToolInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// The tools' metadata, independent of any particular wire format.
pub fn tool_infos() -> Vec<ToolInfo> {
    registry()
        .into_iter()
        .map(|tool| ToolInfo {
            name: tool.name,
            description: tool.description,
            parameters: (tool.parameters)(),
        })
        .collect()
}

/// The set of tools advertised to the model, in OpenAI function-calling shape.
pub fn tool_definitions() -> Vec<ChatCompletionTools> {
    tool_infos()
        .into_iter()
        .map(|info| {
            ChatCompletionTools::Function(ChatCompletionTool {
                function: FunctionObject {
                    name: info.name.to_string(),
                    description: Some(info.description.to_string()),
                    parameters: Some(info.parameters),
                    strict: None,
                },
            })
        })
        .collect()
}

/// Run the tool named `name` with the raw JSON `arguments` string the caller
/// produced. Always returns a string: on failure it returns a JSON object with an
/// `error` field rather than propagating, so a bad tool call becomes feedback the
/// model can recover from instead of aborting the turn.
pub async fn execute(repo: &Repository, name: &str, arguments: &str) -> String {
    let result = match registry().into_iter().find(|tool| tool.name == name) {
        Some(tool) => (tool.handler)(repo, arguments).await,
        None => Err(anyhow::anyhow!("unknown tool: {name}")),
    };

    match result {
        Ok(value) => value.to_string(),
        Err(e) => {
            tracing::warn!("tool {name} failed: {e:#}");
            json!({ "error": format!("{e:#}") }).to_string()
        }
    }
}

/// Parse the model-supplied argument string, tolerating the empty string that
/// some models send for tools with no required arguments.
fn parse_args(arguments: &str) -> Result<Value> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(trimmed).context("tool arguments were not valid JSON")
}

/// Clamp a caller-provided limit into `1..=MAX_LIMIT`, falling back to the
/// default when absent.
fn resolve_limit(args: &Value) -> i64 {
    args.get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT)
}

/// Extract the required, non-empty `query` string argument.
fn require_query(args: &Value) -> Result<&str> {
    args.get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
        .context("missing required `query` argument")
}

async fn semantic_search(repo: &Repository, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let query = require_query(&args)?;
    let limit = resolve_limit(&args);

    let articles = repo.search_semantic(query, limit).await?;
    Ok(json!({ "count": articles.len(), "articles": articles }))
}

async fn search_articles(repo: &Repository, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let query = require_query(&args)?;
    let limit = resolve_limit(&args);

    let articles = repo.search_keyword(query, limit).await?;
    Ok(json!({ "count": articles.len(), "articles": articles }))
}

async fn list_recent_articles(repo: &Repository, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let limit = resolve_limit(&args);
    let category = args.get("category").and_then(Value::as_str);

    let articles = repo.list_recent(category, limit).await?;
    Ok(json!({ "count": articles.len(), "articles": articles }))
}

async fn get_article(repo: &Repository, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let id = args
        .get("id")
        .and_then(Value::as_i64)
        .context("missing required integer `id` argument")?;

    match repo.get_article(id).await? {
        Some(article) => Ok(serde_json::to_value(article)?),
        None => Ok(json!({ "error": format!("no article with id {id}") })),
    }
}

/// Takes the standard handler arguments for registry uniformity; this tool has no
/// parameters, so `_arguments` is ignored.
async fn get_market_snapshot(repo: &Repository, _arguments: &str) -> Result<Value> {
    let snapshots = repo.market_snapshot().await?;
    Ok(json!({ "count": snapshots.len(), "snapshots": snapshots }))
}

/// How many articles to pull from each search strategy before merging.
const ANALYZE_SEARCH_LIMIT: i64 = 12;
/// How many merged articles to keep in the dossier.
const ANALYZE_ARTICLE_CAP: usize = 8;
/// 24h price/OI moves smaller than this (percent) are treated as flat.
const FLAT_PCT: f64 = 1.0;

/// One interpreted market signal: a metric reading paired with a plain-English
/// reading of what it implies. Pre-computing these keeps the eventual analysis
/// anchored to real numbers rather than vibes.
#[derive(Debug, Serialize)]
struct Signal {
    metric: &'static str,
    reading: String,
    interpretation: String,
}

/// Funding rate (per 8h interval) → leverage-crowding read. Binance's baseline is
/// ~0.01%/8h; meaningfully above/below that signals one-sided leverage.
fn funding_signal(rate: f64) -> Signal {
    let annualized = rate * 3.0 * 365.0 * 100.0;
    let interpretation = if rate > 0.0005 {
        "Strongly positive funding: longs pay shorts a steep premium — leverage is crowded long \
         and exposed to a long squeeze."
    } else if rate > 0.0001 {
        "Positive funding: longs pay shorts — a mild long-leverage bias."
    } else if rate < -0.0005 {
        "Strongly negative funding: shorts pay longs heavily — crowded short, exposed to a short \
         squeeze."
    } else if rate < -0.0001 {
        "Negative funding: shorts pay longs — a mild short-leverage bias."
    } else {
        "Near-neutral funding: perpetual leverage is roughly balanced."
    };
    Signal {
        metric: "funding_rate",
        reading: format!("{:.4}% per 8h (~{annualized:.0}% annualized)", rate * 100.0),
        interpretation: interpretation.to_string(),
    }
}

/// Open-interest change vs price change over 24h — the classic four-quadrant read
/// of whether a move is backed by fresh positioning or just unwinding.
fn oi_price_signal(oi_change_pct: f64, price_change_pct: f64) -> Signal {
    let price_up = price_change_pct > FLAT_PCT;
    let price_dn = price_change_pct < -FLAT_PCT;
    let oi_up = oi_change_pct > FLAT_PCT;
    let oi_dn = oi_change_pct < -FLAT_PCT;

    let interpretation = if price_up && oi_up {
        "Open interest rising into a price gain — fresh money / new longs; the move has conviction \
         (bullish continuation)."
    } else if price_up && oi_dn {
        "Price up while open interest falls — short covering rather than new buying; the rally may \
         lack staying power."
    } else if price_dn && oi_up {
        "Open interest building into a price drop — new shorts adding; bearish conviction \
         (bearish continuation)."
    } else if price_dn && oi_dn {
        "Price and open interest both falling — positions unwinding / long liquidation; \
         deleveraging rather than fresh selling."
    } else {
        "No decisive open-interest/price divergence over 24h — positioning looks stable."
    };
    Signal {
        metric: "open_interest_vs_price",
        reading: format!("price {price_change_pct:+.1}% / open interest {oi_change_pct:+.1}% (24h)"),
        interpretation: interpretation.to_string(),
    }
}

/// Retail account long/short vs top-trader position long/short — flags when the
/// crowd and the larger accounts are leaning opposite ways.
fn positioning_signal(retail_ls: f64, top_ls: f64) -> Signal {
    let retail_long = retail_ls > 1.0;
    let top_long = top_ls > 1.0;
    let interpretation = if retail_long && !top_long {
        "Retail is net long while top traders are net short — the crowd may be offside; contrarian \
         caution on longs."
    } else if !retail_long && top_long {
        "Retail is net short while top traders are net long — larger accounts leaning against the \
         crowd; a potential upside setup."
    } else if retail_long && top_long {
        "Retail and top traders are both net long — one-sided positioning; reversal would force a \
         squeeze."
    } else {
        "Retail and top traders are both net short — one-sided bearish positioning."
    };
    Signal {
        metric: "positioning",
        reading: format!("retail accounts L/S {retail_ls:.2}, top traders L/S {top_ls:.2}"),
        interpretation: interpretation.to_string(),
    }
}

/// Taker buy/sell volume ratio → aggressive order-flow direction.
fn taker_flow_signal(ratio: f64) -> Signal {
    let interpretation = if ratio > 1.05 {
        "Aggressive flow skews to buyers — takers lifting offers (net buying pressure)."
    } else if ratio < 0.95 {
        "Aggressive flow skews to sellers — takers hitting bids (net selling pressure)."
    } else {
        "Aggressive buy/sell flow is roughly balanced."
    };
    Signal {
        metric: "taker_order_flow",
        reading: format!("taker buy/sell volume ratio {ratio:.2}"),
        interpretation: interpretation.to_string(),
    }
}

/// Derive the derivatives-structure signals present in `row`. Each is emitted only
/// when its inputs exist, so a partial reading still yields whatever is grounded.
fn derivatives_signals(row: &DerivativesRow) -> Vec<Signal> {
    let mut signals = Vec::new();
    if let Some(rate) = row.funding_rate {
        signals.push(funding_signal(rate));
    }
    if let (Some(oi), Some(price)) =
        (row.open_interest_usd_change_24h_pct, row.mark_price_change_24h_pct)
    {
        signals.push(oi_price_signal(oi, price));
    }
    if let (Some(retail), Some(top)) = (row.long_short_ratio, row.top_trader_long_short_ratio) {
        signals.push(positioning_signal(retail, top));
    }
    if let Some(ratio) = row.taker_buy_sell_ratio {
        signals.push(taker_flow_signal(ratio));
    }
    signals
}

/// Resolve a user-supplied coin reference to a Binance USDT perpetual symbol:
/// uppercase, and append `USDT` unless it's already a `…USDT` symbol.
fn resolve_symbol(coin: &str) -> String {
    let upper = coin.trim().to_uppercase();
    if upper.ends_with("USDT") {
        upper
    } else {
        format!("{upper}USDT")
    }
}

/// Merge keyword and semantic article hits, de-duplicating by id (keyword first,
/// as exact matches are the more reliable), newest first, capped for the dossier.
fn merge_articles(
    keyword: Vec<ArticleSummary>,
    semantic: Vec<ArticleSummary>,
) -> Vec<ArticleSummary> {
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut merged: Vec<ArticleSummary> = Vec::new();
    for article in keyword.into_iter().chain(semantic) {
        if seen.insert(article.id) {
            merged.push(article);
        }
    }
    merged.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    merged.truncate(ANALYZE_ARTICLE_CAP);
    merged
}

async fn analyze_coin(repo: &Repository, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let coin = args
        .get("coin")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .context("missing required `coin` argument")?;
    let symbol = resolve_symbol(coin);

    // Derivatives for this symbol (with 24h OI/price deltas), if tracked.
    let derivatives = repo
        .latest_derivatives()
        .await?
        .into_iter()
        .find(|d| d.symbol.eq_ignore_ascii_case(&symbol));

    // News: exact-name keyword hits plus a meaning-based sweep for catalysts/risks.
    let keyword = repo.search_keyword(coin, ANALYZE_SEARCH_LIMIT).await?;
    let semantic = repo
        .search_semantic(
            &format!("{coin} crypto price outlook, catalysts, risks, regulation, adoption"),
            ANALYZE_SEARCH_LIMIT,
        )
        .await?;
    let articles = merge_articles(keyword, semantic);

    // Market-wide context (sentiment, movers, TVL) — not coin-specific.
    let market_context = repo.market_snapshot().await?;

    // Pre-computed, evidence-tagged signals.
    let mut signals = derivatives.as_ref().map(derivatives_signals).unwrap_or_default();
    if let Some(sentiment) = market_context.iter().find(|s| s.source == "fear-greed") {
        let reading = sentiment
            .summary
            .clone()
            .or_else(|| sentiment.content.clone())
            .unwrap_or_else(|| sentiment.title.clone());
        signals.push(Signal {
            metric: "market_sentiment",
            reading,
            interpretation: "Market-wide Fear & Greed (not specific to this coin) — context for \
                how risk appetite may amplify or dampen the coin's own signals."
                .to_string(),
        });
    }

    // Be explicit about what's missing so the analysis can caveat rather than guess.
    let mut data_gaps: Vec<String> = Vec::new();
    if derivatives.is_none() {
        data_gaps.push(format!(
            "No derivatives data for {symbol} — the coin may not have a Binance USDT perpetual, \
             or it isn't in the tracked symbols list yet."
        ));
    } else if signals
        .iter()
        .all(|s| s.metric == "market_sentiment")
    {
        data_gaps.push(format!(
            "Derivatives exist for {symbol} but 24h-change and positioning fields are still \
             filling in (need ~24h of history)."
        ));
    }
    if articles.is_empty() {
        data_gaps.push(format!("No stored news articles mention \"{coin}\"."));
    }

    Ok(json!({
        "coin": coin,
        "symbol": symbol,
        "as_of": chrono::Utc::now().to_rfc3339(),
        "derivatives": derivatives,
        "signals": signals,
        "market_context": market_context,
        "articles": articles,
        "data_gaps": data_gaps,
    }))
}

async fn get_derivatives(repo: &Repository, arguments: &str) -> Result<Value> {
    let args = parse_args(arguments)?;
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match symbol {
        Some(symbol) => {
            let limit = resolve_limit(&args);
            let history = repo.derivatives_history(symbol, limit).await?;
            Ok(json!({ "symbol": symbol, "count": history.len(), "readings": history }))
        }
        None => {
            let latest = repo.latest_derivatives().await?;
            Ok(json!({ "count": latest.len(), "derivatives": latest }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn article(id: i64, published_at: &str) -> ArticleSummary {
        ArticleSummary {
            id,
            title: format!("article {id}"),
            url: format!("https://example.com/{id}"),
            source: "test".into(),
            category: "crypto".into(),
            summary: None,
            published_at: published_at.into(),
            distance: None,
        }
    }

    fn empty_row(symbol: &str) -> DerivativesRow {
        DerivativesRow {
            id: 1,
            symbol: symbol.into(),
            open_interest: None,
            open_interest_usd: None,
            funding_rate: None,
            mark_price: None,
            long_short_ratio: None,
            long_account: None,
            short_account: None,
            taker_buy_sell_ratio: None,
            taker_buy_vol: None,
            taker_sell_vol: None,
            top_trader_long_short_ratio: None,
            top_trader_long_account: None,
            top_trader_short_account: None,
            next_funding_time: None,
            fetched_at: "2026-06-23T00:00:00Z".into(),
            open_interest_usd_change_24h_pct: None,
            mark_price_change_24h_pct: None,
        }
    }

    #[test]
    fn resolve_symbol_appends_usdt_unless_present() {
        assert_eq!(resolve_symbol("hbar"), "HBARUSDT");
        assert_eq!(resolve_symbol("  eth "), "ETHUSDT");
        assert_eq!(resolve_symbol("HBARUSDT"), "HBARUSDT");
    }

    #[test]
    fn funding_signal_buckets_by_magnitude() {
        assert!(funding_signal(0.001).interpretation.contains("crowded long"));
        assert!(funding_signal(-0.001).interpretation.contains("crowded short"));
        assert!(funding_signal(0.00001).interpretation.contains("balanced"));
    }

    #[test]
    fn oi_price_signal_reads_the_four_quadrants() {
        assert!(oi_price_signal(5.0, 5.0).interpretation.contains("conviction"));
        assert!(oi_price_signal(5.0, -5.0).interpretation.contains("bearish"));
        assert!(oi_price_signal(-5.0, 5.0).interpretation.contains("short covering"));
        assert!(oi_price_signal(-5.0, -5.0).interpretation.contains("deleveraging"));
        assert!(oi_price_signal(0.2, 0.2).interpretation.contains("stable"));
    }

    #[test]
    fn positioning_signal_flags_crowd_vs_smart_money() {
        assert!(positioning_signal(1.5, 0.8).interpretation.contains("offside"));
        assert!(positioning_signal(0.8, 1.5).interpretation.contains("against the crowd"));
    }

    #[test]
    fn taker_flow_signal_reads_direction() {
        assert!(taker_flow_signal(1.2).interpretation.contains("buyers"));
        assert!(taker_flow_signal(0.8).interpretation.contains("sellers"));
        assert!(taker_flow_signal(1.0).interpretation.contains("balanced"));
    }

    #[test]
    fn derivatives_signals_emitted_only_when_inputs_present() {
        // Empty row → no derivative signals.
        assert!(derivatives_signals(&empty_row("HBARUSDT")).is_empty());

        // Fully-populated row → all four structure signals.
        let mut row = empty_row("HBARUSDT");
        row.funding_rate = Some(0.0002);
        row.open_interest_usd_change_24h_pct = Some(3.0);
        row.mark_price_change_24h_pct = Some(2.0);
        row.long_short_ratio = Some(1.4);
        row.top_trader_long_short_ratio = Some(0.9);
        row.taker_buy_sell_ratio = Some(1.1);
        let metrics: Vec<&str> = derivatives_signals(&row).iter().map(|s| s.metric).collect();
        assert_eq!(
            metrics,
            ["funding_rate", "open_interest_vs_price", "positioning", "taker_order_flow"]
        );
    }

    #[test]
    fn merge_articles_dedups_sorts_newest_first_and_caps() {
        let keyword = vec![article(1, "2026-06-20T00:00:00Z"), article(2, "2026-06-22T00:00:00Z")];
        // id 2 duplicated across strategies; id 3 is the newest.
        let semantic = vec![article(2, "2026-06-22T00:00:00Z"), article(3, "2026-06-23T00:00:00Z")];

        let merged = merge_articles(keyword, semantic);
        let ids: Vec<i64> = merged.iter().map(|a| a.id).collect();
        assert_eq!(ids, [3, 2, 1], "deduped and newest-first");
    }
}
