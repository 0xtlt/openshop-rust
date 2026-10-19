//! Cron scheduler: dispatches flows on a schedule.
//!
//! Every `start` process runs a scheduler, and they all compute the same
//! occurrences. The first to claim an occurrence of a cron for a shop in
//! `cron_ticks` queues its run; the others find it taken and skip it. There is
//! no leader, so a process that stops loses nothing the others do not cover.

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Duration, Local, Utc};

use croner::parser::{CronParser, Seconds};
use serde_json::Value;
use tokio::task::JoinHandle;

use super::dispatch::{CronTick, DispatchOptions, dispatch_cron_tick};
use super::errors::FlowConcurrencyError;
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
        // Without a way to store the token (production without
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
            .grant(app, &shopify_app.handle, shop, &Grant::Background, None)
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
    match entry.shops.clone().unwrap_or_default() {
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
const MAX_TICK_LATENESS_MINUTES: i64 = 60;
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
            let identity = serde_json::json!([entry.name, entry.shops_json(), entry.input]).to_string();
            let digest = Sha256::digest(identity.as_bytes());
            let digest = format!("{digest:x}");
            let base = format!("{}#{}", entry.key(), &digest[..16]);
            let rank = seen.iter().filter(|key| **key == base).count();
            seen.push(base.clone());
            if rank == 0 { base } else { format!("{base}#{rank}") }
        })
        .collect()
}

async fn prune_ticks(app: &OpenShop) {
    let result = sqlx::query("DELETE FROM cron_ticks WHERE scheduled_at < $1")
        .bind(Utc::now() - Duration::hours(TICK_RETENTION_HOURS))
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
            report::detached(
                |cron| {
                    cron.unit("cron", flow);
                },
                &error,
            );
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
    let mut missed = Missed::default();
    for CronTarget { shopify_app, shop } in targets {
        let outcome: Result<()> = async {
            // Check if cron is disabled for this shop
            let disabled: Option<(bool,)> = sqlx::query_as(
                "SELECT enabled FROM cron_overrides WHERE app_handle = $1 AND cron_key = $2 AND shop = $3 LIMIT 1",
            )
            .bind(&shopify_app)
            .bind(&cron_key)
            .bind(&shop)
            .fetch_optional(app.db())
            .await?;

            if disabled == Some((false,)) {
                logger::info(format!("[openshop] Cron \"{flow}\" disabled for {shop}, skipping"));
                return Ok(());
            }

            let mut params = DispatchOptions::new(flow.clone(), shop.clone()).shopify_app(shopify_app.clone());
            if let Some(input @ Value::Object(_)) = &entry.input {
                params = params.input(input.clone());
            }
            // `None`: another process queued this occurrence.
            dispatch_cron_tick(app.db(), app.config(), params, &tick).await?;
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

            let cron = parse_schedule(&entry.schedule, flow)?;
            let app = app.clone();
            let job_entry = entry.clone();
            let tick_key = tick_key.clone();
            jobs.push(tokio::spawn(async move {
                let mut after = Local::now();
                loop {
                    let Ok(next) = cron.find_next_occurrence(&after, false) else {
                        return;
                    };
                    let wait = (next - Local::now()).to_std().unwrap_or_default();
                    tokio::time::sleep(wait).await;
                    after = next;

                    // Whole seconds: every process must name the occurrence alike.
                    let Some(scheduled_at) = DateTime::from_timestamp(next.timestamp(), 0) else {
                        continue;
                    };
                    let app = app.clone();
                    let entry = job_entry.clone();
                    let tick_key = tick_key.clone();
                    tokio::spawn(async move { run_entry(&app, &entry, &tick_key, scheduled_at).await });
                }
            }));

            let mode = entry.shops_json();
            let mode = mode.as_str().map(str::to_string).unwrap_or_else(|| mode.to_string());
            let label = entry.name.as_deref().unwrap_or(flow);
            logger::info(format!(
                "[openshop] Cron registered: \"{label}\" → {} ({mode})",
                entry.schedule
            ));
        }

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

    #[tokio::test]
    async fn an_occurrence_too_late_is_not_fired() {
        let flow = Flow::new("sync", |_ctx: crate::FlowContext| async move { crate::Result::Ok(()) })
            .concurrency(Concurrency::Allow);
        let cron = Cron::new("* * * * * *", "sync");
        let config = OpenShopConfig::builder().flow(flow).cron(cron.clone()).build().unwrap();
        let db = TestDb::create().await.unwrap();
        let env = RuntimeEnv {
            shopify_api_key: Some("key".into()),
            shopify_api_secret: Some("secret".into()),
            cwd: std::env::temp_dir().join("openshop-test-no-toml"),
            ..Default::default()
        };
        let app = OpenShop::with_transport(config, db.pool.clone(), env, MockTransport::new()).unwrap();
        let count = || async {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM flow_runs")
                .fetch_one(&db.pool)
                .await
                .unwrap()
        };

        // A process paused for two days wakes up on occurrences the others fired
        // long ago, whose claims are gone: it must not queue them again.
        let old = Utc::now() - Duration::hours(48);
        run_entry(&app, &cron, "sync", DateTime::from_timestamp(old.timestamp(), 0).unwrap()).await;
        assert_eq!(count().await, 0);

        let now = DateTime::from_timestamp(Utc::now().timestamp(), 0).unwrap();
        run_entry(&app, &cron, "sync", now).await;
        assert_eq!(count().await, 1);
        db.cleanup().await.unwrap();
    }
}
