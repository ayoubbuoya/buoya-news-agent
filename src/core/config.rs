//! Configuration: serde structs mirroring `config.default.toml`

use std::path::Path;

use dotenvy::dotenv;
use serde::Deserialize;

use crate::core::error::ConfigError;
use crate::core::types::Category;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TomlConfig {
    #[serde(default)]
    pub sources: Sources,
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub http: Http,
    /// Outbound connectors (Telegram, …). Absent in config = all disabled.
    #[serde(default)]
    pub connectors: Connectors,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sources {
    pub rss: Vec<RssSource>,
    pub defillama: DefillamaSource,
    pub coingecko: CoingeckoSource,
    pub cryptopanic: CryptopanicSource,
    pub fear_greed: FearGreedSource,
    pub reddit: RedditSource,
    pub arxiv: ArxivSource,
    pub huggingface: HuggingfaceSource,
    pub derivatives: DerivativesSource,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RssSource {
    pub name: String,
    pub url: String,
    pub category: Category,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DefillamaSource {
    pub enabled: bool,
}

impl Default for DefillamaSource {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoingeckoSource {
    pub enabled: bool,
    pub top_n: u32,
}

impl Default for CoingeckoSource {
    fn default() -> Self {
        Self {
            enabled: true,
            top_n: 100,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CryptopanicSource {
    pub enabled: bool,
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FearGreedSource {
    pub enabled: bool,
}

impl Default for FearGreedSource {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedditSource {
    pub enabled: bool,
    pub subreddits: Vec<String>,
    pub min_upvotes: u32,
}

impl Default for RedditSource {
    fn default() -> Self {
        Self {
            enabled: true,
            subreddits: vec![
                "CryptoCurrency".into(),
                "MachineLearning".into(),
                "LocalLLaMA".into(),
                "ethereum".into(),
            ],
            min_upvotes: 200,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArxivSource {
    pub enabled: bool,
    pub categories: Vec<String>,
    pub max_per_run: u32,
}

impl Default for ArxivSource {
    fn default() -> Self {
        Self {
            enabled: true,
            categories: vec!["cs.AI".into(), "cs.LG".into(), "cs.CL".into()],
            max_per_run: 25,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HuggingfaceSource {
    pub enabled: bool,
}

impl Default for HuggingfaceSource {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Crypto-futures derivatives metrics (open interest, funding rate, long/short
/// ratio) pulled per symbol from Binance's keyless USDⓈ-M public API. These are the
/// numbers market makers watch; stored structured (not as articles) in the
/// `derivatives` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DerivativesSource {
    pub enabled: bool,
    /// Perpetual contract symbols to track, in Binance's exchange notation (e.g.
    /// `"BTCUSDT"`, `"HBARUSDT"`). Not the same as `general.watchlist`, which mixes
    /// coin names and tickers — derivatives need exact exchange symbols.
    pub symbols: Vec<String>,
    /// Aggregation period for the long/short-ratio endpoint. Binance accepts
    /// `5m`, `15m`, `30m`, `1h`, `2h`, `4h`, `6h`, `12h`, `1d`.
    pub long_short_period: String,
}

impl Default for DerivativesSource {
    fn default() -> Self {
        Self {
            enabled: true,
            symbols: vec![
                "BTCUSDT".into(),
                "ETHUSDT".into(),
                "SOLUSDT".into(),
                "HBARUSDT".into(),
                "XLMUSDT".into(),
            ],
            long_short_period: "5m".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub watchlist: Vec<String>,
    pub retention_days: u32,
    /// How often to re-run ingest, in seconds. Ingest also runs once at startup.
    pub ingest_interval_secs: u64,
}

impl Default for General {
    fn default() -> Self {
        Self {
            watchlist: vec![
                "hedera".into(),
                "hbar".into(),
                "stellar".into(),
                "xlm".into(),
                "ethereum".into(),
            ],
            retention_days: 90,
            ingest_interval_secs: 900,
        }
    }
}

/// Push/notification connectors. Each connector is opt-in (disabled by default) so
/// the daemon runs fine with none configured.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Connectors {
    pub telegram: TelegramToml,
}

/// Non-secret Telegram connector settings. The bot **token is a secret** and lives
/// in the environment (`TELEGRAM_BOT_TOKEN`), not here — see [`AppConfig`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelegramToml {
    /// Master switch. When false, the connector never starts regardless of token.
    pub enabled: bool,
    /// Destination chat/group/channel id that alerts are sent to.
    pub chat_id: String,
    /// Category allowlist for alerts. Empty = send every category.
    pub categories: Vec<Category>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Http {
    pub timeout_ms: u64,
    pub user_agent: String,
}

impl Default for Http {
    fn default() -> Self {
        Self {
            timeout_ms: 15_000,
            user_agent: "buoya-news-agent/1.0 (personal aggregator)".into(),
        }
    }
}

impl TomlConfig {
    /// Load defaults from `default_path`, deep-merge `user_path` on top if it
    /// exists, deserialize, and validate.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let toml_config_str = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
            path: path.display().to_string(),
            source: e,
        })?;

        let toml_config: toml::Value =
            toml::from_str(&toml_config_str).map_err(|e| ConfigError::Parse {
                path: path.display().to_string(),
                source: e,
            })?;

        let cfg: TomlConfig = toml_config.try_into().map_err(|e| ConfigError::Parse {
            path: path.display().to_string(),
            source: e,
        })?;

        // Validate the loaded config
        cfg.validate()?;

        Ok(cfg)
    }

    /// Deserialize from a single TOML string (no merge). Used by tests and any
    /// caller that already holds the merged source.
    pub fn from_str(toml_str: &str) -> Result<Self, ConfigError> {
        let cfg: TomlConfig = toml::from_str(toml_str).map_err(|e| ConfigError::Parse {
            path: "<string>".into(),
            source: e,
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Reject configurations that parse but cannot produce sensible behavior.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let inv = |m: String| ConfigError::Invalid(m);

        if self.sources.cryptopanic.enabled && self.sources.cryptopanic.api_key.trim().is_empty() {
            return Err(inv(
                "sources.cryptopanic is enabled but api_key is empty".into()
            ));
        }
        // A Telegram connector with no destination can't do anything useful. The bot
        // token isn't checked here — it's an env var, not part of TomlConfig — so the
        // `serve` wiring handles a missing token by simply not starting the connector.
        if self.connectors.telegram.enabled && self.connectors.telegram.chat_id.trim().is_empty() {
            return Err(inv(
                "connectors.telegram is enabled but chat_id is empty".into()
            ));
        }
        if self.sources.derivatives.enabled && self.sources.derivatives.symbols.is_empty() {
            return Err(inv(
                "sources.derivatives is enabled but symbols is empty".into()
            ));
        }
        if self.sources.derivatives.enabled
            && self.sources.derivatives.long_short_period.trim().is_empty()
        {
            return Err(inv(
                "sources.derivatives is enabled but long_short_period is empty".into(),
            ));
        }
        if !self.any_source_enabled() {
            return Err(inv("at least one source must be enabled".into()));
        }
        if self.general.ingest_interval_secs == 0 {
            return Err(inv(
                "general.ingest_interval_secs must be greater than 0".into()
            ));
        }
        Ok(())
    }

    fn any_source_enabled(&self) -> bool {
        !self.sources.rss.is_empty()
            || self.sources.defillama.enabled
            || self.sources.coingecko.enabled
            || self.sources.cryptopanic.enabled
            || self.sources.fear_greed.enabled
            || self.sources.reddit.enabled
            || self.sources.arxiv.enabled
            || self.sources.huggingface.enabled
            || self.sources.derivatives.enabled
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub ai_api_key: String,
    pub ai_base_url: String,
    pub ai_model: String,
    pub toml_config: TomlConfig,
    /// Telegram bot token, read from `TELEGRAM_BOT_TOKEN`. `None` when unset — the
    /// connector simply won't start. Kept out of `toml_config` because it's a secret
    /// and belongs in the environment, not a committed/loaded TOML file.
    pub telegram_bot_token: Option<String>,
}

impl AppConfig {
    pub fn load(toml_config_path: &Path) -> anyhow::Result<Self> {
        dotenv().ok();

        let ai_api_key = std::env::var("AI_API_KEY")
            .map_err(|_| anyhow::anyhow!("AI_API_KEY env var not set"))?;
        let ai_base_url =
            std::env::var("AI_BASE_URL").unwrap_or(String::from("https://openrouter.ai/api/v1"));
        let ai_model = std::env::var("AI_MODEL").unwrap_or(String::from("openai/gpt-oss-20b:free"));
        let toml_config = TomlConfig::load(toml_config_path)?;

        // Optional: only present when the user wants the Telegram connector. A blank
        // value is treated the same as unset so an empty `TELEGRAM_BOT_TOKEN=` line
        // doesn't pass the "token present" gate.
        let telegram_bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty());

        Ok(Self {
            ai_api_key,
            ai_base_url,
            ai_model,
            toml_config,
            telegram_bot_token,
        })
    }
}
