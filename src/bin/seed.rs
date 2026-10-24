//! Seed the database with popular public websites (including AI services) and backdated check history so a fresh
//! deployment looks lived-in. Safe to re-run: monitors whose URL is already monitored are
//! skipped. All seeded monitors are anonymous and active, so the live probe scheduler takes
//! over and keeps them fresh right after seeding.
//!
//! Usage: `uptime-medics-seed [--reset]` (uses $DATABASE_URL, default
//! `sqlite://data/uptime_medics.db`). `--reset` deletes existing *anonymous* monitors for the
//! seed URLs and recreates them (handy when tweaking the seed); without it they are skipped.

use std::env;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use uptime_medics::infrastructure::db;

/// (name, url, check_interval_seconds, base_response_ms, created_days_ago)
const SEED_SITES: &[(&str, &str, i64, i64, i64)] = &[
    ("Google", "https://www.google.com", 120, 90, 88),
    ("GitHub", "https://github.com", 120, 350, 76),
    ("Cloudflare", "https://www.cloudflare.com", 120, 250, 64),
    ("Wikipedia", "https://en.wikipedia.org", 300, 400, 52),
    ("Hacker News", "https://news.ycombinator.com", 300, 260, 40),
    ("Mozilla", "https://www.mozilla.org", 300, 300, 31),
    ("Rust", "https://www.rust-lang.org", 300, 300, 30),
    ("Python", "https://www.python.org", 300, 360, 26),
    ("Docker", "https://www.docker.com", 300, 380, 22),
    ("Netlify", "https://www.netlify.com", 300, 300, 18),
    ("Vercel", "https://vercel.com", 300, 320, 15),
    ("GitHub API", "https://api.github.com", 300, 300, 12),
    ("httpbin", "https://httpbin.org", 300, 260, 9),
    ("Example", "https://example.com", 600, 110, 6),
    ("Internet Archive", "https://archive.org", 600, 500, 4),
    ("icanhazip", "https://icanhazip.com", 300, 150, 2),
    ("Claude Code", "https://status.anthropic.com", 120, 300, 3),
    ("Codex", "https://status.openai.com", 120, 350, 2),
    ("OpenRouter", "https://openrouter.ai", 120, 400, 2),
    ("DeepSeek", "https://status.deepseek.com", 120, 450, 1),
];

/// How far back the backdated check history reaches. Kept just under the default 48 h
/// anonymous log retention so nothing visible disappears on the first cleanup run.
const HISTORY_HOURS: i64 = 47;

/// Probe timeout used by every seeded monitor; timeout-style failures reference it.
const TIMEOUT_SECONDS: i64 = 10;

struct LogRow {
    checked_at: DateTime<Utc>,
    status_code: Option<i64>,
    response_time_ms: i64,
    is_successful: bool,
    error_message: Option<String>,
}

/// Tiny deterministic PRNG (xorshift64) so the seed needs no extra dependencies.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    let database_url =
        env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/uptime_medics.db".to_string());
    let pool = db::init(&database_url).await?;
    let reset = env::args().any(|arg| arg == "--reset");

    let mut created = 0usize;
    let mut skipped = 0usize;
    let mut total_logs = 0usize;
    for (index, &(name, url, interval, base_ms, created_days_ago)) in SEED_SITES.iter().enumerate()
    {
        if let Some(existing_id) = existing_monitor(&pool, url).await? {
            if !reset {
                println!("skip  {name:<16} {url} (already monitored: {existing_id})");
                skipped += 1;
                continue;
            }
            // Only anonymous monitors are reset: a registered user's monitor is never touched.
            sqlx::query("DELETE FROM monitors WHERE url = ? AND is_anonymous = 1")
                .bind(url)
                .execute(&pool)
                .await?;
            println!("reset {name:<16} {url} (was {existing_id})");
        }
        let logs = build_logs(interval, base_ms, index as u64);
        let id = insert_monitor(&pool, name, url, interval, created_days_ago, &logs).await?;
        println!("seed  {name:<16} {url} -> {id} ({:>4} logs)", logs.len());
        created += 1;
        total_logs += logs.len();
    }
    println!(
        "done: {created} monitors created, {skipped} already present, {total_logs} check logs inserted"
    );
    Ok(())
}

async fn existing_monitor(pool: &SqlitePool, url: &str) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM monitors WHERE url = ? LIMIT 1")
            .bind(url)
            .fetch_optional(pool)
            .await?,
    )
}

/// Backdated check history: mostly 200s with per-site response times, plus one or two small
/// incident windows (a timeout burst or a 503 burst) that recovered. The most recent checks
/// are always healthy so the seeded `UP` status matches the latest log.
fn build_logs(interval_secs: i64, base_ms: i64, salt: u64) -> Vec<LogRow> {
    let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15 ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let now = Utc::now();

    // Spread the latest check across the last interval so the scheduler staggers the first
    // real probes instead of firing all of them at once.
    let latest = now - Duration::seconds(rng.range(5, interval_secs.max(6) as u64 - 1) as i64);

    let step = Duration::seconds(interval_secs);
    let count = (Duration::hours(HISTORY_HOURS).num_seconds() / step.num_seconds()) as usize;
    let first = latest - step * (count as i32 - 1);
    let jitter_span = (interval_secs / 4) as u64;

    let mut rows: Vec<LogRow> = (0..count)
        .map(|k| {
            let checked_at =
                first + step * k as i32 + Duration::seconds(rng.range(0, jitter_span) as i64);
            let slow = rng.next() % 12 == 0; // ~8% of healthy checks are sluggish
            let multiplier = if slow { 2 + rng.next() % 3 } else { 1 };
            let response_time_ms = (base_ms as u64 + rng.range(0, base_ms as u64)) * multiplier;
            LogRow {
                checked_at,
                status_code: Some(200),
                response_time_ms: response_time_ms as i64,
                is_successful: true,
                error_message: None,
            }
        })
        .collect();

    // Incident windows: 1-2 per site, each lasting 2-4 checks, always followed by recovery.
    // Most land inside the last ~50 rows (what the logs modal displays) so the history visibly
    // blips; the newest dozen rows are always healthy so the seeded `UP` status matches the
    // latest log. A third of the sites only blip deeper in history for variety.
    let incidents = 1 + rng.next() % 2;
    for i in 0..incidents {
        let visible = salt % 3 != 0 && (i == 0 || rng.next() % 2 == 0);
        let start = if visible {
            rng.range(count as u64 - 46, count as u64 - 16) as usize
        } else {
            rng.range(60, (count - 56) as u64) as usize
        };
        let duration = rng.range(2, 4) as usize;
        let timeout = rng.next() % 2 == 0;
        for row in rows.iter_mut().skip(start).take(duration) {
            row.is_successful = false;
            if timeout {
                row.status_code = None;
                row.response_time_ms = TIMEOUT_SECONDS * 1000;
                row.error_message = Some(format!(
                    "TIMEOUT_EXCEEDED (no response within {TIMEOUT_SECONDS}s)"
                ));
            } else {
                row.status_code = Some(503);
                row.response_time_ms = base_ms + rng.range(0, base_ms as u64) as i64;
                row.error_message =
                    Some("Status 503 Service Unavailable (expected 200)".to_string());
            }
        }
    }
    rows
}

async fn insert_monitor(
    pool: &SqlitePool,
    name: &str,
    url: &str,
    interval_secs: i64,
    created_days_ago: i64,
    logs: &[LogRow],
) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let hash = id
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    let created_at =
        Utc::now() - Duration::hours(created_days_ago * 24) - Duration::hours((hash % 24) as i64);
    let last_checked_at = logs.last().expect("history is never empty").checked_at;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO monitors (id, user_id, name, url, method, headers, body, check_interval_seconds, \
         timeout_seconds, expected_status_code, alert_email, is_anonymous, is_active, current_status, \
         consecutive_failures, last_checked_at, created_at) \
         VALUES (?, NULL, ?, ?, 'GET', NULL, NULL, ?, ?, 200, NULL, 1, 1, 'UP', 0, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(url)
    .bind(interval_secs)
    .bind(TIMEOUT_SECONDS)
    .bind(db::to_sql(last_checked_at))
    .bind(db::to_sql(created_at))
    .execute(&mut *tx)
    .await?;

    for row in logs {
        sqlx::query(
            "INSERT INTO check_logs (monitor_id, status_code, response_time_ms, is_successful, \
             error_message, checked_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(row.status_code)
        .bind(row.response_time_ms)
        .bind(row.is_successful)
        .bind(&row.error_message)
        .bind(db::to_sql(row.checked_at))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(id)
}
