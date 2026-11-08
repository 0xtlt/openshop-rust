//! Leases pending runs from PostgreSQL and executes them.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::FutureExt;
use rand::RngExt;
use serde_json::Value;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::backoff::{compute_backoff_ms, jitter_ms};
use super::retention::{SWEEP_INTERVAL, purge_logs};
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

/// How long a shutdown waits for the executions it interrupts to be dropped.
const STOP_GRACE: Duration = Duration::from_secs(1);

/// How long a shutdown waits for a connection, a lock and the statement that
/// hands a run back.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(2);

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

/// The statements the worker sends with literals instead of parameters, to
/// run them with limits in one round trip (see [`bounded`]). The only values
/// are numbers and the IDs of a run, a worker and a claim, all UUIDs.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn claim_literal(lease_id: Option<Uuid>) -> String {
    lease_id.map_or_else(|| "NULL::uuid".to_string(), |id| format!("'{id}'::uuid"))
}

/// Extends the lease of a run, if its execution still owns it.
fn heartbeat_sql(lease_ms: i64, run_id: Uuid, worker_id: &str, lease_id: Option<Uuid>) -> String {
    format!(
        "UPDATE flow_runs SET available_at = now() + {lease_ms} * interval '1 millisecond' \
         WHERE id = '{run_id}' AND worker_id = {} AND status = 'running' \
           AND lease_id IS NOT DISTINCT FROM {}",
        literal(worker_id),
        claim_literal(lease_id)
    )
}

/// Hands a run back to the queue, if its execution still owns it. The run is
/// claimable right away. Its `attempts` is left as it is: the interrupted
/// attempt is the key of its step rows, so the next claim must take a new one.
fn release_sql(run_id: Uuid, worker_id: &str, lease_id: Option<Uuid>) -> String {
    format!(
        "UPDATE flow_runs SET status = 'pending', worker_id = NULL, lease_id = NULL, available_at = now() \
         WHERE id = '{run_id}' AND worker_id = {} AND status = 'running' \
           AND lease_id IS NOT DISTINCT FROM {}",
        literal(worker_id),
        claim_literal(lease_id)
    )
}

/// Prefixes `statement` with limits on how long the server waits for a lock
/// and for the statement to end. Both go in the one message, which the server
/// runs as a transaction of its own, so `SET LOCAL` limits it alone: a limit
/// costs no round trip.
fn bounded(limit: Duration, statement: &str) -> String {
    let ms = limit.as_millis();
    format!("SET LOCAL statement_timeout = {ms}; SET LOCAL lock_timeout = {ms}; {statement}")
}

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

// The statement of a claim, whose candidates are filtered by `$only` too.
macro_rules! claim_sql {
    ($only:literal) => { concat!("\
    WITH candidate AS ( \
      SELECT id, available_at, created_at, \
        status = 'running' AND attempts >= GREATEST( \
          CASE WHEN retry_policy->>'maxAttempts' ~ '^[0-9]{1,9}$' \
            THEN (retry_policy->>'maxAttempts')::bigint ELSE 1 END, 1) AS exhausted \
      FROM flow_runs \
      WHERE status IN ('pending', 'sleeping', 'running') AND available_at <= now() \
        AND (deadline_at IS NULL OR deadline_at > now()) ", $only, " \
      ORDER BY available_at ASC, created_at ASC \
      LIMIT $3 \
      FOR UPDATE SKIP LOCKED \
    ), failed AS ( \
      UPDATE flow_runs SET \
        status = 'failed', \
        error = 'Run abandoned: its worker stopped after ' || attempts || ' attempts without recording an outcome', \
        worker_id = NULL, lease_id = NULL, available_at = NULL, completed_at = now() \
      WHERE id = ANY(ARRAY(SELECT id FROM candidate WHERE exhausted)) \
      RETURNING id, flow_name, app_handle, shop, input, attempts, error, NULL::uuid AS lease_id \
    ), claimed AS ( \
      UPDATE flow_runs SET \
        status = 'running', \
        attempts = CASE WHEN status = 'sleeping' THEN GREATEST(attempts, 1) ELSE attempts + 1 END, \
        worker_id = $1, \
        lease_id = gen_random_uuid(), \
        available_at = now() + $2::bigint * interval '1 millisecond', \
        started_at = COALESCE(started_at, now()) \
      WHERE id = ANY(ARRAY(SELECT id FROM candidate WHERE NOT exhausted)) \
      RETURNING id, flow_name, app_handle, shop, input, attempts, NULL::text AS error, lease_id \
    ) \
    SELECT run.*, candidate.exhausted FROM ( \
      SELECT * FROM claimed UNION ALL SELECT * FROM failed \
    ) run JOIN candidate USING (id) \
    ORDER BY candidate.available_at, candidate.created_at")
    };
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
const CLAIM_SQL: &str = claim_sql!("");

/// [`CLAIM_SQL`] for the runs of `$4` only: what a worker of some runs claims.
const CLAIM_ONLY_SQL: &str = claim_sql!("AND id = ANY($4)");

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
    /// Names this claim: the claim sets it, and every write of the execution checks it.
    lease_id: Option<Uuid>,
    exhausted: bool,
    /// Just before the claim was sent. The database dates the lease from the
    /// claim, so the lease ends no sooner than this instant plus its duration.
    #[sqlx(skip)]
    claimed_at: Option<tokio::time::Instant>,
}

/// A run this worker executes: the claim it holds, the token that stops the
/// execution without recording an outcome, and the one it cancels once the
/// execution is over.
#[derive(Clone)]
struct Held {
    lease_id: Option<Uuid>,
    interrupt: CancellationToken,
    finished: CancellationToken,
}

struct Inner {
    app: OpenShop,
    config: ResolvedWorkerConfig,
    worker_id: String,
    /// The runs this worker may claim, when it is a worker of some runs.
    only: Option<Vec<Uuid>>,
    running: AtomicBool,
    active: AtomicUsize,
    /// Signalled when a run finishes, for a loop that waits for a free slot.
    slot_freed: Notify,
    wake: Mutex<CancellationToken>,
    loop_handle: Mutex<Option<JoinHandle<()>>>,
    /// Held by `start` and for the whole of `stop`: a start made while the
    /// worker stops waits for it, so the shutdown never hands back the runs
    /// of the loop that start begins.
    lifecycle: Mutex<()>,
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
        Self::of(app, overrides, None)
    }

    /// A worker that claims these runs only, as any worker would, retries
    /// included: what runs a flow by hand (`openshop run`, `openshop cron
    /// run`). It removes no log line past their retention.
    pub(crate) fn for_runs(app: OpenShop, overrides: WorkerConfig, runs: Vec<Uuid>) -> Self {
        Self::of(app, overrides, Some(runs))
    }

    fn of(app: OpenShop, overrides: WorkerConfig, only: Option<Vec<Uuid>>) -> Self {
        let config = merge(merge(DEFAULT_WORKER_CONFIG, &app.config().worker), &overrides);
        Self {
            inner: Arc::new(Inner {
                app,
                config,
                worker_id: Uuid::new_v4().to_string(),
                only,
                running: AtomicBool::new(false),
                active: AtomicUsize::new(0),
                slot_freed: Notify::new(),
                wake: Mutex::new(CancellationToken::new()),
                loop_handle: Mutex::new(None),
                lifecycle: Mutex::new(()),
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
        let _lifecycle = self.inner.lifecycle.lock().await;
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
            tokio::join!(
                inner.claim_loop(&stop),
                inner.expire_loop(&stop),
                inner.retention_loop(&stop)
            );
        }));
    }

    /// Stops claiming, then waits for active runs up to one lease duration.
    /// A run that is still executing then is handed back to the queue and its
    /// execution is stopped, so that another worker takes it up at once
    /// instead of when its lease expires. See [`Inner::release_held`].
    pub async fn stop(&self) {
        let _lifecycle = self.inner.lifecycle.lock().await;
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

    /// Removes the log lines past the `log_retention` setting once per sweep
    /// interval, if the application sets one.
    async fn retention_loop(&self, stop: &CancellationToken) {
        let Some(retention) = self.app.config().log_retention.filter(|_| self.only.is_none()) else {
            return;
        };
        let retention = chrono::Duration::from_std(retention).unwrap_or(chrono::Duration::MAX);
        while self.running.load(Ordering::SeqCst) {
            let cutoff = Utc::now()
                .checked_sub_signed(retention)
                .unwrap_or(DateTime::<Utc>::MIN_UTC);
            match purge_logs(self.app.db(), cutoff).await {
                Ok(0) => {}
                Ok(removed) => logger::info(format!("[openshop] Removed {removed} log lines past their retention")),
                Err(error) => logger::warn(format!("[openshop] Worker failed to remove old log lines: {error:#}")),
            }
            self.sleep(SWEEP_INTERVAL.as_millis() as u64, stop).await;
        }
    }

    /// Leases runs, and fails the ones that used all their attempts. Resolves
    /// to the runs to execute and to how many were failed.
    async fn claim(&self, limit: usize) -> Result<(Vec<ClaimedRun>, usize)> {
        let sent = tokio::time::Instant::now();
        let claim = match &self.only {
            Some(runs) => sqlx::query_as::<_, ClaimedRun>(CLAIM_ONLY_SQL)
                .bind(&self.worker_id)
                .bind(self.lease_ms())
                .bind(limit as i64)
                .bind(runs),
            None => sqlx::query_as::<_, ClaimedRun>(CLAIM_SQL)
                .bind(&self.worker_id)
                .bind(self.lease_ms())
                .bind(limit as i64),
        };
        let mut rows = claim.fetch_all(self.app.db()).await?;
        for row in &mut rows {
            row.claimed_at = Some(sent);
        }
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
        let finished = CancellationToken::new();
        let (run_id, lease_id) = (claimed.id, claimed.lease_id);
        self.held().insert(
            run_id,
            Held {
                lease_id,
                interrupt: interrupt.clone(),
                finished: finished.clone(),
            },
        );
        tokio::spawn(async move {
            self.execute(claimed, interrupt).await;
            finished.cancel();
            // Not if a newer claim of the same run, by this worker, replaced it.
            let mut held = self.held();
            if held.get(&run_id).is_some_and(|held| held.lease_id == lease_id) {
                held.remove(&run_id);
            }
            drop(held);
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.slot_freed.notify_one();
        });
    }

    /// Hands back the runs this worker still executes, once it stopped
    /// claiming and waited for them. Each goes back to `pending`, claimable
    /// at once.
    ///
    /// The executions are interrupted first: the runner drops the flow, with
    /// whatever it was doing, and records no outcome. A run is only released
    /// once its execution is gone, so that none goes on beside the worker that
    /// takes the run up. An execution that does not end within [`STOP_GRACE`]
    /// (blocking code never yields) is not released: its heartbeat has
    /// stopped, and the run is left to its lease.
    ///
    /// The release is fenced like every write of an execution, by worker and
    /// claim (`lease_id`, which it clears), so it cannot take a run another
    /// worker has claimed after its lease expired. It cannot block the
    /// shutdown: waiting for a connection and for a lock is bounded by
    /// [`RELEASE_TIMEOUT`], the releases run together, and one that fails is
    /// logged and left to its lease without holding back the others.
    ///
    /// The interrupted attempt stays counted in `attempts`: the claim is told
    /// apart by `lease_id`, but the step rows are still keyed by attempt and
    /// the interrupted one may have left a row, which the next claim must not reuse.
    /// A release is never an abandoned run, though: a run `pending` is claimed
    /// whatever its `attempts`, even with `max_attempts(1)`.
    async fn release_held(&self) {
        let held: Vec<(Uuid, Held)> = self.held().iter().map(|(run, held)| (*run, held.clone())).collect();
        if held.is_empty() {
            return;
        }
        for (_, held) in &held {
            held.interrupt.cancel();
        }
        let grace = tokio::time::Instant::now() + STOP_GRACE;
        let gone = futures::future::join_all(held.iter().map(|(run_id, held)| async move {
            if tokio::time::timeout_at(grace, held.finished.cancelled()).await.is_err() {
                logger::warn(format!(
                    "[openshop] Run {run_id} is still executing after {}ms and is left to its lease",
                    STOP_GRACE.as_millis()
                ));
                return None;
            }
            Some((*run_id, held.lease_id))
        }))
        .await;

        let releases = futures::future::join_all(gone.into_iter().flatten().map(|(run_id, lease_id)| async move {
            match self.release_run(run_id, lease_id).await {
                Ok(true) => logger::info(format!("[openshop] Released run {run_id} to the queue on shutdown")),
                Ok(false) => {}
                Err(error) => logger::warn(format!(
                    "[openshop] Run {run_id} could not be released, it is left to its lease: {error:#}"
                )),
            }
        }));
        // Each release is bounded; this is for the sum to be.
        if tokio::time::timeout(RELEASE_TIMEOUT * 2, releases).await.is_err() {
            logger::warn("[openshop] Some runs could not be released in time and are left to their leases");
        }
    }

    /// Gives the run back to the queue, if this worker still holds the claim.
    async fn release_run(&self, run_id: Uuid, lease_id: Option<Uuid>) -> Result<bool> {
        let statement = bounded(RELEASE_TIMEOUT, &release_sql(run_id, &self.worker_id, lease_id));
        let release = sqlx::raw_sql(sqlx::AssertSqlSafe(&*statement)).execute(self.app.db());
        match tokio::time::timeout(RELEASE_TIMEOUT, release).await {
            Ok(result) => Ok(result?.rows_affected() > 0),
            Err(_) => Err(anyhow::anyhow!("timed out after {}ms", RELEASE_TIMEOUT.as_millis())),
        }
    }

    /// Fails a run this worker still owns, under the claim it holds. A
    /// transient error is retried. If the write fails for good, the lease
    /// expires and the run is claimed again.
    async fn fail_run(&self, run_id: Uuid, lease_id: Option<Uuid>, error: String) {
        let failed = retry(format!("failing run {run_id}"), || async {
            sqlx::query(
                "UPDATE flow_runs SET status = 'failed', error = $2, worker_id = NULL, lease_id = NULL, \
                   completed_at = now() \
                 WHERE id = $1 AND worker_id = $3 AND status = 'running' AND lease_id IS NOT DISTINCT FROM $4",
            )
            .bind(run_id)
            .bind(&error)
            .bind(&self.worker_id)
            .bind(lease_id)
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
            self.fail_run(run.id, claimed.lease_id, run.error.clone()).await;
            report_failed_run(&run);
            return;
        }

        let input = match claimed.input {
            Some(Value::Object(input)) => Value::Object(input),
            _ => Value::Object(JsonMap::new()),
        };

        let heartbeat: HeartbeatFn = {
            let inner = self.clone();
            let (run_id, lease_id) = (claimed.id, claimed.lease_id);
            Arc::new(move || {
                let inner = inner.clone();
                Box::pin(async move { inner.extend_lease(run_id, lease_id).await })
            })
        };

        let mut options = RunFlowOptions::new(claimed.id, claimed.flow_name, claimed.shop)
            .input(input)
            .shopify_app(claimed.app_handle)
            .worker_id(self.worker_id.clone())
            .attempt(claimed.attempts)
            .lease_id(claimed.lease_id);
        options.claimed_at = claimed.claimed_at;
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
            Err(panic) => format!("panicked: {}", panic_message(&panic)),
        };
        self.fail_run(claimed.id, claimed.lease_id, format!("Worker error: {failure}")).await;
    }

    /// Extends the lease of a run, within the time it has to: a heartbeat that
    /// waits for a connection or for a lock longer than that is no use, as the
    /// lease it was meant to extend is about to end. Resolves to `false` when
    /// this execution no longer owns the run.
    ///
    /// The heartbeat shares the pool of the application. The time limit covers
    /// the wait for a connection when the pool is saturated by flows, and the
    /// server enforces it on the statement through `statement_timeout`.
    async fn extend_lease(&self, run_id: Uuid, lease_id: Option<Uuid>) -> Result<bool> {
        let limit = self.heartbeat_timeout();
        let statement = bounded(limit, &heartbeat_sql(self.lease_ms(), run_id, &self.worker_id, lease_id));
        let extend = sqlx::raw_sql(sqlx::AssertSqlSafe(&*statement)).execute(self.app.db());
        match tokio::time::timeout(limit, extend).await {
            Ok(result) => Ok(result?.rows_affected() > 0),
            Err(_) => Err(anyhow::anyhow!("timed out after {}ms", limit.as_millis())),
        }
    }

    /// A quarter of the lease, so that a heartbeat which times out leaves the
    /// next ones room to succeed before the lease ends.
    fn heartbeat_timeout(&self) -> Duration {
        Duration::from_millis(self.config.lease_duration_ms / 4).clamp(HEARTBEAT_MIN_TIMEOUT, HEARTBEAT_MAX_TIMEOUT)
    }

    fn backoff_delay(&self, consecutive: u32) -> u64 {
        let backoff_ms = compute_backoff_ms(
            self.config.poll_interval_ms,
            self.config.poll_backoff_coefficient,
            self.config.poll_max_interval_ms,
            consecutive.into(),
        );
        jitter_ms(backoff_ms, rand::rng().random())
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
        let mut query = sqlx::query_as(sqlx::AssertSqlSafe(&*explain));
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
