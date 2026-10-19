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
use uuid::Uuid;

use super::abort::{cleanup_abort, register_abort};
use super::backoff::compute_next_retry_at;
use super::dispatch::unknown_flow;
use super::errors::InvalidFlowInput;
use super::errors::{FlowCanceledError, FlowTimeoutError, SleepSignal};
use super::step::{StepExecutor, error_message};
use crate::app::OpenShop;
use crate::runtime::report::{self, Description, Panicked};
use crate::runtime::time::to_iso;
use crate::runtime::FlowLogger;
use crate::server::connectors::Connectors;
use crate::server::shopify_apps::DEFAULT_SHOPIFY_APP_HANDLE;
use crate::shopify::client::ShopifyClient;
use crate::types::{ErrorContext, FlowContext, JsonMap, ResolvedRetryPolicy};

/// Default delay between two lease extensions.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Extends the lease. Resolves to `false` when this worker no longer owns the run.
pub type HeartbeatFn = Arc<dyn Fn() -> BoxFuture<'static, Result<bool>> + Send + Sync>;

#[derive(Clone)]
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
    pub on_heartbeat: Option<HeartbeatFn>,
    /// Delay between two heartbeats. Defaults to [`HEARTBEAT_INTERVAL`].
    pub heartbeat_interval: Option<Duration>,
    /// Replaces the provider connectors, typically with fakes in tests.
    pub connectors: Option<Connectors>,
}

impl RunFlowOptions {
    pub fn new(run_id: Uuid, flow_name: impl Into<String>, shop: impl Into<String>) -> Self {
        Self {
            run_id,
            flow_name: flow_name.into(),
            input: None,
            shop: shop.into(),
            shopify_app: None,
            worker_id: None,
            attempt: None,
            on_heartbeat: None,
            heartbeat_interval: None,
            connectors: None,
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

impl RunOutcome {
    pub fn status(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Sleeping { .. } => "sleeping",
            Self::Failed { .. } => "failed",
            Self::Canceled => "canceled",
            Self::LeaseLost => "lease_lost",
        }
    }
}

/// Stops the heartbeat and forgets the cancellation token on every exit path.
struct RunGuard {
    run_id: String,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        cleanup_abort(&self.run_id);
    }
}

pub(crate) fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Terminal or intermediate state written at the end of an attempt.
struct RunUpdate<'a> {
    status: &'static str,
    error: Option<&'a str>,
    available_at: Option<DateTime<Utc>>,
    completed: bool,
}

impl<'a> RunUpdate<'a> {
    fn status(status: &'static str) -> Self {
        Self {
            status,
            error: None,
            available_at: None,
            completed: false,
        }
    }

    fn completed(mut self) -> Self {
        self.completed = true;
        self
    }

    fn error(mut self, error: &'a str) -> Self {
        self.error = Some(error);
        self
    }

    fn available_at(mut self, available_at: DateTime<Utc>) -> Self {
        self.available_at = Some(available_at);
        self
    }
}

/// Applies an update only while the run is owned, and reports whether it was.
async fn finish_run(db: &PgPool, run_id: Uuid, worker_id: Option<&str>, update: RunUpdate<'_>) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE flow_runs SET \
           status = $2, \
           error = CASE WHEN $2 = 'completed' THEN NULL ELSE COALESCE($3, error) END, \
           available_at = COALESCE($4, available_at), \
           completed_at = CASE WHEN $5 THEN now() ELSE completed_at END, \
           worker_id = NULL \
         WHERE id = $1 AND ($6::text IS NULL OR (worker_id = $6 AND status = 'running'))",
    )
    .bind(run_id)
    .bind(update.status)
    .bind(update.error)
    .bind(update.available_at)
    .bind(update.completed)
    .bind(worker_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() > 0)
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
    let signal = register_abort(&run_id_text);

    let heartbeat_interval = opts.heartbeat_interval.unwrap_or(HEARTBEAT_INTERVAL);
    let heartbeat = opts.on_heartbeat.clone().map(|on_heartbeat| {
        let signal = signal.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(heartbeat_interval);
            ticks.tick().await;
            loop {
                ticks.tick().await;
                // A failed extension is retried on the next tick; a lost lease stops the flow.
                if matches!(on_heartbeat().await, Ok(false)) {
                    signal.cancel();
                    return;
                }
            }
        })
    });
    let _guard = RunGuard {
        run_id: run_id_text.clone(),
        heartbeat,
    };

    let logger = FlowLogger::new(db.clone(), run_id_text.clone());
    let attempt = match opts.attempt {
        Some(attempt) => attempt,
        None => resolve_run_attempt(db, run_id).await?,
    };
    report::describe(|run| {
        run.tag("attempt", attempt);
    });
    let worker_id = opts.worker_id.as_deref();
    let step = StepExecutor::new(
        db.clone(),
        run_id_text,
        logger.clone(),
        Some(signal.clone()),
        flow.step_timeout,
        attempt,
    );
    let connectors = match opts.connectors.clone() {
        Some(connectors) => connectors,
        None => Connectors::for_shop(app, &opts.shop, &shopify_app),
    };
    let shopify = ShopifyClient::new(app, &opts.shop, &shopify_app);

    let execution = async {
        let validated_input = match &flow.input {
            Some(schema) => schema
                .parse(input)
                .map_err(|summary| {
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
        outcome.unwrap_or_else(|panic| Err(Panicked::of_attempt(format!("Flow panicked: {}", panic_message(panic)))))
    };

    let outcome = execution.await;
    // What the flow logged is stored before its outcome is.
    logger.flush().await;

    let result = match outcome {
        Ok(()) => {
            if finish_run(db, run_id, worker_id, RunUpdate::status("completed").completed()).await? {
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
            if let Some(sleep) = error.downcast_ref::<SleepSignal>() {
                let resume_at = sleep.resume_at;
                if finish_run(
                    db,
                    run_id,
                    worker_id,
                    RunUpdate::status("sleeping").available_at(resume_at),
                )
                .await?
                {
                    logger.info_with(
                        format!("Flow \"{flow_name}\" sleeping"),
                        json!({ "flowName": flow_name, "resumeAt": to_iso(&resume_at) }),
                    );
                    RunOutcome::Sleeping { resume_at }
                } else {
                    RunOutcome::LeaseLost
                }
            } else if error.downcast_ref::<FlowCanceledError>().is_some() || signal.is_cancelled() {
                if finish_run(db, run_id, worker_id, RunUpdate::status("canceled").completed()).await? {
                    logger.info_with(
                        format!("Flow \"{flow_name}\" was canceled"),
                        json!({ "flowName": flow_name }),
                    );
                    RunOutcome::Canceled
                } else {
                    RunOutcome::LeaseLost
                }
            } else {
                record_failure(app, run_id, worker_id, flow_name, &logger, error).await?
            }
        }
    };

    logger.flush().await;
    Ok(result)
}

async fn record_failure(
    app: &OpenShop,
    run_id: Uuid,
    worker_id: Option<&str>,
    flow_name: &str,
    logger: &FlowLogger,
    error: anyhow::Error,
) -> Result<RunOutcome> {
    let db = app.db();
    let error_message = error_message(&error);

    let run: Option<(i32, Option<DateTime<Utc>>, Option<Value>)> =
        sqlx::query_as("SELECT attempts, deadline_at, retry_policy FROM flow_runs WHERE id = $1 LIMIT 1")
            .bind(run_id)
            .fetch_optional(db)
            .await?;

    let next_at = run.and_then(|(attempts, deadline_at, retry_policy)| {
        let policy: ResolvedRetryPolicy = serde_json::from_value(retry_policy?).ok()?;
        compute_next_retry_at(attempts as i64, &policy, deadline_at)
    });

    let will_retry = next_at.is_some();
    let update = match next_at {
        Some(next_at) => RunUpdate::status("pending").error(&error_message).available_at(next_at),
        None => RunUpdate::status("failed").error(&error_message).completed(),
    };
    let owned = finish_run(db, run_id, worker_id, update).await?;
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
