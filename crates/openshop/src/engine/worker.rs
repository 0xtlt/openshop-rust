//! Leases pending runs from PostgreSQL and executes them.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::FutureExt;
use rand::Rng;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::runner::{HEARTBEAT_INTERVAL, HeartbeatFn, RunFlowOptions, panic_message, run_flow};
use super::step::error_message;
use crate::app::OpenShop;
use crate::runtime::logger;
use crate::types::{JsonMap, PartialWorkerConfig, WorkerConfig};

pub const DEFAULT_WORKER_CONFIG: WorkerConfig = WorkerConfig {
    concurrency: 5,
    poll_interval_ms: 1_000,
    poll_max_interval_ms: 5_000,
    poll_backoff_coefficient: 1.5,
    lease_duration_ms: 30_000,
};

fn merge(mut config: WorkerConfig, partial: &PartialWorkerConfig) -> WorkerConfig {
    if let Some(value) = partial.concurrency {
        config.concurrency = value;
    }
    if let Some(value) = partial.poll_interval_ms {
        config.poll_interval_ms = value;
    }
    if let Some(value) = partial.poll_max_interval_ms {
        config.poll_max_interval_ms = value;
    }
    if let Some(value) = partial.poll_backoff_coefficient {
        config.poll_backoff_coefficient = value;
    }
    if let Some(value) = partial.lease_duration_ms {
        config.lease_duration_ms = value;
    }
    config
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ClaimedRun {
    id: Uuid,
    flow_name: String,
    app_handle: String,
    shop: String,
    input: Option<Value>,
    attempts: i32,
}

struct Inner {
    app: OpenShop,
    config: WorkerConfig,
    worker_id: String,
    running: AtomicBool,
    active: AtomicUsize,
    wake: Mutex<CancellationToken>,
    loop_handle: Mutex<Option<JoinHandle<()>>>,
}

/// Run several workers against the same database to increase throughput: runs
/// are claimed with `FOR UPDATE SKIP LOCKED` and protected by a renewable lease.
#[derive(Clone)]
pub struct Worker {
    inner: Arc<Inner>,
}

impl Worker {
    pub fn new(app: OpenShop, overrides: PartialWorkerConfig) -> Self {
        let config = merge(merge(DEFAULT_WORKER_CONFIG, &app.config().worker), &overrides);
        Self {
            inner: Arc::new(Inner {
                app,
                config,
                worker_id: Uuid::new_v4().to_string(),
                running: AtomicBool::new(false),
                active: AtomicUsize::new(0),
                wake: Mutex::new(CancellationToken::new()),
                loop_handle: Mutex::new(None),
            }),
        }
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    pub fn active_count(&self) -> usize {
        self.inner.active.load(Ordering::SeqCst)
    }

    pub fn worker_id(&self) -> &str {
        &self.inner.worker_id
    }

    pub async fn start(&self) {
        if self.inner.running.swap(true, Ordering::SeqCst) {
            return;
        }
        logger::info(format!(
            "[openshop] Worker started (id={}, concurrency={})",
            &self.inner.worker_id[..8],
            self.inner.config.concurrency
        ));

        let stop = CancellationToken::new();
        *self.inner.wake.lock().await = stop.clone();
        let inner = self.inner.clone();
        *self.inner.loop_handle.lock().await = Some(tokio::spawn(async move {
            inner.run_loop(stop).await;
        }));
    }

    /// Stops claiming, then waits for active runs up to one lease duration.
    pub async fn stop(&self) {
        self.inner.running.store(false, Ordering::SeqCst);
        logger::info("[openshop] Worker stopping...");
        self.inner.wake.lock().await.cancel();
        if let Some(handle) = self.inner.loop_handle.lock().await.take() {
            let _ = handle.await;
        }

        let deadline = Instant::now() + Duration::from_millis(self.inner.config.lease_duration_ms);
        while self.active_count() > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        logger::info("[openshop] Worker stopped");
    }
}

impl Inner {
    async fn run_loop(self: Arc<Self>, stop: CancellationToken) {
        let mut consecutive_empty: u32 = 0;

        while self.running.load(Ordering::SeqCst) {
            if self.active.load(Ordering::SeqCst) >= self.config.concurrency {
                self.sleep(self.config.poll_interval_ms, &stop).await;
                continue;
            }

            match self.claim_one().await {
                Ok(Some(claimed)) => {
                    consecutive_empty = 0;
                    self.clone().process_run(claimed);
                }
                Ok(None) => {
                    consecutive_empty += 1;
                    self.sleep(self.backoff_delay(consecutive_empty), &stop).await;
                }
                Err(error) => {
                    // Keep polling through database outages.
                    consecutive_empty += 1;
                    logger::warn(format!("[openshop] Worker failed to claim a run: {error:#}"));
                    self.sleep(self.backoff_delay(consecutive_empty), &stop).await;
                }
            }
        }
    }

    async fn claim_one(&self) -> Result<Option<ClaimedRun>> {
        let db = self.app.db();
        let now = Utc::now();
        let lease_until = self.lease_until();

        // Expire deadline-exceeded runs
        sqlx::query(
            "UPDATE flow_runs SET \
               status = 'failed', \
               error = 'Flow timed out — exceeded maximum allowed duration', \
               worker_id = NULL, available_at = NULL, completed_at = $1 \
             WHERE status IN ('pending', 'running', 'sleeping') \
               AND deadline_at IS NOT NULL AND deadline_at <= $1",
        )
        .bind(now)
        .execute(db)
        .await?;

        // Atomic claim using FOR UPDATE SKIP LOCKED. A `running` row past its
        // `available_at` is an expired lease and can be taken over.
        let claimed = sqlx::query_as::<_, ClaimedRun>(
            "WITH candidate AS ( \
               SELECT id FROM flow_runs \
               WHERE status IN ('pending', 'sleeping', 'running') AND available_at <= $1 \
               ORDER BY available_at ASC, created_at ASC \
               LIMIT 1 \
               FOR UPDATE SKIP LOCKED \
             ) \
             UPDATE flow_runs SET \
               status = 'running', \
               attempts = attempts + 1, \
               worker_id = $2, \
               available_at = $3, \
               started_at = COALESCE(started_at, $1) \
             WHERE id = (SELECT id FROM candidate) \
             RETURNING id, flow_name, app_handle, shop, input, attempts",
        )
        .bind(now)
        .bind(&self.worker_id)
        .bind(lease_until)
        .fetch_optional(db)
        .await?;

        Ok(claimed)
    }

    fn lease_until(&self) -> DateTime<Utc> {
        Utc::now() + chrono::Duration::milliseconds(self.config.lease_duration_ms as i64)
    }

    fn process_run(self: Arc<Self>, claimed: ClaimedRun) {
        self.active.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            self.execute(claimed).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
        });
    }

    /// Fails a run this worker still owns. If the write fails too, the lease
    /// expires and the run is claimed again.
    async fn fail_run(&self, run_id: Uuid, error: String) {
        let _ = sqlx::query(
            "UPDATE flow_runs SET status = 'failed', error = $2, worker_id = NULL, completed_at = now() \
             WHERE id = $1 AND worker_id = $3 AND status = 'running'",
        )
        .bind(run_id)
        .bind(error)
        .bind(&self.worker_id)
        .execute(self.app.db())
        .await;
    }

    async fn execute(self: &Arc<Self>, claimed: ClaimedRun) {
        if !self.app.config().flows.contains_key(&claimed.flow_name) {
            self.fail_run(claimed.id, format!("Flow \"{}\" not registered", claimed.flow_name))
                .await;
            return;
        }

        let input = match claimed.input {
            Some(Value::Object(input)) => Value::Object(input),
            _ => Value::Object(JsonMap::new()),
        };

        let heartbeat: HeartbeatFn = {
            let inner = self.clone();
            let run_id = claimed.id;
            Arc::new(move || {
                let inner = inner.clone();
                Box::pin(async move {
                    let extended = sqlx::query(
                        "UPDATE flow_runs SET available_at = $1 \
                         WHERE id = $2 AND worker_id = $3 AND status = 'running'",
                    )
                    .bind(inner.lease_until())
                    .bind(run_id)
                    .bind(&inner.worker_id)
                    .execute(inner.app.db())
                    .await?;
                    Ok(extended.rows_affected() > 0)
                })
            })
        };

        let mut options = RunFlowOptions::new(claimed.id, claimed.flow_name, claimed.shop)
            .input(input)
            .shopify_app(claimed.app_handle)
            .worker_id(self.worker_id.clone())
            .attempt(claimed.attempts);
        options.on_heartbeat = Some(heartbeat);
        // Extend the lease at least twice per lease period.
        options.heartbeat_interval =
            Some(HEARTBEAT_INTERVAL.min(Duration::from_millis((self.config.lease_duration_ms / 2).max(1))));

        // A panic outside of the flow body (which the runner handles itself) must
        // neither leak this worker's slot nor leave the run leased.
        let failure = match AssertUnwindSafe(run_flow(&self.app, options)).catch_unwind().await {
            Ok(Ok(_)) => return,
            Ok(Err(error)) => error_message(&error),
            Err(panic) => format!("panicked: {}", panic_message(panic)),
        };
        self.fail_run(claimed.id, format!("Worker error: {failure}")).await;
    }

    fn backoff_delay(&self, consecutive: u32) -> u64 {
        let base = self.config.poll_interval_ms as f64
            * self
                .config
                .poll_backoff_coefficient
                .powi(consecutive.saturating_sub(1).min(64) as i32);
        let capped = base.min(self.config.poll_max_interval_ms as f64);
        (capped * (0.5 + rand::rng().random::<f64>() * 0.5)).round() as u64
    }

    async fn sleep(&self, ms: u64, stop: &CancellationToken) {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
            _ = stop.cancelled() => {}
        }
    }
}
