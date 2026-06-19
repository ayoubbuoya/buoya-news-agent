//! Typed library errors (thiserror). Binary boundaries (`main`, tool fns) add
//! human-readable context via `anyhow`.

use thiserror::Error;

/// Errors raised while loading, parsing, or validating configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse TOML in {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },

    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Errors raised while fetching or parsing an upstream source.
#[derive(Debug, Error)]
pub enum FetchError {
    #[error("http request failed: {0}")]
    Http(#[source] reqwest::Error),

    #[error("failed to parse feed: {0}")]
    Parse(String),
}
