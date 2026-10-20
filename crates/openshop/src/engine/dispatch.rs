//! Queueing a flow run.

use anyhow::{Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::backoff::resolve_retry_policy;
use super::errors::{FlowConcurrencyError, InvalidFlowInput, UnknownFlowError};
use crate::app::OpenShopConfig;
use crate::runtime::time::millis;
use crate::server::shopify_apps::DEFAULT_SHOPIFY_APP_HANDLE;
use crate::types::{Concurrency, JsonMap, RetryPolicy, ShopScope};

/// A flow run to queue: the flow, the shop it runs for, and how.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DispatchOptions {
    pub flow_name: String,
    /// Defaults to `{}`.
    pub input: Option<Value>,
    pub shop: String,
    /// Defaults to the single-app handle `default`.
    pub shopify_app: Option<String>,
    pub parent_run_id: Option<String>,
    /// Delay before the run becomes available to workers.
    pub delay: Option<std::time::Duration>,
    pub retry_policy: Option<RetryPolicy>,
}

impl DispatchOptions {
    pub fn new(flow_name: impl Into<String>, shop: impl Into<String>) -> Self {
        Self {
            flow_name: flow_name.into(),
            shop: shop.into(),
            ..Default::default()
        }
    }

    /// A run for the shop and the Shopify app of a context: a flow, a
    /// webhook, a proxy request…
    pub fn for_scope(flow_name: impl Into<String>, scope: &impl ShopScope) -> Self {
        Self::new(flow_name, scope.shop()).shopify_app(scope.shopify_app())
    }

    pub fn input(mut self, input: Value) -> Self {
        self.input = Some(input);
        self
    }

    pub fn shopify_app(mut self, handle: impl Into<String>) -> Self {
        self.shopify_app = Some(handle.into());
        self
    }

    pub fn parent_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.parent_run_id = Some(run_id.into());
        self
    }

    /// Makes the run wait before workers can take it.
    pub fn delay(mut self, delay: std::time::Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    /// Overrides the retry policy of the flow for this run.
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedRun {
    pub run_id: Uuid,
    pub status: &'static str,
}

pub(crate) fn unknown_flow(config: &OpenShopConfig, flow_name: &str) -> anyhow::Error {
    anyhow::Error::new(UnknownFlowError {
        flow_name: flow_name.to_string(),
        available: config.flows.keys().cloned().collect(),
    })
}

/// Takes, until the end of the transaction, the lock that serializes the runs
/// of a flow for a shop: whoever makes a run active takes it first.
pub(crate) async fn lock_flow(tx: &mut PgConnection, shopify_app: &str, shop: &str, flow_name: &str) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("{shopify_app}:{shop}:{flow_name}"))
        .execute(tx)
        .await?;
    Ok(())
}

/// Fails with a [`FlowConcurrencyError`] when another run of the flow is active
/// for the shop. Meant to be called under [`lock_flow`], for a flow that rejects
/// concurrent runs; `except` is the run that is about to become active.
pub(crate) async fn reject_when_active(
    tx: &mut PgConnection,
    shopify_app: &str,
    shop: &str,
    flow_name: &str,
    except: Option<Uuid>,
) -> Result<()> {
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM flow_runs \
         WHERE app_handle = $1 AND shop = $2 AND flow_name = $3 \
           AND status IN ('pending', 'running', 'sleeping') \
           AND ($4::uuid IS NULL OR id <> $4) \
         LIMIT 1",
    )
    .bind(shopify_app)
    .bind(shop)
    .bind(flow_name)
    .bind(except)
    .fetch_optional(tx)
    .await?;

    if let Some((existing_run_id,)) = existing {
        bail!(FlowConcurrencyError {
            flow_name: flow_name.to_string(),
            shop: shop.to_string(),
            existing_run_id: existing_run_id.to_string(),
        });
    }
    Ok(())
}

/// Inserts a `pending` run. Fails with [`UnknownFlowError`] for a flow that is
/// not registered, and with [`InvalidFlowInput`] when the input fails the input
/// schema of the flow. Concurrency is decided under an advisory lock scoped
/// to `(app, shop, flow)` so that simultaneous dispatches cannot both pass.
pub async fn dispatch_flow(db: &PgPool, config: &OpenShopConfig, options: DispatchOptions) -> Result<QueuedRun> {
    Ok(queue_run(db, config, options, None)
        .await?
        .expect("a run without a tick is always queued"))
}

/// An occurrence of a cron for a shop: what the processes running the same
/// scheduler agree on to queue its run once.
pub(crate) struct CronTick<'a> {
    pub cron_key: &'a str,
    pub scheduled_at: DateTime<Utc>,
    /// An occurrence older than this, by the clock of the database, is not claimed.
    pub max_lateness: Duration,
}

/// Queues the run of a cron occurrence, unless a process already did. The
/// claim of the occurrence and the run are written in one transaction, so a
/// run that is refused leaves the occurrence free. `None` when it was taken, or
/// is too late to be claimed: the freshness is decided by the claim itself,
/// after any wait for the lock, since the claims of older occurrences may have
/// been removed by now.
pub(crate) async fn dispatch_cron_tick(
    db: &PgPool,
    config: &OpenShopConfig,
    options: DispatchOptions,
    tick: &CronTick<'_>,
) -> Result<Option<QueuedRun>> {
    queue_run(db, config, options, Some(tick)).await
}

async fn queue_run(
    db: &PgPool,
    config: &OpenShopConfig,
    options: DispatchOptions,
    tick: Option<&CronTick<'_>>,
) -> Result<Option<QueuedRun>> {
    let DispatchOptions {
        flow_name,
        input,
        shop,
        shopify_app,
        parent_run_id,
        delay,
        retry_policy: run_retry_policy,
    } = options;
    let shopify_app = shopify_app.unwrap_or_else(|| DEFAULT_SHOPIFY_APP_HANDLE.to_string());
    let input = input.unwrap_or_else(|| Value::Object(JsonMap::new()));

    let Some(flow) = config.flows.get(&flow_name) else {
        return Err(unknown_flow(config, &flow_name));
    };

    // The caller's mistake, before a run is queued that could only fail.
    if let Some(schema) = &flow.input {
        // A validator is the code of the application: it may panic.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| schema.parse(input.clone()))) {
            Ok(Ok(_)) => {}
            Ok(Err(summary)) => return Err(InvalidFlowInput { flow_name, summary }.into()),
            Err(_) => bail!("The input validator of flow \"{flow_name}\" panicked"),
        }
    }

    let mut tx = db.begin().await?;
    lock_flow(&mut tx, &shopify_app, &shop, &flow_name).await?;

    if let Some(tick) = tick {
        let claimed = sqlx::query(
            "INSERT INTO cron_ticks (app_handle, shop, cron_key, scheduled_at) \
             SELECT $1, $2, $3, $4::timestamptz \
             WHERE $4::timestamptz >= clock_timestamp() - make_interval(secs => $5) \
             ON CONFLICT DO NOTHING",
        )
        .bind(&shopify_app)
        .bind(&shop)
        .bind(tick.cron_key)
        .bind(tick.scheduled_at)
        .bind(tick.max_lateness.num_milliseconds() as f64 / 1000.0)
        .execute(&mut *tx)
        .await?;
        if claimed.rows_affected() == 0 {
            return Ok(None);
        }
    }

    if flow.concurrency.unwrap_or_default() == Concurrency::Reject {
        reject_when_active(&mut tx, &shopify_app, &shop, &flow_name, None).await?;
    }

    // The deadline and the date from which the run can be claimed are dates of the
    // clock of the database, which is the one the workers compare them with.
    let deadline_ms = flow.timeout.map(|timeout| timeout.as_millis().min(i64::MAX as u128) as i64);
    let retry_policy = resolve_retry_policy(&[Some(config.retry_policy), Some(flow.retry_policy), run_retry_policy]);
    let delay_ms = delay.map(millis).unwrap_or(0).min(i64::MAX as u64) as i64;

    let (run_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO flow_runs \
           (app_handle, shop, flow_name, status, input, deadline_at, parent_run_id, retry_policy, available_at) \
         VALUES ($1, $2, $3, 'pending', $4::json, \
           clock_timestamp() + $5::bigint * interval '1 millisecond', $6, $7::json, \
           clock_timestamp() + $8::bigint * interval '1 millisecond') \
         RETURNING id",
    )
    .bind(&shopify_app)
    .bind(&shop)
    .bind(&flow_name)
    .bind(input.to_string())
    .bind(deadline_ms)
    .bind(parent_run_id)
    .bind(serde_json::to_string(&retry_policy)?)
    .bind(delay_ms)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Some(QueuedRun {
        run_id,
        status: "pending",
    }))
}
