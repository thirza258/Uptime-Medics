//! SQLite pool setup (WAL mode, busy timeout, foreign keys) and embedded migrations.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Timestamps are stored as `YYYY-MM-DD HH:MM:SS` (UTC), the same textual format SQLite's
/// `CURRENT_TIMESTAMP` and `datetime('now')` produce, so comparisons in SQL are consistent.
pub const SQL_DATETIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

pub fn now_sql() -> String {
    to_sql(Utc::now())
}

pub fn to_sql(dt: DateTime<Utc>) -> String {
    dt.format(SQL_DATETIME_FORMAT).to_string()
}

pub async fn init(database_url: &str) -> Result<SqlitePool> {
    if let Some(path) = sqlite_file_path(database_url) {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating database directory {}", parent.display()))?;
            }
        }
    }

    let options = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("parsing DATABASE_URL {database_url:?}"))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_millis(5000));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(15))
        .connect_with(options)
        .await
        .context("connecting to SQLite")?;

    MIGRATOR
        .run(&pool)
        .await
        .context("running database migrations")?;

    tracing::info!(database_url, "database ready (WAL mode)");
    Ok(pool)
}

fn sqlite_file_path(url: &str) -> Option<PathBuf> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))?;
    let rest = rest.split('?').next().unwrap_or("");
    if rest.is_empty() || rest == ":memory:" {
        return None;
    }
    Some(PathBuf::from(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_file_paths() {
        assert_eq!(
            sqlite_file_path("sqlite:///data/um.db"),
            Some(PathBuf::from("/data/um.db"))
        );
        assert_eq!(
            sqlite_file_path("sqlite://data/um.db?mode=rwc"),
            Some(PathBuf::from("data/um.db"))
        );
        assert_eq!(sqlite_file_path("sqlite::memory:"), None);
    }
}
