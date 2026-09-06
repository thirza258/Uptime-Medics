//! Session lifecycle on top of the `sessions` table: create on login/register, resolve from a
//! token, revoke on logout.

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::config::Config;
use crate::domain::user::CurrentUser;
use crate::infrastructure::db::{now_sql, to_sql};
use crate::security::auth;

/// Create a session row and return the `Set-Cookie` value carrying its JWT.
pub async fn create_session(
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
) -> anyhow::Result<String> {
    let session_id = Uuid::new_v4().to_string();
    let expires_at =
        chrono::Utc::now() + chrono::Duration::seconds(config.session_ttl.as_secs() as i64);
    sqlx::query("INSERT INTO sessions (id, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(&session_id)
        .bind(user_id)
        .bind(to_sql(expires_at))
        .execute(pool)
        .await?;
    let token = auth::issue_token(config, user_id, &session_id)?;
    Ok(auth::session_cookie(config, &token))
}

/// Resolve a session token to its user; `None` for invalid, expired or revoked sessions.
pub async fn resolve_session(
    pool: &SqlitePool,
    config: &Config,
    token: &str,
) -> Result<Option<CurrentUser>, sqlx::Error> {
    let Some((user_id, session_id)) = auth::decode_token(config, token) else {
        return Ok(None);
    };
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT u.id, u.email FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.id = ? AND s.user_id = ? AND s.expires_at > ?",
    )
    .bind(&session_id)
    .bind(&user_id)
    .bind(now_sql())
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id, email)| CurrentUser { id, email }))
}

/// Revoke the session referenced by `token` (no-op for invalid tokens).
pub async fn revoke_session(
    pool: &SqlitePool,
    config: &Config,
    token: &str,
) -> Result<(), sqlx::Error> {
    if let Some((_, session_id)) = auth::decode_token(config, token) {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(&session_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}
