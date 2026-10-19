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
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::runner::{HEARTBEAT_INTERVAL, HeartbeatFn, RunFlowOptions, describe_run, panic_message, run_flow};
use super::step::error_message;
use super::transient::{is_transient, retry};
use crate::app::OpenShop;
use crate::runtime::{logger, report};
use crate::runtime::time::millis;
use crate::types::{JsonMap, ResolvedWorkerConfig, WorkerConfig};

pub const DEFAULT_WORKER_CONFIG: ResolvedWorkerConfig = ResolvedWorkerConfig {
    concurrency: 5,
    poll_interval_ms: 1_000,
    poll_max_interval_ms: 5_000,
    poll_backoff_coefficient: 1.5,
    lease_duration_ms: 30_000,
};

fn merge(mut config: ResolvedWorkerConfig, partial: &WorkerConfig) -> ResolvedWorkerConfig {
    if let Some(value) = partial.concurrency {
        config.concurrency = value;
    }
    if let Some(value) = partial.poll_interval {
        config.poll_interval_ms = millis(value);
    }
    if let Some(value) = partial.poll_max_interval {
        config.poll_max_interval_ms = millis(value);
    }
    if let Some(value) = partial.poll_backoff_coefficient {
        config.poll_backoff_coefficient = value;
    }
    if let Some(value) = partial.lease_duration {
        config.lease_duration_ms = millis(value);
    }
    config
}

/// Fails the unfinished runs that outlived their deadline, and returns them.
/// The `WHERE` clause repeats the predicate of `flow_runs_deadline_idx`.
const EXPIRE_SQL: &str = "\
    UPDATE flow_runs SET \
      status = 'failed', \
      error = 'Flow timed out — exceeded maximum allowed duration', \
      worker_id = NULL, available_at = NULL, completed_at = $1 \
    WHERE status IN ('pending', 'sleeping', 'running') \
      AND deadline_at IS NOT NULL AND deadline_at <= $1 \
    RETURNING id, flow_name, app_handle, shop, error";

/// A run failed without an attempt.
#[derive(Debug, Clone, sqlx::FromRow)]
struct FailedRun {
    id: Uuid,
    flow_name: String,
    app_handle: String,
    shop: String,
    error: String,
}

/// Reports a run that failed without an attempt: the runner, which reports
/// the others, never saw it.
fn report_failed_run(run: &FailedRun) {
    report::detached(
        |failed| describe_run(failed, &run.flow_name, &run.shop, &run.app_handle, run.id),
        &anyhow::anyhow!("{}", run.error),
    );
}

/// Leases up to `$4` runs in one statement and returns them oldest first.
///
/// `FOR UPDATE SKIP LOCKED` lets several workers claim at the same time. A
/// `running` row past its `available_at` is an expired lease and can be taken
/// over. A run past its deadline is left to [`EXPIRE_SQL`].
///
/// Every claim is an attempt, except the one of a run waking up from a sleep:
/// a sleep is not a failure, and a flow may sleep as often as it needs to.
/// An expired lease means the worker died before it recorded the outcome of
/// its attempt. A run that did so with its last attempt is failed instead of
/// claimed, or a flow that takes its worker down would be claimed forever. The
/// attempts allowed are the `maxAttempts` of the retry policy of the run, or
/// one when it has none, as in [`super::runner`] when a failure is recorded.
///
/// The candidates repeat the predicate of `flow_runs_claimable_idx`, which
/// yields them already ordered, and the updates find them by primary key:
/// the cost of a claim follows its size, not the length of the queue.
const CLAIM_SQL: &str = "\
    WITH candidate AS ( \
      SELECT id, available_at, created_at, \
        status = 'running' AND attempts >= GREATEST( \
          CASE WHEN retry_policy->>'maxAttempts' ~ '^[0-9]{1,9}$' \
            THEN (retry_policy->>'maxAttempts')::bigint ELSE 1 END, 1) AS exhausted \
      FROM flow_runs \
      WHERE status IN ('pending', 'sleeping', 'running') AND available_at <= $1 \
        AND (deadline_at IS NULL OR deadline_at > $1) \
      ORDER BY available_at ASC, created_at ASC \
      LIMIT $4 \
      FOR UPDATE SKIP LOCKED \
    ), failed AS ( \
      UPDATE flow_runs SET \
        status = 'failed', \
        error = 'Run abandoned: its worker stopped after ' || attempts || ' attempts without recording an outcome', \
        worker_id = NULL, available_at = NULL, completed_at = $1 \
      WHERE id = ANY(ARRAY(SELECT id FROM candidate WHERE exhausted)) \
      RETURNING id, flow_name, app_handle, shop, input, attempts, error \
    ), claimed AS ( \
      UPDATE flow_runs SET \
        status = 'running', \
        attempts = CASE WHEN status = 'sleeping' THEN GREATEST(attempts, 1) ELSE attempts + 1 END, \
        worker_id = $2, \
        available_at = $3, \
        started_at = COALESCE(started_at, $1) \
      WHERE id = ANY(ARRAY(SELECT id FROM candidate WHERE NOT exhausted)) \
      RETURNING id, flow_name, app_handle, shop, input, attempts, NULL::text AS error \
    ) \
    SELECT run.*, candidate.exhausted FROM ( \
      SELECT * FROM claimed UNION ALL SELECT * FROM failed \
    ) run JOIN candidate USING (id) \
    ORDER BY candidate.available_at, candidate.created_at";

/// A run the claim returned: leased to this worker, or failed because it used
/// all its attempts.
#[derive(Debug, Clone, sqlx::FromRow)]
struct ClaimedRun {
    id: Uuid,
    flow_name: String,
    app_handle: String,
    shop: String,
    input: Option<Value>,
    attempts: i32,
    error: Option<String>,
    exhausted: bool,
}

struct Inner {
    app: OpenShop,
    config: ResolvedWorkerConfig,
    worker_id: String,
    running: AtomicBool,
    active: AtomicUsize,
    /// Signalled when a run finishes, for a loop that waits for a free slot.
    slot_freed: Notify,
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
    pub fn new(app: OpenShop, overrides: WorkerConfig) -> Self {
        let config = merge(merge(DEFAULT_WORKER_CONFIG, &app.config().worker), &overrides);
        Self {
            inner: Arc::new(Inner {
                app,
                config,
                worker_id: Uuid::new_v4().to_string(),
                running: AtomicBool::new(false),
                active: AtomicUsize::new(0),
                slot_freed: Notify::new(),
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
            tokio::join!(inner.claim_loop(&stop), inner.expire_loop(&stop));
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
    /// Fills the free slots from the queue until the worker stops.
    async fn claim_loop(self: &Arc<Self>, stop: &CancellationToken) {
        let mut consecutive_empty: u32 = 0;

        while self.running.load(Ordering::SeqCst) {
            let free = self
                .config
                .concurrency
                .saturating_sub(self.active.load(Ordering::SeqCst));
            if free == 0 {
                // Every slot is taken: the next run to finish wakes the loop.
                tokio::select! {
                    _ = self.slot_freed.notified() => {}
                    _ = stop.cancelled() => {}
                }
                continue;
            }

            match self.claim(free).await {
                Ok((claimed, failed)) if claimed.is_empty() && failed == 0 => {
                    consecutive_empty += 1;
                    self.sleep(self.backoff_delay(consecutive_empty), stop).await;
                }
                Ok((claimed, _)) => {
                    consecutive_empty = 0;
                    for run in claimed {
                        self.clone().process_run(run);
                    }
                }
                Err(error) => {
                    // Keep polling through database outages.
                    consecutive_empty += 1;
                    logger::warn(format!("[openshop] Worker failed to claim runs: {error:#}"));
                    self.sleep(self.backoff_delay(consecutive_empty), stop).await;
                }
            }
        }
    }

    /// Expires the runs past their deadline once per poll interval, whether or
    /// not the worker has a free slot. Claiming skips them in the meantime.
    async fn expire_loop(&self, stop: &CancellationToken) {
        while self.running.load(Ordering::SeqCst) {
            let expired = sqlx::query_as::<_, FailedRun>(EXPIRE_SQL)
                .bind(Utc::now())
                .fetch_all(self.app.db())
                .await;
            match expired {
                Ok(runs) => {
                    for run in &runs {
                        report_failed_run(run);
                    }
                }
                Err(error) => logger::warn(format!(
                    "[openshop] Worker failed to expire runs past their deadline: {error:#}"
                )),
            }
            self.sleep(self.config.poll_interval_ms, stop).await;
        }
    }

    /// Leases runs, and fails the ones that used all their attempts. Resolves
    /// to the runs to execute and to how many were failed.
    async fn claim(&self, limit: usize) -> Result<(Vec<ClaimedRun>, usize)> {
        let rows = sqlx::query_as::<_, ClaimedRun>(CLAIM_SQL)
            .bind(Utc::now())
            .bind(&self.worker_id)
            .bind(self.lease_until())
            .bind(limit as i64)
            .fetch_all(self.app.db())
            .await?;
        let (exhausted, claimed): (Vec<_>, Vec<_>) = rows.into_iter().partition(|run| run.exhausted);
        for run in &exhausted {
            report_failed_run(&FailedRun {
                id: run.id,
                flow_name: run.flow_name.clone(),
                app_handle: run.app_handle.clone(),
                shop: run.shop.clone(),
                error: run.error.clone().unwrap_or_default(),
            });
        }
        Ok((claimed, exhausted.len()))
    }

    fn lease_until(&self) -> DateTime<Utc> {
        Utc::now() + chrono::Duration::milliseconds(self.config.lease_duration_ms as i64)
    }

    fn process_run(self: Arc<Self>, claimed: ClaimedRun) {
        self.active.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            self.execute(claimed).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.slot_freed.notify_one();
        });
    }

    /// Fails a run this worker still owns, at the attempt it claimed. A
    /// transient error is retried. If the write fails for good, the lease
    /// expires and the run is claimed again.
    async fn fail_run(&self, run_id: Uuid, attempt: i32, error: String) {
        let failed = retry(format!("failing run {run_id}"), || async {
            sqlx::query(
                "UPDATE flow_runs SET status = 'failed', error = $2, worker_id = NULL, completed_at = now() \
                 WHERE id = $1 AND worker_id = $3 AND status = 'running' AND attempts = $4",
            )
            .bind(run_id)
            .bind(&error)
            .bind(&self.worker_id)
            .bind(attempt)
            .execute(self.app.db())
            .await?;
            Ok(())
        })
        .await;
        if let Err(failure) = failed {
            logger::warn(format!(
                "[openshop] Run {run_id} is left to its lease, which expires without a heartbeat: {failure:#}"
            ));
        }
    }

    async fn execute(self: &Arc<Self>, claimed: ClaimedRun) {
        if !self.app.config().flows.contains_key(&claimed.flow_name) {
            let run = FailedRun {
                id: claimed.id,
                error: format!("Flow \"{}\" not registered", claimed.flow_name),
                flow_name: claimed.flow_name,
                app_handle: claimed.app_handle,
                shop: claimed.shop,
            };
            self.fail_run(run.id, claimed.attempts, run.error.clone()).await;
            report_failed_run(&run);
            return;
        }

        let input = match claimed.input {
            Some(Value::Object(input)) => Value::Object(input),
            _ => Value::Object(JsonMap::new()),
        };

        let heartbeat: HeartbeatFn = {
            let inner = self.clone();
            let (run_id, attempt) = (claimed.id, claimed.attempts);
            Arc::new(move || {
                let inner = inner.clone();
                Box::pin(async move {
                    let extended = sqlx::query(
                        "UPDATE flow_runs SET available_at = $1 \
                         WHERE id = $2 AND worker_id = $3 AND status = 'running' AND attempts = $4",
                    )
                    .bind(inner.lease_until())
                    .bind(run_id)
                    .bind(&inner.worker_id)
                    .bind(attempt)
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
            // The outcome could not be written, whatever the flow did. Failing
            // the run through the same database would only fail again, or turn
            // a blip into a definitive failure. Its heartbeat stopped with the
            // execution: the lease expires and the run is claimed again, from
            // its checkpoints.
            Ok(Err(error)) if is_transient(&error) => {
                logger::warn(format!(
                    "[openshop] Run {} is left to its lease, which expires without a heartbeat: {}",
                    claimed.id,
                    error_message(&error)
                ));
                return;
            }
            Ok(Err(error)) => error_message(&error),
            Err(panic) => format!("panicked: {}", panic_message(panic)),
        };
        self.fail_run(claimed.id, claimed.attempts, format!("Worker error: {failure}")).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDb;

    /// The plan PostgreSQL chooses for a statement of the worker, as JSON text.
    async fn plan(db: &TestDb, statement: &str, limit: Option<i64>) -> String {
        let explain = format!("EXPLAIN (FORMAT JSON) {statement}");
        let mut query = sqlx::query_as(&explain).bind(Utc::now());
        if let Some(limit) = limit {
            query = query.bind("worker").bind(Utc::now()).bind(limit);
        }
        let (plan,): (Value,) = query.fetch_one(&db.pool).await.unwrap();
        plan.to_string()
    }

    #[tokio::test]
    async fn reads_the_queue_through_its_indexes() {
        let db = TestDb::create().await.unwrap();
        // Enough runs for a scan of the table to cost more than an index.
        sqlx::raw_sql(
            "INSERT INTO flow_runs (shop, flow_name, available_at, deadline_at) \
             SELECT 'shop.myshopify.com', 'flow', now(), now() + interval '1 hour' FROM generate_series(1, 5000); \
             ANALYZE flow_runs",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        // An index whose predicate the statement does not repeat would be ignored.
        let claim = plan(&db, CLAIM_SQL, Some(5)).await;
        assert!(claim.contains("flow_runs_claimable_idx"), "{claim}");
        assert!(!claim.contains("Seq Scan"), "{claim}");
        let expire = plan(&db, EXPIRE_SQL, None).await;
        assert!(expire.contains("flow_runs_deadline_idx"), "{expire}");
        assert!(!expire.contains("Seq Scan"), "{expire}");
        db.cleanup().await.unwrap();
    }
}
