//! Cron scheduler: dispatches flows on a schedule.
//!
//! There is no leader election. Run one scheduler unless duplicate cron
//! attempts are acceptable.

use anyhow::{Result, anyhow, bail};
use chrono::Local;

use croner::parser::{CronParser, Seconds};
use serde_json::Value;
use tokio::task::JoinHandle;

use super::dispatch::DispatchOptions;
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

/// The shop of `SHOPIFY_CLIENT_CREDENTIALS_SHOP`, for each Shopify app that
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
        let granted = credentials
            .grant(app, &shopify_app.handle, shop, Grant::Configured, None)
            .await?;
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

async fn run_entry(app: &OpenShop, entry: &Cron) {
    let flow = &entry.flow;
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
            app.dispatch_flow(params).await?;
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

        for entry in &app.config().crons {
            let flow = &entry.flow;
            if !app.config().flows.contains_key(flow) {
                logger::warn(format!("[openshop] Cron references unknown flow \"{flow}\", skipping"));
                continue;
            }

            let cron = parse_schedule(&entry.schedule, flow)?;
            let app = app.clone();
            let job_entry = entry.clone();
            jobs.push(tokio::spawn(async move {
                let mut after = Local::now();
                loop {
                    let Ok(next) = cron.find_next_occurrence(&after, false) else {
                        return;
                    };
                    let wait = (next - Local::now()).to_std().unwrap_or_default();
                    tokio::time::sleep(wait).await;
                    after = next;

                    let app = app.clone();
                    let entry = job_entry.clone();
                    tokio::spawn(async move { run_entry(&app, &entry).await });
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
}
