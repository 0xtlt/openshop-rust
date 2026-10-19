//! What the operator dashboard reads beyond the records of the framework:
//! filtered lists, counts per bucket of time, per flow and per shop. Every
//! statement only reads, and none reads the access token of a shop or the
//! configuration of a provider.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::db::FlowRun;

/// The periods a page can look at, with the size of a point of its chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Range {
    Hour,
    Day,
    Week,
    Month,
}

impl Range {
    pub(super) const ALL: [Range; 4] = [Range::Hour, Range::Day, Range::Week, Range::Month];

    /// The range a query string names, the last day when it names none.
    pub(super) fn parse(value: Option<&str>) -> Self {
        Self::ALL
            .into_iter()
            .find(|range| Some(range.key()) == value)
            .unwrap_or(Range::Day)
    }

    pub(super) const fn key(self) -> &'static str {
        match self {
            Range::Hour => "1h",
            Range::Day => "24h",
            Range::Week => "7d",
            Range::Month => "30d",
        }
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Range::Hour => "the last hour",
            Range::Day => "the last 24 hours",
            Range::Week => "the last 7 days",
            Range::Month => "the last 30 days",
        }
    }

    fn duration(self) -> Duration {
        match self {
            Range::Hour => Duration::hours(1),
            Range::Day => Duration::hours(24),
            Range::Week => Duration::days(7),
            Range::Month => Duration::days(30),
        }
    }

    /// The time one point of a chart covers.
    pub(super) fn bucket(self) -> Duration {
        match self {
            Range::Hour => Duration::minutes(2),
            Range::Day => Duration::minutes(30),
            Range::Week => Duration::hours(3),
            Range::Month => Duration::hours(12),
        }
    }

    /// Where the period starts for `now`: on a bucket boundary, so that the
    /// points of a chart keep their place from one load to the next.
    pub(super) fn start(self, now: DateTime<Utc>) -> DateTime<Utc> {
        let bucket = self.bucket().num_seconds();
        let start = (now - self.duration()).timestamp().div_euclid(bucket) * bucket;
        DateTime::from_timestamp(start, 0).unwrap_or(now)
    }

    /// How many buckets lead from the start of the period to `now`.
    pub(super) fn buckets(self, now: DateTime<Utc>) -> usize {
        let elapsed = (now - self.start(now)).num_seconds();
        (elapsed / self.bucket().num_seconds()) as usize + 1
    }
}

/// `%text%` for `ILIKE`, with what `LIKE` reads as a pattern escaped.
fn containing(text: &str) -> String {
    let mut pattern = String::with_capacity(text.len() + 2);
    pattern.push('%');
    for character in text.chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

type Sql = QueryBuilder<'static, Postgres>;

/// Appends ` WHERE` before the first condition and ` AND` before the others.
struct Conditions {
    any: bool,
}

impl Conditions {
    fn new() -> Self {
        Self { any: false }
    }

    fn next(&mut self, query: &mut Sql) {
        query.push(if self.any { " AND " } else { " WHERE " });
        self.any = true;
    }
}

/// Which runs a page looks at. `text` matches a part of the error, and
/// `shop` a part of the shop domain, or the whole of it when `exact_shop`.
#[derive(Debug, Clone, Default)]
pub(super) struct RunFilter {
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub status: Option<String>,
    pub flow: Option<String>,
    pub shop: Option<String>,
    pub exact_shop: bool,
    pub text: Option<String>,
}

impl RunFilter {
    fn push(&self, query: &mut Sql) {
        let mut conditions = Conditions::new();
        if let Some(since) = self.since {
            conditions.next(query);
            query.push("created_at >= ").push_bind(since);
        }
        if let Some(before) = self.before {
            conditions.next(query);
            query.push("created_at < ").push_bind(before);
        }
        if let Some(status) = &self.status {
            conditions.next(query);
            query.push("status = ").push_bind(status.clone());
        }
        if let Some(flow) = &self.flow {
            conditions.next(query);
            query.push("flow_name = ").push_bind(flow.clone());
        }
        if let Some(shop) = &self.shop {
            conditions.next(query);
            if self.exact_shop {
                query.push("shop = ").push_bind(shop.clone());
            } else {
                query.push("shop ILIKE ").push_bind(containing(shop));
            }
        }
        if let Some(text) = &self.text {
            conditions.next(query);
            query.push("error ILIKE ").push_bind(containing(text));
        }
    }
}

/// The runs a filter keeps, the latest first.
pub(super) async fn runs(db: &PgPool, filter: &RunFilter, limit: i64) -> Result<Vec<FlowRun>> {
    let mut query = Sql::new("SELECT * FROM flow_runs");
    filter.push(&mut query);
    query.push(" ORDER BY created_at DESC LIMIT ").push_bind(limit);
    Ok(query.build_query_as().fetch_all(db).await?)
}

/// Counts per bucket: what a chart draws.
pub(super) struct Counts {
    pub total: Vec<i64>,
    /// The failed runs, or the log lines of level `error`.
    pub bad: Vec<i64>,
}

/// Puts the counts of the buckets a statement found in their place, the
/// buckets without anything counting zero.
fn spread(rows: Vec<(i64, i64, i64)>, buckets: usize) -> Counts {
    let mut counts = Counts {
        total: vec![0; buckets],
        bad: vec![0; buckets],
    };
    for (bucket, total, bad) in rows {
        if let Some(index) = usize::try_from(bucket).ok().filter(|index| *index < buckets) {
            counts.total[index] = total;
            counts.bad[index] = bad;
        }
    }
    counts
}

/// The number of the bucket a timestamp falls in, counted from `start`.
fn push_bucket(query: &mut Sql, column: &str, start: DateTime<Utc>, bucket: Duration) {
    query
        .push(format!("floor(extract(epoch FROM ({column} - "))
        .push_bind(start)
        .push("))::float8 / ")
        .push_bind(bucket.num_seconds() as f64)
        .push(")::bigint");
}

/// The runs a filter keeps, and those that failed, per bucket from `start`.
pub(super) async fn run_counts(
    db: &PgPool,
    filter: &RunFilter,
    start: DateTime<Utc>,
    bucket: Duration,
    buckets: usize,
) -> Result<Counts> {
    let mut query = Sql::new("SELECT ");
    push_bucket(&mut query, "created_at", start, bucket);
    query.push(", count(*), count(*) FILTER (WHERE status = 'failed') FROM flow_runs");
    RunFilter {
        since: Some(start),
        before: None,
        ..filter.clone()
    }
    .push(&mut query);
    query.push(" GROUP BY 1");
    Ok(spread(query.build_query_as().fetch_all(db).await?, buckets))
}

/// Which log lines a page looks at. `shop` and `text` match a part of the
/// shop domain and of the message.
#[derive(Debug, Clone, Default)]
pub(super) struct LogFilter {
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub level: Option<String>,
    pub flow: Option<String>,
    pub shop: Option<String>,
    pub text: Option<String>,
}

impl LogFilter {
    fn push(&self, query: &mut Sql) {
        let mut conditions = Conditions::new();
        if let Some(since) = self.since {
            conditions.next(query);
            query.push("l.created_at >= ").push_bind(since);
        }
        if let Some(before) = self.before {
            conditions.next(query);
            query.push("l.created_at < ").push_bind(before);
        }
        if let Some(level) = &self.level {
            conditions.next(query);
            query.push("l.level = ").push_bind(level.clone());
        }
        if let Some(flow) = &self.flow {
            conditions.next(query);
            query.push("r.flow_name = ").push_bind(flow.clone());
        }
        if let Some(shop) = &self.shop {
            conditions.next(query);
            query.push("r.shop ILIKE ").push_bind(containing(shop));
        }
        if let Some(text) = &self.text {
            conditions.next(query);
            query.push("l.message ILIKE ").push_bind(containing(text));
        }
    }
}

/// A log line, with the run that wrote it.
#[derive(Debug, FromRow)]
pub(super) struct LogLine {
    pub created_at: DateTime<Utc>,
    pub level: String,
    pub message: Option<String>,
    pub flow_run_id: Option<String>,
    pub shop: Option<String>,
    pub flow_name: Option<String>,
}

/// A line names its run by its id, as text.
const LOGS_WITH_RUNS: &str = " FROM logs l LEFT JOIN flow_runs r ON r.id::text = l.flow_run_id";

/// The log lines a filter keeps, the latest first.
pub(super) async fn logs(db: &PgPool, filter: &LogFilter, limit: i64) -> Result<Vec<LogLine>> {
    let mut query = Sql::new("SELECT l.created_at, l.level, l.message, l.flow_run_id, r.shop, r.flow_name");
    query.push(LOGS_WITH_RUNS);
    filter.push(&mut query);
    query.push(" ORDER BY l.created_at DESC LIMIT ").push_bind(limit);
    Ok(query.build_query_as().fetch_all(db).await?)
}

/// The log lines a filter keeps, and those of level `error`, per bucket from `start`.
pub(super) async fn log_counts(
    db: &PgPool,
    filter: &LogFilter,
    start: DateTime<Utc>,
    bucket: Duration,
    buckets: usize,
) -> Result<Counts> {
    let mut query = Sql::new("SELECT ");
    push_bucket(&mut query, "l.created_at", start, bucket);
    query.push(", count(*), count(*) FILTER (WHERE l.level = 'error')");
    query.push(LOGS_WITH_RUNS);
    LogFilter {
        since: Some(start),
        before: None,
        ..filter.clone()
    }
    .push(&mut query);
    query.push(" GROUP BY 1");
    Ok(spread(query.build_query_as().fetch_all(db).await?, buckets))
}

/// What the runs of a period add up to.
#[derive(Debug, FromRow)]
pub(super) struct Totals {
    pub runs: i64,
    pub failed: i64,
}

pub(super) async fn totals(db: &PgPool, since: DateTime<Utc>) -> Result<Totals> {
    Ok(sqlx::query_as(
        "SELECT count(*) AS runs, count(*) FILTER (WHERE status = 'failed') AS failed \
         FROM flow_runs WHERE created_at >= $1",
    )
    .bind(since)
    .fetch_one(db)
    .await?)
}

/// What waits for a worker, or is with one.
#[derive(Debug, FromRow)]
pub(super) struct Queue {
    pub waiting: i64,
    pub running: i64,
    /// Since when the run that has waited the longest could start.
    pub oldest_waiting: Option<DateTime<Utc>>,
}

pub(super) async fn queue(db: &PgPool) -> Result<Queue> {
    Ok(sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status IN ('pending', 'sleeping')) AS waiting, \
                count(*) FILTER (WHERE status = 'running') AS running, \
                min(coalesce(available_at, created_at)) FILTER ( \
                  WHERE status = 'pending' AND coalesce(available_at, created_at) <= now() \
                ) AS oldest_waiting \
         FROM flow_runs WHERE status IN ('pending', 'sleeping', 'running')",
    )
    .fetch_one(db)
    .await?)
}

/// The runs of a flow, or of a shop, in a period.
#[derive(Debug, FromRow)]
pub(super) struct Tally {
    pub name: String,
    pub runs: i64,
    pub failed: i64,
    /// The mean time between the start and the end of the runs that ended.
    pub mean_ms: Option<f64>,
    pub latest: Option<DateTime<Utc>>,
}

async fn tallies(db: &PgPool, column: &str, since: DateTime<Utc>, only_failing: bool, limit: i64) -> Result<Vec<Tally>> {
    let failing = if only_failing {
        " HAVING count(*) FILTER (WHERE status = 'failed') > 0"
    } else {
        ""
    };
    Ok(sqlx::query_as(&format!(
        "SELECT {column} AS name, count(*) AS runs, count(*) FILTER (WHERE status = 'failed') AS failed, \
                (avg(extract(epoch FROM (completed_at - started_at))) FILTER ( \
                  WHERE started_at IS NOT NULL AND completed_at IS NOT NULL \
                ) * 1000)::float8 AS mean_ms, \
                max(created_at) AS latest \
         FROM flow_runs WHERE created_at >= $1 GROUP BY 1{failing} \
         ORDER BY failed DESC, runs DESC, name ASC LIMIT $2",
    ))
    .bind(since)
    .bind(limit)
    .fetch_all(db)
    .await?)
}

/// The flows that ran in a period, the ones with the most failures first.
pub(super) async fn flows(db: &PgPool, since: DateTime<Utc>, only_failing: bool, limit: i64) -> Result<Vec<Tally>> {
    tallies(db, "flow_name", since, only_failing, limit).await
}

/// The shops with failures in a period, the ones with the most first.
pub(super) async fn failing_shops(db: &PgPool, since: DateTime<Utc>, limit: i64) -> Result<Vec<Tally>> {
    tallies(db, "shop", since, true, limit).await
}

/// An installation, with what its shop ran in a period.
#[derive(Debug, FromRow)]
pub(super) struct Shop {
    pub app_handle: String,
    pub shop: String,
    pub scopes: Option<String>,
    pub installed_at: DateTime<Utc>,
    pub uninstalled_at: Option<DateTime<Utc>>,
    pub has_token: bool,
    pub runs: i64,
    pub failed: i64,
    pub latest: Option<DateTime<Utc>>,
}

impl Shop {
    /// `installed`, `uninstalled`, or `pending` when the row has no token and
    /// is not uninstalled (an installation started before `/auth` wrote no
    /// row), with the time it has been so.
    pub(super) fn state(&self) -> (&'static str, &DateTime<Utc>) {
        match (&self.uninstalled_at, self.has_token) {
            (Some(uninstalled_at), _) => ("uninstalled", uninstalled_at),
            (None, true) => ("installed", &self.installed_at),
            (None, false) => ("pending", &self.installed_at),
        }
    }
}

pub(super) const SHOP_STATES: [&str; 3] = ["installed", "uninstalled", "pending"];

/// The installations, the shops with the most failures first. `shop` matches
/// a part of the domain, or the whole of it when `exact`.
pub(super) async fn shops(
    db: &PgPool,
    since: DateTime<Utc>,
    shop: Option<&str>,
    exact: bool,
    state: Option<&str>,
    limit: i64,
) -> Result<Vec<Shop>> {
    let mut query = Sql::new(
        "SELECT i.app_handle, i.shop, i.scopes, i.installed_at, i.uninstalled_at, \
                i.access_token IS NOT NULL AS has_token, \
                coalesce(r.runs, 0) AS runs, coalesce(r.failed, 0) AS failed, r.latest \
         FROM installations i \
         LEFT JOIN ( \
           SELECT app_handle, shop, count(*) AS runs, count(*) FILTER (WHERE status = 'failed') AS failed, \
                  max(created_at) AS latest \
           FROM flow_runs WHERE created_at >= ",
    );
    query
        .push_bind(since)
        .push(" GROUP BY app_handle, shop) r ON r.app_handle = i.app_handle AND r.shop = i.shop");
    let mut conditions = Conditions::new();
    if let Some(shop) = shop {
        conditions.next(&mut query);
        if exact {
            query.push("i.shop = ").push_bind(shop.to_string());
        } else {
            query.push("i.shop ILIKE ").push_bind(containing(shop));
        }
    }
    let state = match state {
        Some("installed") => Some("i.access_token IS NOT NULL AND i.uninstalled_at IS NULL"),
        Some("uninstalled") => Some("i.uninstalled_at IS NOT NULL"),
        Some("pending") => Some("i.access_token IS NULL AND i.uninstalled_at IS NULL"),
        _ => None,
    };
    if let Some(state) = state {
        conditions.next(&mut query);
        query.push(state);
    }
    query
        .push(" ORDER BY failed DESC, runs DESC, i.installed_at DESC, i.shop ASC LIMIT ")
        .push_bind(limit);
    Ok(query.build_query_as().fetch_all(db).await?)
}

/// Whether a shop saved the configuration of a provider, and how its last
/// check went. The configuration itself is not read.
#[derive(Debug, FromRow)]
pub(super) struct ProviderState {
    pub app_handle: String,
    pub provider_name: String,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_check_ok: Option<bool>,
    pub updated_at: DateTime<Utc>,
}

pub(super) async fn providers_of(db: &PgPool, shop: &str) -> Result<Vec<ProviderState>> {
    Ok(sqlx::query_as(
        "SELECT app_handle, provider_name, last_checked_at, last_check_ok, updated_at \
         FROM provider_configs WHERE shop = $1 ORDER BY app_handle, provider_name",
    )
    .bind(shop)
    .fetch_all(db)
    .await?)
}

/// The keys of the crons a shop turned off for itself.
pub(super) async fn crons_off_for(db: &PgPool, shop: &str) -> Result<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT cron_key FROM cron_overrides WHERE shop = $1 AND enabled = false ORDER BY cron_key")
            .bind(shop)
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().map(|(key,)| key).collect())
}

/// How many shops turned each cron off, by cron key.
pub(super) async fn crons_off(db: &PgPool) -> Result<Vec<(String, i64)>> {
    Ok(
        sqlx::query_as("SELECT cron_key, count(*) FROM cron_overrides WHERE enabled = false GROUP BY cron_key")
            .fetch_all(db)
            .await?,
    )
}

/// The id of a run a log line names, when it is one.
pub(super) fn run_id(line: &LogLine) -> Option<Uuid> {
    line.flow_run_id.as_deref().and_then(|id| Uuid::parse_str(id).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_range_is_named_by_its_key_and_defaults_to_the_last_day() {
        assert_eq!(Range::parse(Some("1h")), Range::Hour);
        assert_eq!(Range::parse(Some("7d")), Range::Week);
        assert_eq!(Range::parse(Some("30d")), Range::Month);
        assert_eq!(Range::parse(Some("24h")), Range::Day);
        assert_eq!(Range::parse(Some("2y")), Range::Day);
        assert_eq!(Range::parse(None), Range::Day);
    }

    #[test]
    fn a_period_starts_on_a_bucket_and_reaches_now() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 8, 47, 13).unwrap();
        for (range, start, buckets) in [
            (Range::Hour, (2026, 10, 11, 7, 46), 31),
            (Range::Day, (2026, 10, 10, 8, 30), 49),
            (Range::Week, (2026, 10, 4, 6, 0), 57),
            (Range::Month, (2026, 9, 11, 0, 0), 61),
        ] {
            let (year, month, day, hour, minute) = start;
            let expected = Utc.with_ymd_and_hms(year, month, day, hour, minute, 0).unwrap();
            assert_eq!(range.start(now), expected, "{range:?}");
            assert_eq!(range.buckets(now), buckets, "{range:?}");
            // The last bucket holds `now`.
            let last = expected + range.bucket() * (buckets as i32 - 1);
            assert!(last <= now && now < last + range.bucket(), "{range:?}");
        }
    }

    #[test]
    fn a_search_matches_its_text_and_not_a_pattern() {
        assert_eq!(containing("502"), "%502%");
        assert_eq!(containing("100%_done\\"), "%100\\%\\_done\\\\%");
    }

    #[test]
    fn counts_take_their_place_and_missing_buckets_count_zero() {
        let counts = spread(vec![(0, 4, 1), (2, 7, 0), (9, 1, 1), (-1, 1, 1)], 3);
        assert_eq!(counts.total, [4, 0, 7]);
        assert_eq!(counts.bad, [1, 0, 0]);
    }

    #[test]
    fn an_installation_says_its_state() {
        let at = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 10, 5, 0, 0, 0).unwrap();
        let shop = |uninstalled_at, has_token| Shop {
            app_handle: "default".into(),
            shop: "one.myshopify.com".into(),
            scopes: None,
            installed_at: at,
            uninstalled_at,
            has_token,
            runs: 0,
            failed: 0,
            latest: None,
        };
        assert_eq!(shop(None, true).state(), ("installed", &at));
        assert_eq!(shop(None, false).state(), ("pending", &at));
        assert_eq!(shop(Some(later), false).state(), ("uninstalled", &later));
    }
}
