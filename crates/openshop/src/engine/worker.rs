//! Leases pending runs from PostgreSQL and executes them.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
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

/// Bounds of the time a heartbeat has to extend a lease.
const HEARTBEAT_MIN_TIMEOUT: Duration = Duration::from_millis(250);
const HEARTBEAT_MAX_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a shutdown waits for the executions it handed back to stop.
const RELEASE_GRACE: Duration = Duration::from_secs(1);

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
///
/// Every time of a lease or of a deadline is read from the clock of the
/// database, never from the one of a worker: replicas whose clocks differ must
/// agree on when a lease ends.
const EXPIRE_SQL: &str = "\
    UPDATE flow_runs SET \
      status = 'failed', \
      error = 'Flow timed out — exceeded maximum allowed duration', \
      worker_id = NULL, available_at = NULL, completed_at = now() \
    WHERE status IN ('pending', 'sleeping', 'running') \
      AND deadline_at IS NOT NULL AND deadline_at <= now() \
    RETURNING id, flow_name, app_handle, shop, error";

/// Extends the lease of a run, if its execution still owns it: `$1` is the
/// lease in milliseconds, `$2` the run, `$3` the worker, `$4` the attempt.
const HEARTBEAT_SQL: &str = "\
    UPDATE flow_runs SET available_at = now() + $1::bigint * interval '1 millisecond' \
    WHERE id = $2 AND worker_id = $3 AND status = 'running' AND attempts = $4";

/// Hands a run back to the queue, if its execution still owns it: `$1` is the
/// run, `$2` the worker, `$3` the attempt. The run is claimable right away.
/// Its `attempts` is left as it is: the interrupted attempt is the key of its
/// step rows, so the next claim must take a new one.
const RELEASE_SQL: &str = "\
    UPDATE flow_runs SET status = 'pending', worker_id = NULL, available_at = now() \
    WHERE id = $1 AND worker_id = $2 AND status = 'running' AND attempts = $3";

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

/// Leases up to `$3` runs in one statement, to the worker `$1` for `$2`
/// milliseconds, and returns them oldest first.
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
      WHERE status IN ('pending', 'sleeping', 'running') AND available_at <= now() \
        AND (deadline_at IS NULL OR deadline_at > now()) \
      ORDER BY available_at ASC, created_at ASC \
      LIMIT $3 \
      FOR UPDATE SKIP LOCKED \
    ), failed AS ( \
      UPDATE flow_runs SET \
        status = 'failed', \
        error = 'Run abandoned: its worker stopped after ' || attempts || ' attempts without recording an outcome', \
        worker_id = NULL, available_at = NULL, completed_at = now() \
      WHERE id = ANY(ARRAY(SELECT id FROM candidate WHERE exhausted)) \
      RETURNING id, flow_name, app_handle, shop, input, attempts, error \
    ), claimed AS ( \
      UPDATE flow_runs SET \
        status = 'running', \
        attempts = CASE WHEN status = 'sleeping' THEN GREATEST(attempts, 1) ELSE attempts + 1 END, \
        worker_id = $1, \
        available_at = now() + $2::bigint * interval '1 millisecond', \
        started_at = COALESCE(started_at, now()) \
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

/// A run this worker executes: the attempt it claimed it at, and the token
/// that stops the execution without recording an outcome.
struct Held {
    attempt: i32,
    interrupt: CancellationToken,
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
    /// The runs this worker leased and still executes. Shutdown hands back the
    /// ones that did not finish.
    held: std::sync::Mutex<HashMap<Uuid, Held>>,
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
                held: std::sync::Mutex::new(HashMap::new()),
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
    /// A run that is still executing then is handed back to the queue and its
    /// execution is stopped, so that another worker takes it up at once
    /// instead of when its lease expires. See [`Inner::release_held`].
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
        self.inner.release_held().await;
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
            .bind(&self.worker_id)
            .bind(self.lease_ms())
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

    /// The lease in milliseconds. The database adds it to its own clock.
    fn lease_ms(&self) -> i64 {
        self.config.lease_duration_ms.min(i64::MAX as u64) as i64
    }

    fn held(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Held>> {
        self.held.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn process_run(self: Arc<Self>, claimed: ClaimedRun) {
        self.active.fetch_add(1, Ordering::SeqCst);
        let interrupt = CancellationToken::new();
        let (run_id, attempt) = (claimed.id, claimed.attempts);
        self.held().insert(
            run_id,
            Held {
                attempt,
                interrupt: interrupt.clone(),
            },
        );
        tokio::spawn(async move {
            self.execute(claimed, interrupt).await;
            // Not if a newer claim of the same run, by this worker, replaced it.
            let mut held = self.held();
            if held.get(&run_id).is_some_and(|held| held.attempt == attempt) {
                held.remove(&run_id);
            }
            drop(held);
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.slot_freed.notify_one();
        });
    }

    /// Hands back the runs this worker still executes, once it stopped
    /// claiming and waited for them. Each goes back to `pending`, claimable
    /// at once, and its execution is interrupted: the flow is cancelled, but
    /// no outcome is recorded for it, because the run is not owned any more. The release is fenced like every write of an execution, by
    /// worker and attempt, so it cannot take a run another worker has claimed
    /// after its lease expired.
    ///
    /// The interrupted attempt stays counted in `attempts`: that number keys
    /// the step rows of the attempt, which the next claim must not reuse.
    /// A release is never an abandoned run, though: a run `pending` is claimed
    /// whatever its `attempts`, even with `max_attempts(1)`.
    async fn release_held(&self) {
        let held: Vec<(Uuid, i32, CancellationToken)> = self
            .held()
            .iter()
            .map(|(run, held)| (*run, held.attempt, held.interrupt.clone()))
            .collect();
        if held.is_empty() {
            return;
        }
        for (run_id, attempt, interrupt) in &held {
            let released = retry(format!("releasing run {run_id}"), || async {
                let result = sqlx::query(RELEASE_SQL)
                    .bind(run_id)
                    .bind(&self.worker_id)
                    .bind(attempt)
                    .execute(self.app.db())
                    .await?;
                Ok(result.rows_affected() > 0)
            })
            .await;
            match released {
                Ok(true) => logger::info(format!("[openshop] Released run {run_id} to the queue on shutdown")),
                Ok(false) => {}
                Err(error) => logger::warn(format!(
                    "[openshop] Run {run_id} is left to its lease, which expires without a heartbeat: {error:#}"
                )),
            }
            // After the release, so that the execution finds the run not owned.
            interrupt.cancel();
        }
        // Give the executions a moment to see the cancellation.
        let deadline = Instant::now() + RELEASE_GRACE;
        while self.active.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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

    async fn execute(self: &Arc<Self>, claimed: ClaimedRun, interrupt: CancellationToken) {
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
                Box::pin(async move { inner.extend_lease(run_id, attempt).await })
            })
        };

        let mut options = RunFlowOptions::new(claimed.id, claimed.flow_name, claimed.shop)
            .input(input)
            .shopify_app(claimed.app_handle)
            .worker_id(self.worker_id.clone())
            .attempt(claimed.attempts);
        options.on_heartbeat = Some(heartbeat);
        options.lease_duration = Some(Duration::from_millis(self.config.lease_duration_ms));
        options.interrupt = Some(interrupt);
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

    /// Extends the lease of a run, within the time it has to: a heartbeat that
    /// waits for a connection or for a lock longer than that is no use, as the
    /// lease it was meant to extend is about to end. Resolves to `false` when
    /// this execution no longer owns the run.
    ///
    /// The heartbeat shares the pool of the application. The time limit covers
    /// the wait for a connection when the pool is saturated by flows, and the
    /// server enforces it on the statement through `statement_timeout`.
    async fn extend_lease(&self, run_id: Uuid, attempt: i32) -> Result<bool> {
        let limit = self.heartbeat_timeout();
        let extend = async {
            let mut tx = self.app.db().begin().await?;
            sqlx::query("SELECT set_config('statement_timeout', $1, true)")
                .bind(limit.as_millis().to_string())
                .execute(&mut *tx)
                .await?;
            let extended = sqlx::query(HEARTBEAT_SQL)
                .bind(self.lease_ms())
                .bind(run_id)
                .bind(&self.worker_id)
                .bind(attempt)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(extended.rows_affected() > 0)
        };
        match tokio::time::timeout(limit, extend).await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("timed out after {}ms", limit.as_millis())),
        }
    }

    /// A quarter of the lease, so that a heartbeat which times out leaves the
    /// next ones room to succeed before the lease ends.
    fn heartbeat_timeout(&self) -> Duration {
        Duration::from_millis(self.config.lease_duration_ms / 4).clamp(HEARTBEAT_MIN_TIMEOUT, HEARTBEAT_MAX_TIMEOUT)
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
        let mut query = sqlx::query_as(&explain);
        if let Some(limit) = limit {
            query = query.bind("worker").bind(30_000_i64).bind(limit);
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
