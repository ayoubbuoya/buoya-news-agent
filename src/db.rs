use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;

pub async fn init_db() -> Result<SqlitePool> {
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL must be set in environment")?;

    let opts = SqliteConnectOptions::from_str(&database_url)
        .context("invalid DATABASE_URL")?
        .create_if_missing(true);

    if let Some(parent) = opts.get_filename().parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pool = SqlitePool::connect_with(opts).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS articles (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            title       TEXT NOT NULL,
            url         TEXT NOT NULL UNIQUE,
            source      TEXT NOT NULL,
            category    TEXT NOT NULL,
            summary     TEXT,
            content     TEXT,
            published_at TEXT NOT NULL,
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}
