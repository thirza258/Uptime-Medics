//! Bounded history responses aggregated over the indexed SQLite check log, not the last page.

use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

use crate::domain::check_log::CheckLog;
use crate::domain::history::{HistoryBucket, HistoryRange, HistoryResponse, HistorySummary};
use crate::infrastructure::db::to_sql;

#[derive(sqlx::FromRow)]
struct BucketTotals {
    bucket: i64,
    total_checks: i64,
    successful_checks: i64,
    response_time_sum: i64,
    first_checked_at: DateTime<Utc>,
    last_checked_at: DateTime<Utc>,
}

pub async fn load(
    pool: &SqlitePool,
    monitor_id: &str,
    range: HistoryRange,
    now: DateTime<Utc>,
    retention_hours: Option<i64>,
) -> Result<HistoryResponse, sqlx::Error> {
    // SQLite log timestamps have second precision. Each range/bucket is (start, end], so a
    // boundary check belongs to exactly one bucket and a check at `end` is included.
    let end = DateTime::from_timestamp(now.timestamp(), 0).expect("valid current timestamp");
    let start = end - Duration::seconds(range.seconds());
    let bucket_seconds = range.bucket_seconds();
    let mut buckets: Vec<HistoryBucket> = (0..range.seconds() / bucket_seconds)
        .map(|i| HistoryBucket {
            start: start + Duration::seconds(i * bucket_seconds),
            end: start + Duration::seconds((i + 1) * bucket_seconds),
            summary: HistorySummary::default(),
        })
        .collect();

    // One read snapshot keeps the summary, chart and latest checks consistent during a flush.
    let mut tx = pool.begin().await?;
    let rows = sqlx::query_as::<_, BucketTotals>(
        "SELECT (CAST(strftime('%s', checked_at) AS INTEGER) - ? - 1) / ? AS bucket, \
         COUNT(*) AS total_checks, SUM(CASE WHEN is_successful THEN 1 ELSE 0 END) AS successful_checks, \
         SUM(response_time_ms) AS response_time_sum, \
         MIN(checked_at) AS first_checked_at, MAX(checked_at) AS last_checked_at \
         FROM check_logs WHERE monitor_id = ? AND checked_at > ? AND checked_at <= ? \
         GROUP BY bucket ORDER BY bucket",
    )
    .bind(start.timestamp())
    .bind(bucket_seconds)
    .bind(monitor_id)
    .bind(to_sql(start))
    .bind(to_sql(end))
    .fetch_all(&mut *tx)
    .await?;
    let recent_checks = sqlx::query_as::<_, CheckLog>(
        "SELECT * FROM check_logs WHERE monitor_id = ? AND checked_at > ? AND checked_at <= ? \
         ORDER BY checked_at DESC, id DESC LIMIT 50",
    )
    .bind(monitor_id)
    .bind(to_sql(start))
    .bind(to_sql(end))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut summary = HistorySummary::default();
    let mut response_time_sum = 0;
    for row in rows {
        summary.total_checks += row.total_checks;
        summary.successful_checks += row.successful_checks;
        summary.first_checked_at.get_or_insert(row.first_checked_at);
        summary.last_checked_at = Some(row.last_checked_at);
        response_time_sum += row.response_time_sum;
        let bucket = &mut buckets[row.bucket as usize];
        bucket.summary = HistorySummary {
            total_checks: row.total_checks,
            successful_checks: row.successful_checks,
            first_checked_at: Some(row.first_checked_at),
            last_checked_at: Some(row.last_checked_at),
            ..HistorySummary::default()
        };
        finish_summary(&mut bucket.summary, row.response_time_sum);
    }
    finish_summary(&mut summary, response_time_sum);
    Ok(HistoryResponse {
        range,
        start,
        end,
        bucket_seconds,
        retention_hours,
        summary,
        buckets,
        recent_checks,
    })
}

fn finish_summary(summary: &mut HistorySummary, response_time_sum: i64) {
    summary.failed_checks = summary.total_checks - summary.successful_checks;
    if summary.total_checks > 0 {
        summary.uptime_percent =
            Some(100.0 * summary.successful_checks as f64 / summary.total_checks as f64);
        summary.avg_response_time_ms = Some(response_time_sum as f64 / summary.total_checks as f64);
    }
}
