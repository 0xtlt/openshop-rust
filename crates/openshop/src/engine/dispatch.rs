//! Queueing a flow run.

use crate::Timestamp;
use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::backoff::resolve_retry_policy;
use super::errors::{FlowConcurrencyError, InvalidFlowInput, UnknownFlowError};
use super::scheduler::MAX_TICK_LATENESS_MINUTES;
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
    /// The run that started this one. [`for_scope`](Self::for_scope) sets it
    /// to the run of a flow context.
    pub parent_run_id: Option<String>,
    /// While another run of the flow for the shop with this key is active,
    /// the run is refused: see [`concurrency_key`](Self::concurrency_key).
    pub concurrency_key: Option<String>,
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
    /// webhook, a proxy request… The run of a flow context is the parent of
    /// the new run.
    pub fn for_scope(flow_name: impl Into<String>, scope: &impl ShopScope) -> Self {
        let mut options = Self::new(flow_name, scope.shop()).shopify_app(scope.shopify_app());
        options.parent_run_id = scope.run_id().map(|run_id| run_id.to_string());
        options
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

    /// One active run per resource: the run is refused, with a
    /// [`FlowConcurrencyError`], while another run of the flow for the shop
    /// with the same key (`order:123`) is `pending`, `running` or `sleeping`.
    /// It wins over the `concurrency_key` of the flow.
    pub fn concurrency_key(mut self, key: impl Into<String>) -> Self {
        self.concurrency_key = Some(key.into());
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
/// for the shop, or with a `concurrency_key`, another run with that key. Meant
/// to be called under [`lock_flow`], for a flow that rejects concurrent runs or
/// a run that has a key; `except` is the run that is about to become active.
pub(crate) async fn reject_when_active(
    tx: &mut PgConnection,
    shopify_app: &str,
    shop: &str,
    flow_name: &str,
    concurrency_key: Option<&str>,
    except: Option<Uuid>,
) -> Result<()> {
    // With a key, the statement repeats the predicate of `flow_runs_concurrency_key_idx`.
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM flow_runs \
         WHERE app_handle = $1 AND shop = $2 AND flow_name = $3 \
           AND status IN ('pending', 'running', 'sleeping') \
           AND ($4::uuid IS NULL OR id <> $4) \
           AND ($5::text IS NULL OR (concurrency_key IS NOT NULL AND concurrency_key = $5)) \
         LIMIT 1",
    )
    .bind(shopify_app)
    .bind(shop)
    .bind(flow_name)
    .bind(except)
    .bind(concurrency_key)
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
/// schema of the flow, and with [`FlowConcurrencyError`] when the flow rejects
/// concurrent runs and has an active one for the shop, or the run has a
/// concurrency key that an active run of the flow for the shop has too.
/// Concurrency is decided under an advisory lock scoped to `(app, shop, flow)`
/// so that simultaneous dispatches cannot both pass.
pub async fn dispatch_flow(db: &PgPool, config: &OpenShopConfig, options: DispatchOptions) -> Result<QueuedRun> {
    Ok(queue_run(db, config, options, None)
        .await?
        .expect("a run without a tick is always queued"))
}

/// An occurrence of a cron for a shop: what the processes running the same
/// scheduler agree on to queue its run once.
pub(crate) struct CronTick<'a> {
    pub cron_key: &'a str,
    pub scheduled_at: Timestamp,
}

/// Queues a run, or with a `tick`, the run of a cron occurrence unless a
/// process already did. The claim of the occurrence and the run are written in
/// one transaction, so a run that is refused leaves the occurrence free. `None`
/// when the occurrence was taken, or is older than
/// [`MAX_TICK_LATENESS_MINUTES`] by the clock of the database: the freshness is
/// decided by the claim itself, after any wait for the lock, since the claims
/// of older occurrences may have been removed by now.
pub(crate) async fn queue_run(
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
        concurrency_key,
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
    let concurrency_key = match (concurrency_key, &flow.concurrency_key) {
        (Some(key), _) => Some(key),
        (None, Some(key_of)) => {
            // The code of the application, like the validator.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| key_of(&input))) {
                Ok(key) => key,
                Err(_) => bail!("The concurrency key of flow \"{flow_name}\" panicked"),
            }
        }
        (None, None) => None,
    };

    let mut tx = db.begin().await?;
    lock_flow(&mut tx, &shopify_app, &shop, &flow_name).await?;

    if let Some(tick) = tick {
        let claimed = sqlx::query(
            "INSERT INTO cron_ticks (app_handle, shop, cron_key, scheduled_at) \
             SELECT $1, $2, $3, $4::timestamptz \
             WHERE $4::timestamptz >= clock_timestamp() - make_interval(mins => $5) \
             ON CONFLICT DO NOTHING",
        )
        .bind(&shopify_app)
        .bind(&shop)
        .bind(tick.cron_key)
        .bind(tick.scheduled_at)
        .bind(MAX_TICK_LATENESS_MINUTES as i32)
        .execute(&mut *tx)
        .await?;
        if claimed.rows_affected() == 0 {
            return Ok(None);
        }
    }

    if flow.concurrency.unwrap_or_default() == Concurrency::Reject {
        reject_when_active(&mut tx, &shopify_app, &shop, &flow_name, None, None).await?;
    } else if let Some(key) = &concurrency_key {
        reject_when_active(&mut tx, &shopify_app, &shop, &flow_name, Some(key), None).await?;
    }

    // The deadline and the date from which the run can be claimed are dates of the
    // clock of the database, which is the one the workers compare them with.
    let deadline_ms = flow
        .timeout
        .map(|timeout| timeout.as_millis().min(i64::MAX as u128) as i64);
    let retry_policy = resolve_retry_policy(&[Some(config.retry_policy), Some(flow.retry_policy), run_retry_policy]);
    let delay_ms = delay.map(millis).unwrap_or(0).min(i64::MAX as u64) as i64;

    let (run_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO flow_runs \
           (app_handle, shop, flow_name, status, input, deadline_at, parent_run_id, retry_policy, available_at, \
            concurrency_key) \
         VALUES ($1, $2, $3, 'pending', $4::json, \
           clock_timestamp() + $5::bigint * interval '1 millisecond', $6, $7::json, \
           clock_timestamp() + $8::bigint * interval '1 millisecond', $9) \
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
    .bind(&concurrency_key)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Some(QueuedRun {
        run_id,
        status: "pending",
    }))
}
