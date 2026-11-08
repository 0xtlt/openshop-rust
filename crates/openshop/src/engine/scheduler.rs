//! Cron scheduler: dispatches flows on a schedule.
//!
//! Every `start` process runs a scheduler, and they all compute the same
//! occurrences. The first to claim an occurrence of a cron for a shop in
//! `cron_ticks` queues its run; the others find it taken and skip it. There is
//! no leader, so a process that stops loses nothing the others do not cover.
//!
//! A schedule is read in UTC, or in the time zone of the cron. A process that
//! was stopped or suspended does not catch up: the occurrences it slept through
//! are one firing, for the latest of them.

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::time::Instant;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use croner::parser::{CronParser, Seconds};
use futures::FutureExt;
use serde_json::Value;
use tokio::task::JoinHandle;

use super::dispatch::{CronTick, DispatchOptions, queue_run};
use super::errors::FlowConcurrencyError;
use super::runner::panic_message;
use super::step::error_message;
use crate::app::OpenShop;
use crate::runtime::{logger, report};
use crate::server::shopify_apps::DEFAULT_SHOPIFY_APP_HANDLE;
use crate::shopify::client_credentials::Grant;
use crate::types::{Cron, CronShops};

#[derive(Debug, Clone, PartialEq, Eq)]
struct CronTarget {
    shopify_app: String,
    shop: String,
}

fn default_target(shop: &str) -> CronTarget {
    CronTarget {
        shopify_app: DEFAULT_SHOPIFY_APP_HANDLE.to_string(),
        shop: shop.to_string(),
    }
}

async fn resolve_installed_targets(app: &OpenShop, shops: &[String]) -> Result<Vec<CronTarget>> {
    if shops.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT app_handle, shop FROM installations WHERE uninstalled_at IS NULL AND shop = ANY($1)")
            .bind(shops)
            .fetch_all(app.db())
            .await?;

    // A shop without an installation still runs under the default app handle.
    let mut targets = Vec::new();
    for shop in shops {
        let matches: Vec<CronTarget> = rows
            .iter()
            .filter(|(_, row_shop)| row_shop == shop)
            .map(|(shopify_app, shop)| CronTarget {
                shopify_app: shopify_app.clone(),
                shop: shop.clone(),
            })
            .collect();
        if matches.is_empty() {
            targets.push(default_target(shop));
        } else {
            targets.extend(matches);
        }
    }
    Ok(targets)
}

/// The shop of the `client_credentials_shop` setting, for each Shopify app that
/// has no installation row for it, such as after the database was emptied:
/// targeted once Shopify gives the app a token for it, which stores the row.
/// A row marked uninstalled is left as it is, and the shop untargeted.
async fn configured_targets(app: &OpenShop) -> Result<Vec<CronTarget>> {
    let credentials = app.client_credentials();
    let Some(shop) = credentials.configured_shop() else {
        return Ok(Vec::new());
    };
    let known: Vec<(String,)> = sqlx::query_as("SELECT app_handle FROM installations WHERE shop = $1")
        .bind(shop)
        .fetch_all(app.db())
        .await?;

    let mut targets = Vec::new();
    for shopify_app in app.apps().all() {
        if known.iter().any(|(handle,)| *handle == shopify_app.handle) {
            continue;
        }
        // Without a way to store the token (a malformed
        // `ENCRYPTION_KEY`), nothing is asked: said once per app, not at
        // every tick, and never remembered as a refusal.
        if let Err(error) = app.crypto().can_encrypt() {
            if credentials.first_report(&shopify_app.handle) {
                logger::warn(format!(
                    "[openshop] Cannot store a client credentials token of app \"{}\" for {shop}: {error}",
                    shopify_app.handle
                ));
            }
            continue;
        }
        // The shops of the other rows are served whatever happens here.
        let granted = match credentials
            .grant(app, &shopify_app.handle, shop, &Grant::Background)
            .await
        {
            Ok(granted) => granted,
            Err(error) => {
                logger::warn(format!("{error:#}"));
                None
            }
        };
        if granted.is_some() {
            targets.push(CronTarget {
                shopify_app: shopify_app.handle.clone(),
                shop: shop.to_string(),
            });
        }
    }
    Ok(targets)
}

async fn resolve_targets(app: &OpenShop, entry: &Cron) -> Result<Vec<CronTarget>> {
    match entry.shops.clone() {
        CronShops::Global => Ok(vec![default_target("__global__")]),
        CronShops::All => {
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT app_handle, shop FROM installations WHERE uninstalled_at IS NULL")
                    .fetch_all(app.db())
                    .await?;
            let mut targets: Vec<CronTarget> = rows
                .into_iter()
                .map(|(shopify_app, shop)| CronTarget { shopify_app, shop })
                .collect();
            targets.extend(configured_targets(app).await?);
            Ok(targets)
        }
        CronShops::Shop(shop) => resolve_installed_targets(app, &[shop]).await,
        CronShops::Shops(shops) => resolve_installed_targets(app, &shops).await,
    }
}

/// Parses a five-field expression, a six-field one with seconds, or a nickname such as `@daily`.
pub fn parse_schedule(schedule: &str, flow: &str) -> Result<croner::Cron> {
    let trimmed = schedule.trim();
    let fields = trimmed.split_whitespace().count();
    if !trimmed.starts_with('@') && fields != 5 && fields != 6 {
        bail!(
            "[openshop] Invalid cron schedule \"{schedule}\" for flow \"{flow}\": expected 5 or 6 fields, got {fields}"
        );
    }
    CronParser::builder()
        .seconds(Seconds::Optional)
        .build()
        .parse(trimmed)
        .map_err(|error| anyhow!("[openshop] Invalid cron schedule \"{schedule}\" for flow \"{flow}\": {error}"))
}

/// The time zone a cron is read in: the one it declares, else UTC.
pub fn parse_timezone(cron: &Cron) -> Result<Tz> {
    let Some(name) = &cron.timezone else {
        return Ok(Tz::UTC);
    };
    name.trim().parse::<Tz>().map_err(|_| {
        anyhow!(
            "[openshop] Invalid time zone \"{name}\" for the cron of flow \"{}\": expected an IANA name such as \"Europe/Paris\"",
            cron.flow
        )
    })
}

/// A schedule and the time zone it is read in.
#[derive(Clone)]
struct Schedule {
    cron: croner::Cron,
    zone: Tz,
}

impl Schedule {
    fn of(entry: &Cron) -> Result<Self> {
        Ok(Self {
            cron: parse_schedule(&entry.schedule, &entry.flow)?,
            zone: parse_timezone(entry)?,
        })
    }

    /// The first occurrence after an instant.
    fn after(&self, instant: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let next = self.cron.find_next_occurrence(&instant.with_timezone(&self.zone), false);
        next.ok().map(|next| next.with_timezone(&Utc))
    }

    /// The last occurrence up to an instant.
    fn until(&self, instant: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let previous = self
            .cron
            .find_previous_occurrence(&instant.with_timezone(&self.zone), true);
        previous.ok().map(|previous| previous.with_timezone(&Utc))
    }

    /// The occurrence to fire when the wait for `next` ends at `now`: `next`
    /// itself, or when the wait ran late (a suspended host, a clock set
    /// forward) the latest occurrence since, so that what was missed is one
    /// firing and not one per occurrence. `None` when `now` is before `next`.
    /// How late an occurrence may still be fired is decided once, by `run_entry`.
    fn due(&self, next: DateTime<Utc>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if now < next {
            return None;
        }
        Some(self.until(now).filter(|latest| *latest > next).unwrap_or(next))
    }
}

/// When a cron fires next after an instant, in the time zone it declares.
/// `None` when its schedule or its time zone is invalid, or has no more occurrence.
pub fn next_occurrence(cron: &Cron, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    Schedule::of(cron).ok()?.after(after)
}

/// The shops a firing of a cron queued no run for, and why the first one.
#[derive(Default)]
struct Missed {
    shops: usize,
    first: Option<(CronTarget, anyhow::Error)>,
}

impl Missed {
    fn add(&mut self, target: CronTarget, error: anyhow::Error) {
        self.shops += 1;
        self.first.get_or_insert((target, error));
    }

    /// Reports the firing once, whatever the number of shops: no run exists
    /// to say that it is missing, and a cron of every shop that fails for all
    /// of them would otherwise crowd out everything else there is to report.
    fn report(self, flow: &str) {
        let Some((target, error)) = self.first else { return };
        report::detached(
            |cron| {
                cron.unit("cron", flow).tag("shops_missed", self.shops);
                // The shop is the user of an event: only when it is the one concerned.
                if self.shops == 1 {
                    cron.shop(&target.shop, &target.shopify_app);
                }
            },
            &error,
        );
    }
}

/// How long the claims of past occurrences are kept. They only have to outlive
/// the gap between the processes that fire the same occurrence.
const TICK_RETENTION_HOURS: i64 = 24;

/// An occurrence this late is not fired: another process fired it long ago,
/// and its claim may be pruned any time now. Must stay well under the retention.
pub(super) const MAX_TICK_LATENESS_MINUTES: i64 = 60;
const _: () = assert!(MAX_TICK_LATENESS_MINUTES < TICK_RETENTION_HOURS * 60);

/// What names a configured cron in `cron_ticks`: its flow and schedule, as in
/// the override key, plus a digest of what else sets it apart (name, shops,
/// input), and a rank among crons that are identical. Two crons of one flow and
/// schedule then claim their own occurrences, and every process of the same
/// configuration agrees on the name, whatever the order of the crons.
fn tick_keys(crons: &[Cron]) -> Vec<String> {
    use sha2::{Digest, Sha256};
    let mut seen: Vec<String> = Vec::new();
    crons
        .iter()
        .map(|entry| {
            // The effective time zone: "UTC" and no time zone are the same cron.
            let zone = parse_timezone(entry).map_or_else(|_| entry.timezone.clone().unwrap_or_default(), |zone| zone.name().to_string());
            let identity = serde_json::json!([entry.name, entry.shops_json(), entry.input, zone]).to_string();
            let digest = Sha256::digest(identity.as_bytes());
            let digest = hex::encode(digest);
            let base = format!("{}#{}", entry.key(), &digest[..16]);
            let rank = seen.iter().filter(|key| **key == base).count();
            seen.push(base.clone());
            if rank == 0 { base } else { format!("{base}#{rank}") }
        })
        .collect()
}

async fn prune_ticks(app: &OpenShop) {
    // The clock of the database, as the claim that reads these rows.
    let result = sqlx::query("DELETE FROM cron_ticks WHERE scheduled_at < clock_timestamp() - make_interval(hours => $1)")
        .bind(TICK_RETENTION_HOURS as i32)
        .execute(app.db())
        .await;
    if let Err(error) = result {
        logger::warn(format!(
            "[openshop] Could not remove old cron ticks: {}",
            error_message(&error.into())
        ));
    }
}

/// Fires an occurrence of a cron. `scheduled_at` is the same instant in every
/// process, and is what makes the run be queued once.
async fn run_entry(app: &OpenShop, entry: &Cron, tick_key: &str, scheduled_at: DateTime<Utc>) {
    let flow = &entry.flow;
    if Utc::now() - scheduled_at > Duration::minutes(MAX_TICK_LATENESS_MINUTES) {
        logger::warn(format!(
            "[openshop] Cron \"{flow}\": skipping the occurrence of {scheduled_at}, too late to fire"
        ));
        return;
    }
    let targets = match resolve_targets(app, entry).await {
        Ok(targets) => targets,
        Err(error) => {
            logger::warn(format!(
                "[openshop] Cron \"{flow}\" could not resolve its shops: {}",
                error_message(&error)
            ));
            report_cron(flow, &error);
            return;
        }
    };

    if targets.is_empty() {
        logger::info(format!("[openshop] Cron \"{flow}\": no shops to run for, skipping"));
        return;
    }
    logger::info(format!(
        "[openshop] Cron triggered: {flow} → {} target(s)",
        targets.len()
    ));

    let cron_key = entry.key();
    prune_ticks(app).await;
    let tick = CronTick {
        cron_key: tick_key,
        scheduled_at,
    };
    // The shops that turned the cron off, in one query for all the targets.
    let turned_off: sqlx::Result<Vec<(String, String)>> =
        sqlx::query_as("SELECT app_handle, shop FROM cron_overrides WHERE cron_key = $1 AND enabled = false")
            .bind(&cron_key)
            .fetch_all(app.db())
            .await;
    let turned_off: HashSet<(String, String)> = match turned_off {
        Ok(rows) => rows.into_iter().collect(),
        Err(error) => {
            let error = anyhow::Error::from(error);
            logger::warn(format!(
                "[openshop] Cron \"{flow}\" could not read its overrides: {}",
                error_message(&error)
            ));
            report_cron(flow, &error);
            return;
        }
    };

    let mut missed = Missed::default();
    for CronTarget { shopify_app, shop } in targets {
        let outcome: Result<()> = async {
            if turned_off.contains(&(shopify_app.clone(), shop.clone())) {
                logger::info(format!("[openshop] Cron \"{flow}\" disabled for {shop}, skipping"));
                return Ok(());
            }

            let mut params = DispatchOptions::new(flow.clone(), shop.clone()).shopify_app(shopify_app.clone());
            if let Some(input @ Value::Object(_)) = &entry.input {
                params = params.input(input.clone());
            }
            // `None`: another process queued this occurrence.
            queue_run(app.db(), app.config(), params, Some(&tick)).await?;
            Ok(())
        }
        .await;

        if let Err(error) = outcome {
            logger::error_with(
                format!("[openshop] Cron flow \"{flow}\" failed for {shop}"),
                serde_json::json!({ "error": error_message(&error) }),
            );
            // A flow that refuses a second run while one is active does what it declares.
            if !error.is::<FlowConcurrencyError>() {
                missed.add(CronTarget { shopify_app, shop }, error);
            }
        }
    }
    missed.report(flow);
}

/// The wait before a cron that stopped is started again, doubled each time it
/// stops again soon after, up to the longest.
const RESTART_AFTER: std::time::Duration = std::time::Duration::from_secs(1);
const RESTART_AT_MOST: std::time::Duration = std::time::Duration::from_secs(300);

/// Fires a cron on its schedule, and starts it again when it stops: it never
/// ends silently, which for a cron means that nothing runs and nobody knows.
async fn keep_scheduled(app: OpenShop, entry: Cron, tick_key: String, schedule: Schedule) {
    let label = entry.name.clone().unwrap_or_else(|| entry.flow.clone());
    let mut wait = RESTART_AFTER;
    loop {
        let started = Instant::now();
        let ended = AssertUnwindSafe(fire_on_schedule(&app, &entry, &tick_key, &schedule))
            .catch_unwind()
            .await;
        let reason = match ended {
            Ok(()) => "its schedule has no more occurrence".to_string(),
            Err(panic) => format!("it panicked: {}", panic_message(&panic)),
        };
        // A cron that ran for a while before it stopped is not one that keeps stopping.
        if started.elapsed() > RESTART_AT_MOST {
            wait = RESTART_AFTER;
        }
        let message = format!(
            "[openshop] Cron \"{label}\" stopped, {reason}. Starting it again in {}s.",
            wait.as_secs()
        );
        logger::error(&message);
        if wait == RESTART_AFTER {
            report_cron(&entry.flow, &anyhow!(message));
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(RESTART_AT_MOST);
    }
}

/// Reports a failure of a cron, apart from any of its runs.
fn report_cron(flow: &str, error: &anyhow::Error) {
    report::detached(
        |cron| {
            cron.unit("cron", flow);
        },
        error,
    );
}

/// Waits for each occurrence of a schedule and fires it. Returns when the
/// schedule has none left.
async fn fire_on_schedule(app: &OpenShop, entry: &Cron, tick_key: &str, schedule: &Schedule) {
    let mut after = Utc::now();
    while let Some(next) = schedule.after(after) {
        tokio::time::sleep((next - Utc::now()).to_std().unwrap_or_default()).await;
        let now = Utc::now();
        // Woken before the time, such as by a clock set back: wait again.
        let Some(due) = schedule.due(next, now) else { continue };
        after = now.max(due);

        // Whole seconds: every process must name the occurrence alike.
        let Some(scheduled_at) = DateTime::from_timestamp(due.timestamp(), 0) else {
            continue;
        };
        let (app, entry, tick_key) = (app.clone(), entry.clone(), tick_key.to_string());
        tokio::spawn(async move { run_entry(&app, &entry, &tick_key, scheduled_at).await });
    }
}

/// The flows that unfinished runs wait for and `is_registered` does not know.
async fn unregistered_flows(db: &sqlx::PgPool, is_registered: impl Fn(&str) -> bool) -> sqlx::Result<Vec<String>> {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT flow_name FROM flow_runs WHERE status IN ('pending', 'sleeping', 'running') ORDER BY flow_name",
    )
    .fetch_all(db)
    .await?;
    Ok(names.into_iter().filter(|name| !is_registered(name)).collect())
}

/// Says which waiting runs name a flow the application does not register: a
/// worker fails them as soon as it claims them. It happens when a flow is renamed, or comes from the TypeScript
/// version under another name.
async fn warn_about_unregistered_flows(app: &OpenShop) {
    let flows = &app.config().flows;
    match unregistered_flows(app.db(), |name| flows.contains_key(name)).await {
        Ok(unknown) if unknown.is_empty() => {}
        Ok(unknown) => logger::warn(format!(
            "[openshop] Runs are waiting for flows this application does not register: {}. \
             A worker fails them when it claims them. If a flow was renamed, give it its previous name with `#[flow(\"…\")]`.",
            unknown.iter().map(|name| format!("\"{name}\"")).collect::<Vec<_>>().join(", ")
        )),
        Err(error) => logger::warn(format!(
            "[openshop] Could not check the flows of the waiting runs: {error}"
        )),
    }
}

/// The crons disabled for a shop whose key no cron in `keys` has.
async fn orphaned_overrides(db: &sqlx::PgPool, keys: &[String]) -> sqlx::Result<Vec<String>> {
    let disabled: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT cron_key FROM cron_overrides WHERE NOT enabled ORDER BY cron_key")
            .fetch_all(db)
            .await?;
    Ok(disabled.into_iter().filter(|key| !keys.contains(key)).collect())
}

/// Says which crons a shop disabled no longer match a cron of the
/// application. A cron is known by its flow and its schedule: when either
/// changes, as when a cron is ported from the TypeScript version with another
/// spelling, the shops that disabled it run it again.
async fn warn_about_orphaned_overrides(app: &OpenShop) {
    let keys: Vec<String> = app.config().crons.iter().map(Cron::key).collect();
    match orphaned_overrides(app.db(), &keys).await {
        Ok(orphaned) if orphaned.is_empty() => {}
        Ok(orphaned) => logger::warn(format!(
            "[openshop] Some shops disabled crons that no longer exist: {}. \
             A cron is known by `flow:schedule`: if one of them was changed, the shops that disabled it run it again. \
             Write its schedule as it was to keep them disabled.",
            orphaned.iter().map(|key| format!("\"{key}\"")).collect::<Vec<_>>().join(", ")
        )),
        Err(error) => logger::warn(format!("[openshop] Could not check the disabled crons: {error}")),
    }
}

/// Running cron jobs. Dropping the scheduler stops them.
pub struct Scheduler {
    jobs: Vec<JoinHandle<()>>,
}

impl Scheduler {
    /// Registers every configured cron. Fails on an invalid schedule.
    pub fn start(app: &OpenShop) -> Result<Self> {
        let mut jobs = Vec::new();
        let keys = tick_keys(&app.config().crons);

        for (entry, tick_key) in app.config().crons.iter().zip(keys) {
            let flow = &entry.flow;
            if !app.config().flows.contains_key(flow) {
                logger::warn(format!("[openshop] Cron references unknown flow \"{flow}\", skipping"));
                continue;
            }

            let schedule = Schedule::of(entry)?;
            let app = app.clone();
            let job_entry = entry.clone();
            let tick_key = tick_key.clone();
            jobs.push(tokio::spawn(async move {
                keep_scheduled(app, job_entry, tick_key, schedule).await
            }));

            let mode = entry.shops_json();
            let mode = mode.as_str().map(str::to_string).unwrap_or_else(|| mode.to_string());
            let label = entry.name.as_deref().unwrap_or(flow);
            logger::info(format!(
                "[openshop] Cron registered: \"{label}\" → {} ({mode})",
                entry.schedule
            ));
        }

        let check = app.clone();
        // What an earlier version, or a rename, left in the database. Not one
        // of the jobs: it ends once it has looked.
        tokio::spawn(async move {
            warn_about_unregistered_flows(&check).await;
            warn_about_orphaned_overrides(&check).await;
        });

        Ok(Self { jobs })
    }

    pub fn job_count(&self) -> usize {
        self.jobs.len()
    }

    pub fn stop(&mut self) {
        for job in self.jobs.drain(..) {
            job.abort();
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::Flow;
    use crate::runtime::RuntimeEnv;
    use crate::testing::{MockTransport, TestDb};
    use crate::{Concurrency, OpenShopConfig};
    use serde_json::json;

    #[tokio::test]
    async fn names_the_flows_of_waiting_runs_that_are_not_registered() {
        let db = TestDb::create().await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO flow_runs (shop, flow_name, status) VALUES \
               ('s', 'syncOrders', 'pending'), ('s', 'sync-orders', 'pending'), ('s', 'oldSleep', 'sleeping'), \
               ('s', 'oldRunning', 'running'), ('s', 'oldDone', 'completed'), ('s', 'oldFailed', 'failed')",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        let unknown = unregistered_flows(&db.pool, |name| name == "syncOrders").await.unwrap();

        // Finished runs are history: only those a worker would claim count.
        assert_eq!(unknown, ["oldRunning", "oldSleep", "sync-orders"]);
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn names_the_disabled_crons_that_no_longer_exist() {
        let db = TestDb::create().await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO cron_overrides (shop, cron_key, enabled) VALUES \
               ('a', 'sync:*/5 * * * *', false), ('b', 'sync:0 * * * *', false), ('a', 'gone:0 * * * *', true)",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        let keys = [Cron::new("*/5 * * * *", "sync").key()];
        // An enabled override changes nothing, so it is not worth a warning.
        assert_eq!(orphaned_overrides(&db.pool, &keys).await.unwrap(), ["sync:0 * * * *"]);
        db.cleanup().await.unwrap();
    }

    #[test]
    fn accepts_five_and_six_field_schedules_and_nicknames() {
        assert!(parse_schedule("*/5 * * * *", "sync").is_ok());
        assert!(parse_schedule("* * * * * *", "sync").is_ok());
        assert!(parse_schedule("@daily", "sync").is_ok());
        assert!(parse_schedule("  0 3 * * *  ", "sync").is_ok());
    }

    #[test]
    fn rejects_schedules_with_the_wrong_number_of_fields() {
        let error = parse_schedule("* * *", "sync").unwrap_err().to_string();
        assert_eq!(
            error,
            "[openshop] Invalid cron schedule \"* * *\" for flow \"sync\": expected 5 or 6 fields, got 3"
        );
        assert!(parse_schedule("* * * * * * *", "sync").is_err());
    }

    #[test]
    fn rejects_out_of_range_values() {
        assert!(parse_schedule("60 3 * * *", "sync").is_err());
        assert!(parse_schedule("0 3 * e* *", "sync").is_err());
    }

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn schedule(expression: &str, timezone: Option<&str>) -> Schedule {
        let mut cron = Cron::new(expression, "sync");
        cron.timezone = timezone.map(str::to_string);
        Schedule::of(&cron).unwrap()
    }

    #[test]
    fn a_wait_that_ends_late_fires_the_latest_occurrence_once() {
        let every_minute = schedule("* * * * *", None);
        let next = utc("2026-01-01T10:01:00Z");

        // On time, or a little late: the occurrence that was waited for.
        assert_eq!(every_minute.due(next, utc("2026-01-01T10:01:00Z")), Some(next));
        assert_eq!(every_minute.due(next, utc("2026-01-01T10:01:30Z")), Some(next));
        // An hour of suspension: the latest occurrence, not 60 of them.
        assert_eq!(
            every_minute.due(next, utc("2026-01-01T11:00:20Z")),
            Some(utc("2026-01-01T11:00:00Z"))
        );
        // Woken too soon: not yet.
        assert_eq!(every_minute.due(next, utc("2026-01-01T10:00:59Z")), None);
    }

    #[test]
    fn a_schedule_is_read_in_utc_unless_the_cron_has_a_time_zone() {
        let after = utc("2026-01-15T00:00:00Z");
        assert_eq!(
            schedule("0 9 * * *", None).after(after),
            Some(utc("2026-01-15T09:00:00Z"))
        );
        // Paris is an hour ahead of UTC in winter, two in summer.
        let paris = schedule("0 9 * * *", Some("Europe/Paris"));
        assert_eq!(paris.after(after), Some(utc("2026-01-15T08:00:00Z")));
        assert_eq!(paris.after(utc("2026-07-15T00:00:00Z")), Some(utc("2026-07-15T07:00:00Z")));
    }

    #[test]
    fn an_unknown_time_zone_is_refused() {
        let mut cron = Cron::new("0 9 * * *", "sync");
        cron.timezone = Some("Mars/Olympus".to_string());
        assert_eq!(
            parse_timezone(&cron).unwrap_err().to_string(),
            "[openshop] Invalid time zone \"Mars/Olympus\" for the cron of flow \"sync\": expected an IANA name such as \"Europe/Paris\""
        );
    }

    #[test]
    fn crons_of_one_flow_and_schedule_get_their_own_tick_keys() {
        let plain = Cron::new("0 3 * * *", "sync");
        let full = Cron::new("0 3 * * *", "sync").input(json!({ "mode": "full" }));
        let keys = tick_keys(&[plain.clone(), full.clone(), plain.clone()]);

        assert_eq!(keys.iter().collect::<std::collections::BTreeSet<_>>().len(), 3);
        assert!(keys.iter().all(|key| key.starts_with("sync:0 3 * * *#")));
        // The same configuration, in another order, names its crons alike.
        let reordered = tick_keys(&[full, plain.clone(), plain]);
        assert_eq!(reordered[0], keys[1]);
        assert_eq!([&reordered[1], &reordered[2]], [&keys[0], &keys[2]]);
    }

    #[test]
    fn crons_that_differ_by_time_zone_get_their_own_tick_keys() {
        let utc = Cron::new("0 9 * * *", "sync");
        let paris = Cron::new("0 9 * * *", "sync").timezone("Europe/Paris");
        let explicit = Cron::new("0 9 * * *", "sync").timezone("UTC");

        let keys = tick_keys(&[utc.clone(), paris.clone()]);
        assert_ne!(keys[0], keys[1]);
        // Whatever the order, each keeps its key: no rank is shared between zones.
        let reordered = tick_keys(&[paris, utc.clone()]);
        assert_eq!([&reordered[1], &reordered[0]], [&keys[0], &keys[1]]);
        assert!(!reordered.iter().any(|key| key.ends_with("#1")));
        // Saying UTC is the default.
        assert_eq!(tick_keys(&[explicit])[0], tick_keys(&[utc])[0]);
    }

    async fn app_with_a_cron(db: &TestDb) -> (OpenShop, Cron) {
        let flow = Flow::new("sync", |_ctx: crate::FlowContext| async move { crate::Result::Ok(()) })
            .concurrency(Concurrency::Allow);
        let cron = Cron::new("* * * * * *", "sync");
        let config = OpenShopConfig::builder().flow(flow).cron(cron.clone()).build().unwrap();
        let env = RuntimeEnv {
            shopify_api_key: Some("key".into()),
            shopify_api_secret: Some("secret".into()),
            development: true,
            cwd: std::env::temp_dir().join("openshop-test-no-toml"),
            ..Default::default()
        };
        let app = OpenShop::with_transport(config, db.pool.clone(), env, MockTransport::new()).unwrap();
        (app, cron)
    }

    async fn run_count(db: &TestDb) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM flow_runs")
            .fetch_one(&db.pool)
            .await
            .unwrap()
    }

    fn whole_seconds_ago(age: Duration) -> DateTime<Utc> {
        DateTime::from_timestamp((Utc::now() - age).timestamp(), 0).unwrap()
    }

    #[tokio::test]
    async fn an_occurrence_too_late_is_not_fired() {
        let db = TestDb::create().await.unwrap();
        let (app, cron) = app_with_a_cron(&db).await;

        // A process paused for two days wakes up on occurrences the others fired
        // long ago, whose claims are gone: it must not queue them again.
        run_entry(&app, &cron, "sync", whole_seconds_ago(Duration::hours(48))).await;
        assert_eq!(run_count(&db).await, 0);

        run_entry(&app, &cron, "sync", whole_seconds_ago(Duration::zero())).await;
        assert_eq!(run_count(&db).await, 1);
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn the_claim_itself_refuses_an_occurrence_that_became_too_late() {
        let db = TestDb::create().await.unwrap();
        let (app, _) = app_with_a_cron(&db).await;
        let claim = |scheduled_at| {
            let app = app.clone();
            async move {
                let tick = CronTick {
                    cron_key: "sync",
                    scheduled_at,
                };
                let options = DispatchOptions::new("sync", "__global__");
                queue_run(app.db(), app.config(), options, Some(&tick)).await.unwrap()
            }
        };

        // A process that passed the check of `run_entry`, then stalled past the
        // retention while the claim of the occurrence was pruned: it has no
        // claim to find, and must not make a new one for a run already queued.
        let stale = whole_seconds_ago(Duration::hours(TICK_RETENTION_HOURS + 1));
        assert!(claim(stale).await.is_none());
        let late = whole_seconds_ago(Duration::minutes(MAX_TICK_LATENESS_MINUTES + 1));
        assert!(claim(late).await.is_none());
        assert_eq!(run_count(&db).await, 0);

        // A fresh occurrence is claimed once.
        let fresh = whole_seconds_ago(Duration::seconds(5));
        assert!(claim(fresh).await.is_some());
        assert!(claim(fresh).await.is_none());
        assert_eq!(run_count(&db).await, 1);
        db.cleanup().await.unwrap();
    }
}
