//! Executes one attempt of a flow run and records its outcome.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::abort::{Abort, register_abort};
use super::backoff::compute_next_retry_at;
use super::dispatch::unknown_flow;
use super::errors::InvalidFlowInput;
use super::errors::{FlowCanceledError, FlowTimeoutError, LeaseLostError, SleepSignal};
use super::step::{RunOwner, StepExecutor, error_message};
use super::transient::retry;
use crate::app::OpenShop;
use crate::runtime::FlowLogger;
use crate::runtime::logger;
use crate::runtime::report::{self, Description, Panicked};
use crate::runtime::time::to_iso;
use crate::server::connectors::Connectors;
use crate::server::shopify_apps::DEFAULT_SHOPIFY_APP_HANDLE;
use crate::shopify::client::ShopifyClient;
use crate::types::{ErrorContext, FlowContext, JsonMap, ResolvedRetryPolicy};

/// Default delay between two lease extensions.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Extends the lease. Resolves to `false` when this worker no longer owns the run.
pub type HeartbeatFn = Arc<dyn Fn() -> BoxFuture<'static, Result<bool>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct RunFlowOptions {
    pub run_id: Uuid,
    pub flow_name: String,
    /// Defaults to `{}`.
    pub input: Option<Value>,
    pub shop: String,
    pub shopify_app: Option<String>,
    /// When set, the outcome is only recorded if this worker still owns the run.
    pub worker_id: Option<String>,
    pub attempt: Option<i32>,
    /// The claim the worker holds on the run: the outcome is only recorded while it does.
    pub lease_id: Option<Uuid>,
    pub on_heartbeat: Option<HeartbeatFn>,
    /// Delay between two heartbeats. Defaults to [`HEARTBEAT_INTERVAL`].
    pub heartbeat_interval: Option<Duration>,
    /// The instant just before the statement that leased the run was sent. The
    /// lease ends no sooner than this plus `lease_duration`: the database dates
    /// it from the claim, which may have waited for a connection or a lock before
    /// it answered. Without it, the lease is counted from the start of the run.
    pub claimed_at: Option<tokio::time::Instant>,
    /// How long a lease lasts. When the heartbeat fails to extend it for that
    /// long, the lease has expired and another worker may have taken the run:
    /// the execution stops, and records nothing. Without it, the heartbeat
    /// keeps trying for as long as the flow runs.
    pub lease_duration: Option<Duration>,
    /// Cancelling it stops the execution without recording its outcome, as when
    /// the run was handed back to the queue: whoever cancels it owns the
    /// outcome from then on.
    pub interrupt: Option<CancellationToken>,
    /// Replaces the provider connectors, typically with fakes in tests.
    pub connectors: Option<Connectors>,
}

impl RunFlowOptions {
    pub fn new(run_id: Uuid, flow_name: impl Into<String>, shop: impl Into<String>) -> Self {
        Self {
            run_id,
            flow_name: flow_name.into(),
            shop: shop.into(),
            ..Default::default()
        }
    }

    pub fn input(mut self, input: Value) -> Self {
        self.input = Some(input);
        self
    }

    pub fn shopify_app(mut self, handle: impl Into<String>) -> Self {
        self.shopify_app = Some(handle.into());
        self
    }

    pub fn worker_id(mut self, worker_id: impl Into<String>) -> Self {
        self.worker_id = Some(worker_id.into());
        self
    }

    pub fn attempt(mut self, attempt: i32) -> Self {
        self.attempt = Some(attempt);
        self
    }

    pub fn lease_id(mut self, lease_id: Option<Uuid>) -> Self {
        self.lease_id = lease_id;
        self
    }

    pub fn connectors(mut self, connectors: Connectors) -> Self {
        self.connectors = Some(connectors);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    Completed,
    Sleeping {
        resume_at: DateTime<Utc>,
    },
    Failed {
        error: String,
        will_retry: bool,
    },
    Canceled,
    /// Another worker took over the run, or it was canceled from elsewhere.
    LeaseLost,
}

/// Stops the heartbeat and forgets the cancellation token on every exit path.
struct RunGuard {
    abort: Abort,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        self.abort.cleanup();
    }
}

/// Takes the flow away from an execution whose lease ran out.
fn expire(run_id: Uuid, halt: &CancellationToken) {
    logger::warn(format!(
        "[openshop] Lease of run {run_id} expired without a heartbeat, stopping its execution"
    ));
    halt.cancel();
}

/// The message of a panic, from what it was given.
pub(crate) fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

/// Writes the state of a run at the end of an attempt: `completed`, `failed`
/// and `canceled` end it, `pending` and `sleeping` make it wait for
/// `available_at`. An `error` replaces the previous one; a completed run has
/// none.
///
/// The update applies only while the run is owned, and reports whether it was.
///
/// A transient error is retried; the ownership is checked again by every try,
/// so one that comes after the run was taken over writes nothing. If the first
/// try did commit and only its answer was lost, the next one finds the run
/// finished and reports it as not owned.
async fn finish_run(
    db: &PgPool,
    run_id: Uuid,
    owner: Option<&RunOwner>,
    status: &str,
    error: Option<&str>,
    available_at: Option<DateTime<Utc>>,
) -> Result<bool> {
    retry(format!("recording the outcome of run {run_id}"), || async {
        let result = sqlx::query(
            "UPDATE flow_runs SET \
           status = $2, \
           error = CASE WHEN $2 = 'completed' THEN NULL ELSE COALESCE($3, error) END, \
           available_at = COALESCE($4, available_at), \
           completed_at = CASE WHEN $2 IN ('completed', 'failed', 'canceled') THEN now() ELSE completed_at END, \
           worker_id = NULL, \
           lease_id = NULL \
         WHERE id = $1 AND ($5::text IS NULL \
           OR (worker_id = $5 AND status = 'running' AND ($6::uuid IS NULL OR lease_id = $6)))",
        )
        .bind(run_id)
        .bind(status)
        .bind(error)
        .bind(available_at)
        .bind(owner.map(|owner| owner.worker_id.as_str()))
        .bind(owner.and_then(|owner| owner.lease_id))
        .execute(db)
        .await?;
        Ok(result.rows_affected() > 0)
    })
    .await
}

/// Names a run for the failures reported about it.
pub(crate) fn describe_run(run: &mut Description<'_>, flow_name: &str, shop: &str, shopify_app: &str, run_id: Uuid) {
    run.unit("flow", flow_name)
        .shop(shop, shopify_app)
        .tag("run_id", run_id);
}

pub async fn run_flow(app: &OpenShop, opts: RunFlowOptions) -> Result<RunOutcome> {
    // A run has a scope of its own: its failures carry its name and its log lines.
    report::isolated(report::holding_panics(async {
        let shopify_app = opts.shopify_app.as_deref().unwrap_or(DEFAULT_SHOPIFY_APP_HANDLE);
        report::describe(|run| describe_run(run, &opts.flow_name, &opts.shop, shopify_app, opts.run_id));
        let outcome = run_attempt(app, opts).await;
        // The attempt could not be made, or its outcome could not be recorded.
        if let Err(error) = &outcome {
            report::error(error);
        }
        outcome
    }))
    .await
}

async fn run_attempt(app: &OpenShop, opts: RunFlowOptions) -> Result<RunOutcome> {
    let db = app.db();
    let config = app.config();
    let flow_name = opts.flow_name.as_str();
    let shopify_app = opts
        .shopify_app
        .clone()
        .unwrap_or_else(|| DEFAULT_SHOPIFY_APP_HANDLE.to_string());
    let input = opts.input.clone().unwrap_or_else(|| Value::Object(JsonMap::new()));

    let Some(flow) = config.flows.get(flow_name).cloned() else {
        return Err(unknown_flow(config, flow_name));
    };

    let run_id = opts.run_id;
    let run_id_text = run_id.to_string();
    // Cancelled when the engine takes the execution away from the flow: its lease
    // ran out, the run is owned by another execution, or the worker hands it back.
    // The flow is then dropped, not just told to stop, so that its code outside the
    // steps does not go on beside the execution that owns the run now.
    let halt = opts.interrupt.clone().unwrap_or_default().child_token();
    let abort = register_abort(&run_id_text, Some(&halt));
    let signal = abort.token.clone();

    let heartbeat_interval = opts.heartbeat_interval.unwrap_or(HEARTBEAT_INTERVAL);
    let lease = opts.lease_duration;
    // The lease ends, for every other worker, `lease` after the database got the
    // last request that extended it (or the claim). This task cannot know when
    // that was exactly, only when it sent the request: the lease ends no sooner
    // than that, and a margin covers the time to hear the answer.
    let margin = lease.map_or(Duration::ZERO, |lease| lease / 10);
    let deadline_after = move |sent: tokio::time::Instant| lease.map(|lease| sent + lease.saturating_sub(margin));
    let first_deadline = deadline_after(opts.claimed_at.unwrap_or_else(tokio::time::Instant::now));
    if first_deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
        // The claim took so long to answer that the lease is about to end, or has: another
        // worker may take the run before this flow could do anything. It is not started.
        logger::warn(format!(
            "[openshop] Lease of run {run_id} was about to expire when it was claimed, not starting it"
        ));
        abort.cleanup();
        return Ok(RunOutcome::LeaseLost);
    }
    let heartbeat = opts.on_heartbeat.clone().map(|on_heartbeat| {
        let halt = halt.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(heartbeat_interval);
            ticks.tick().await;
            // The deadline is raced against everything this task waits for, a tick as well
            // as a heartbeat that hangs on a saturated pool or a lock: once it has passed,
            // another worker may own the run, and the flow must be gone before it starts.
            let mut deadline = first_deadline;
            let expiry = |deadline: Option<tokio::time::Instant>| async move {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            };
            let mut failures: u32 = 0;
            loop {
                tokio::select! {
                    _ = ticks.tick() => {}
                    // Nothing to extend once the execution is taken away.
                    _ = halt.cancelled() => return,
                    _ = expiry(deadline) => {
                        expire(run_id, &halt);
                        return;
                    }
                }
                let sent = tokio::time::Instant::now();
                let result = tokio::select! {
                    result = on_heartbeat() => result,
                    _ = halt.cancelled() => return,
                    _ = expiry(deadline) => {
                        logger::warn(format!(
                            "[openshop] Heartbeat of run {run_id} did not answer before the lease ran out"
                        ));
                        expire(run_id, &halt);
                        return;
                    }
                };
                match result {
                    // Another execution owns the run, or it was canceled: stop the flow.
                    Ok(false) => {
                        halt.cancel();
                        return;
                    }
                    Ok(true) => {
                        if failures > 0 {
                            logger::info(format!(
                                "[openshop] Heartbeat of run {run_id} recovered after {failures} failures"
                            ));
                        }
                        failures = 0;
                        deadline = deadline_after(sent);
                    }
                    // A failed extension is tried again on the next tick, until the deadline
                    // above. The first failure and every tenth after it are logged, so that
                    // an outage does not flood the logs.
                    Err(error) => {
                        failures += 1;
                        if failures == 1 || failures.is_multiple_of(10) {
                            logger::warn(format!(
                                "[openshop] Heartbeat of run {run_id} failed ({failures} in a row): {error:#}"
                            ));
                        }
                    }
                }
            }
        })
    });
    let _guard = RunGuard { abort, heartbeat };

    // The lines of the run say, in the console, what it runs for.
    let logger = FlowLogger::new(db.clone(), run_id_text.clone()).with_fields(json!({
        "shop": opts.shop,
        "shopifyApp": shopify_app,
        "flow": flow_name,
    }));
    let attempt = match opts.attempt {
        Some(attempt) => attempt,
        None => resolve_run_attempt(db, run_id).await?,
    };
    report::describe(|run| {
        run.tag("attempt", attempt);
    });
    let owner = opts.worker_id.clone().map(|worker_id| RunOwner {
        worker_id,
        lease_id: opts.lease_id,
    });
    let owner = owner.as_ref();
    let step = StepExecutor::new(
        db.clone(),
        run_id_text,
        logger.clone(),
        Some(signal.clone()),
        flow.step_timeout,
        attempt,
        owner.cloned(),
    );
    let connectors = match opts.connectors.clone() {
        Some(connectors) => connectors,
        None => Connectors::for_shop(app, &opts.shop, &shopify_app),
    };
    let shopify = ShopifyClient::new(app, &opts.shop, &shopify_app);

    let execution = async {
        let validated_input = match &flow.input {
            Some(schema) => schema.parse(input).map_err(|summary| {
                anyhow::Error::new(InvalidFlowInput {
                    flow_name: flow_name.to_string(),
                    summary,
                })
            })?,
            None => input,
        };

        logger.info_with(
            format!("Flow \"{flow_name}\" started"),
            json!({ "flowName": flow_name }),
        );

        let context = FlowContext {
            input: validated_input,
            connectors,
            shopify,
            shop: opts.shop.clone(),
            shopify_app: shopify_app.clone(),
            step,
            logger: logger.clone(),
            signal: signal.clone(),
            db: db.clone(),
            app: app.clone(),
        };

        // The flow function is called inside the guarded future so that a panic
        // before its first await is caught too.
        let run = AssertUnwindSafe(async { (flow.run)(context).await }).catch_unwind();
        let outcome = match flow.timeout {
            Some(timeout) => match tokio::time::timeout(timeout, run).await {
                Ok(outcome) => outcome,
                Err(_) => {
                    return Err(anyhow!(FlowTimeoutError {
                        flow_name: flow_name.to_string(),
                        timeout_ms: timeout.as_millis() as u64,
                    }));
                }
            },
            None => run.await,
        };
        outcome.unwrap_or_else(|panic| {
            Err(Panicked::of_attempt(format!(
                "Flow panicked: {}",
                panic_message(&panic)
            )))
        })
    };

    // The execution is raced against the halt, and dropped when it wins.
    let outcome = tokio::select! {
        biased;
        _ = halt.cancelled() => None,
        outcome = execution => Some(outcome),
    };
    let Some(outcome) = outcome else {
        // The flow is gone. Whoever owns the run now records what comes next.
        logger.flush().await;
        return Ok(RunOutcome::LeaseLost);
    };
    // What the flow logged is stored before its outcome is.
    logger.flush().await;

    let result = match outcome {
        Ok(()) => {
            if finish_run(db, run_id, owner, "completed", None, None).await? {
                logger.info_with(
                    format!("Flow \"{flow_name}\" completed"),
                    json!({ "flowName": flow_name }),
                );
                RunOutcome::Completed
            } else {
                RunOutcome::LeaseLost
            }
        }
        Err(error) => {
            if error.downcast_ref::<LeaseLostError>().is_some() {
                // A step found the run taken over: nothing more is written.
                RunOutcome::LeaseLost
            } else if let Some(sleep) = error.downcast_ref::<SleepSignal>() {
                let resume_at = sleep.resume_at;
                if finish_run(db, run_id, owner, "sleeping", None, Some(resume_at)).await? {
                    logger.info_with(
                        format!("Flow \"{flow_name}\" sleeping"),
                        json!({ "flowName": flow_name, "resumeAt": to_iso(&resume_at) }),
                    );
                    RunOutcome::Sleeping { resume_at }
                } else {
                    RunOutcome::LeaseLost
                }
            } else if halt.is_cancelled() {
                // The engine stopped the flow, not a cancellation of the run: the lease
                // ran out, or the worker handed the run back. Whoever owns it now records
                // what comes next.
                RunOutcome::LeaseLost
            } else if error.downcast_ref::<FlowCanceledError>().is_some() || signal.is_cancelled() {
                if finish_run(db, run_id, owner, "canceled", None, None).await? {
                    logger.info_with(
                        format!("Flow \"{flow_name}\" was canceled"),
                        json!({ "flowName": flow_name }),
                    );
                    RunOutcome::Canceled
                } else {
                    RunOutcome::LeaseLost
                }
            } else {
                record_failure(app, run_id, owner, flow_name, &logger, error).await?
            }
        }
    };

    logger.flush().await;
    Ok(result)
}

/// What decides whether a failed run is tried again, and when.
#[derive(sqlx::FromRow)]
struct RetryState {
    attempts: i32,
    deadline_at: Option<DateTime<Utc>>,
    retry_policy: Option<Value>,
    /// The time of the database.
    now: DateTime<Utc>,
}

async fn record_failure(
    app: &OpenShop,
    run_id: Uuid,
    owner: Option<&RunOwner>,
    flow_name: &str,
    logger: &FlowLogger,
    error: anyhow::Error,
) -> Result<RunOutcome> {
    let db = app.db();
    let error_message = error_message(&error);

    // The retry is dated with the clock of the database, like the claim that reads it.
    let run: Option<RetryState> = retry(format!("reading the retry policy of run {run_id}"), || async {
        Ok(sqlx::query_as(
            "SELECT attempts, deadline_at, retry_policy, now() AS now FROM flow_runs WHERE id = $1 LIMIT 1",
        )
        .bind(run_id)
        .fetch_optional(db)
        .await?)
    })
    .await?;

    let next_at = run.and_then(|run| {
        let policy: ResolvedRetryPolicy = serde_json::from_value(run.retry_policy?).ok()?;
        compute_next_retry_at(run.attempts as i64, &policy, run.deadline_at, run.now)
    });

    let will_retry = next_at.is_some();
    let status = if will_retry { "pending" } else { "failed" };
    let owned = finish_run(db, run_id, owner, status, Some(&error_message), next_at).await?;
    if !owned {
        return Ok(RunOutcome::LeaseLost);
    }

    // Reported before the line that says so is logged: the trail of the event
    // is what led to the failure.
    report::describe(|run| {
        run.tag("will_retry", will_retry);
    });
    if will_retry {
        report::warning(&error);
    } else {
        report::error(&error);
    }
    logger.error_with(
        format!("Flow \"{flow_name}\" failed: {error_message}"),
        json!({ "flowName": flow_name, "error": error_message, "willRetry": will_retry }),
    );

    if let Some(on_error) = &app.config().on_error {
        let context = ErrorContext {
            flow: Some(flow_name.to_string()),
            step: None,
        };
        // A failing error hook must not crash the worker.
        let _ = AssertUnwindSafe(async { on_error(Arc::new(error), context).await })
            .catch_unwind()
            .await;
    }

    Ok(RunOutcome::Failed {
        error: error_message,
        will_retry,
    })
}

async fn resolve_run_attempt(db: &PgPool, run_id: Uuid) -> Result<i32> {
    let run: Option<(i32,)> = sqlx::query_as("SELECT attempts FROM flow_runs WHERE id = $1 LIMIT 1")
        .bind(run_id)
        .fetch_optional(db)
        .await?;
    Ok(match run {
        Some((attempts,)) if attempts > 0 => attempts,
        _ => 1,
    })
}
