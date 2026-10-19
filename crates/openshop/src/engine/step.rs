//! Checkpointed steps: a completed step is never executed twice for a run.

use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::errors::{FlowCanceledError, LeaseLostError, SleepSignal, StepTimeoutError};
use super::transient::retry;
use crate::runtime::FlowLogger;
use crate::runtime::time::to_iso;
use crate::types::StepOptions;

/// Full error text including context, e.g. `while syncing: connection refused`.
pub(crate) fn error_message(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

/// Marks a step as failed when its future is dropped mid-run (flow timeout,
/// panic), so that it does not stay `running` forever.
struct InterruptedStepGuard {
    db: PgPool,
    step_id: Uuid,
    armed: bool,
}

impl Drop for InterruptedStepGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let db = self.db.clone();
        let step_id = self.step_id;
        runtime.spawn(async move {
            let _ = sqlx::query(
                "UPDATE step_results SET status = 'failed', error = 'Step was interrupted' \
                 WHERE id = $1 AND status = 'running'",
            )
            .bind(step_id)
            .execute(&db)
            .await;
        });
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
}

/// Who an execution is, when a worker leased the run for it. A claim of the
/// run bumps its `attempts`, except when it wakes up from a sleep. So the
/// worker and the attempt tell this execution from every other one, including
/// an earlier one of the same worker whose lease expired.
#[derive(Debug, Clone)]
pub struct RunOwner {
    pub worker_id: String,
    /// The attempt the claim returned. Any attempt matches when unset.
    pub attempt: Option<i32>,
}

/// The condition for a write to apply: the run is still leased to the
/// execution. The arguments are the numbers of the parameters that hold the run
/// ID, the worker and the attempt of the owner; without an owner the last two
/// are `NULL` and the condition always holds.
///
/// The status of the run is not part of it: a run canceled from the API keeps
/// its worker, and its steps still record that they were canceled. Whoever
/// takes a run over, or finishes it, changes the worker or the attempt.
///
/// The row of the run is locked `FOR SHARE` until the statement ends. Reading
/// it is not enough: the statement may wait for another one that holds the row
/// it writes, and the run could be taken over in the meantime. A claim, a
/// heartbeat or the end of the run wait for the write instead (or, for a claim,
/// skip the run). The lock on the run is always taken before the one on the
/// step, and no statement takes them the other way round.
fn owned(run: u8, worker: u8, attempt: u8) -> String {
    format!(
        "(${worker}::text IS NULL OR EXISTS (SELECT 1 FROM flow_runs \
           WHERE id = ${run}::uuid AND worker_id = ${worker} \
             AND attempts = COALESCE(${attempt}::int, attempts) FOR SHARE))"
    )
}

#[derive(Clone)]
pub struct StepExecutor {
    db: PgPool,
    flow_run_id: String,
    logger: FlowLogger,
    signal: CancellationToken,
    default_timeout: Option<Duration>,
    attempt: i32,
    owner: Option<RunOwner>,
}

impl StepExecutor {
    /// With an `owner`, a step only writes while the run is still owned by it:
    /// once another execution took the run over, the step fails with
    /// [`LeaseLostError`], stops the flow and stores nothing.
    pub fn new(
        db: PgPool,
        flow_run_id: impl Into<String>,
        logger: FlowLogger,
        signal: Option<CancellationToken>,
        default_timeout: Option<Duration>,
        attempt: i32,
        owner: Option<RunOwner>,
    ) -> Self {
        Self {
            db,
            flow_run_id: flow_run_id.into(),
            logger,
            signal: signal.unwrap_or_default(),
            default_timeout,
            attempt,
            owner,
        }
    }

    /// Stores the state of a step, if the run is still owned. A step that
    /// finds it is not stops the flow: it belongs to another execution now.
    /// Only the columns given are changed.
    async fn store(
        &self,
        step_id: Uuid,
        status: &str,
        output: Option<String>,
        error: Option<&str>,
        duration_ms: i32,
    ) -> Result<()> {
        let sql = format!(
            "UPDATE step_results SET status = $1, output = COALESCE($2::json, output), \
               error = COALESCE($3, error), duration_ms = $4 \
             WHERE id = $5 AND {}",
            owned(6, 7, 8)
        );
        let written = retry(format!("storing the state of step {step_id}"), || async {
            let result = sqlx::query(&sql)
                .bind(status)
                .bind(&output)
                .bind(error)
                .bind(duration_ms)
                .bind(step_id)
                .bind(&self.flow_run_id)
                .bind(self.owner.as_ref().map(|owner| owner.worker_id.as_str()))
                .bind(self.owner.as_ref().and_then(|owner| owner.attempt))
                .execute(&self.db)
                .await?;
            Ok(result.rows_affected() > 0)
        })
        .await?;
        self.require(written)
    }

    /// Whether the row `id` is there. It was written by an earlier try of an
    /// insert whose answer was lost: `id` is generated for the insert and
    /// belongs to nobody else.
    async fn is_stored(&self, id: Uuid) -> bool {
        sqlx::query_scalar::<_, i32>("SELECT 1 FROM step_results WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.db)
            .await
            .is_ok_and(|row| row.is_some())
    }

    /// Fails when a write was refused because the run is not owned any more.
    fn require(&self, written: bool) -> Result<()> {
        if written || self.owner.is_none() {
            return Ok(());
        }
        Err(self.lost())
    }

    /// The run belongs to another execution: stop the flow.
    fn lost(&self) -> anyhow::Error {
        self.signal.cancel();
        anyhow!(LeaseLostError)
    }

    /// Runs `f` once and stores its output. On a retried run the stored output
    /// is returned instead of running `f` again.
    pub async fn run<T, F, Fut>(&self, name: &str, f: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        self.run_with(name, StepOptions::default(), f).await
    }

    pub async fn run_with<T, F, Fut>(&self, name: &str, options: StepOptions, f: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        if self.signal.is_cancelled() {
            bail!(FlowCanceledError);
        }

        // Deterministic replay: return cached result
        let existing: Option<(Option<Value>,)> = sqlx::query_as(
            "SELECT output FROM step_results \
             WHERE flow_run_id = $1 AND step_name = $2 AND status = 'completed' LIMIT 1",
        )
        .bind(&self.flow_run_id)
        .bind(name)
        .fetch_optional(&self.db)
        .await?;

        if let Some((output,)) = existing {
            self.logger
                .info_with(format!("Step \"{name}\" skipped (cached)"), json!({ "step": name }));
            return Ok(serde_json::from_value(output.unwrap_or(Value::Null))?);
        }

        self.logger
            .info_with(format!("Step \"{name}\" started"), json!({ "step": name }));

        // The ID is chosen before the first try: if that one commits and its
        // answer is lost, the next one finds the row and knows it is its own.
        let new_id = Uuid::new_v4();
        let insert = format!(
            "INSERT INTO step_results (id, flow_run_id, step_name, attempt, status) \
             SELECT $6::uuid, $1::text, $2::text, $3::int, 'running' WHERE {} RETURNING id",
            owned(1, 4, 5)
        );
        let inserted: Result<Option<(Uuid,)>> = retry(format!("starting step \"{name}\""), || async {
            let result = sqlx::query_as(&insert)
                .bind(&self.flow_run_id)
                .bind(name)
                .bind(self.attempt)
                .bind(self.owner.as_ref().map(|owner| owner.worker_id.as_str()))
                .bind(self.owner.as_ref().and_then(|owner| owner.attempt))
                .bind(new_id)
                .fetch_optional(&self.db)
                .await;
            match result {
                Err(error) if is_unique_violation(&error) && self.is_stored(new_id).await => Ok(Some((new_id,))),
                other => Ok(other?),
            }
        })
        .await;

        let step_id = match inserted {
            Ok(Some((id,))) => id,
            Ok(None) => return Err(self.lost()),
            Err(insert_error) => {
                // The step already ran in this attempt: reuse its outcome instead of running twice.
                let current: Option<(String, Option<Value>, Option<String>)> = sqlx::query_as(
                    "SELECT status, output, error FROM step_results \
                     WHERE flow_run_id = $1 AND step_name = $2 AND attempt = $3 LIMIT 1",
                )
                .bind(&self.flow_run_id)
                .bind(name)
                .bind(self.attempt)
                .fetch_optional(&self.db)
                .await?;

                return match current {
                    Some((status, output, _)) if status == "completed" => {
                        Ok(serde_json::from_value(output.unwrap_or(Value::Null))?)
                    }
                    Some((status, _, error)) if status == "failed" => Err(anyhow!(
                        error.unwrap_or_else(|| format!("Step \"{name}\" already failed in attempt {}", self.attempt))
                    )),
                    _ => Err(insert_error),
                };
            }
        };

        let mut interrupted = InterruptedStepGuard {
            db: self.db.clone(),
            step_id,
            armed: true,
        };
        let start = Instant::now();
        let timeout = options.timeout.or(self.default_timeout);
        let elapsed_ms = |start: Instant| start.elapsed().as_millis().min(i32::MAX as u128) as i32;

        let work = async {
            match timeout {
                Some(timeout) => match tokio::time::timeout(timeout, f()).await {
                    Ok(result) => result,
                    Err(_) => Err(anyhow!(StepTimeoutError {
                        step_name: name.to_string(),
                        timeout_ms: timeout.as_millis() as u64,
                    })),
                },
                None => f().await,
            }
        };

        let outcome = tokio::select! {
            biased;
            _ = self.signal.cancelled() => Err(anyhow!(FlowCanceledError)),
            result = work => result,
        };
        let outcome = match outcome {
            Ok(_) if self.signal.is_cancelled() => Err(anyhow!(FlowCanceledError)),
            other => other,
        };
        // The step ran to an outcome: it is recorded below.
        interrupted.armed = false;

        match outcome {
            Ok(result) => {
                let duration_ms = elapsed_ms(start);
                let output = serde_json::to_value(&result)?;
                self.store(step_id, "completed", Some(output.to_string()), None, duration_ms)
                    .await?;

                self.logger.info_with(
                    format!("Step \"{name}\" completed ({duration_ms}ms)"),
                    json!({ "step": name, "durationMs": duration_ms }),
                );
                Ok(result)
            }
            Err(error) => {
                let duration_ms = elapsed_ms(start);

                if error.downcast_ref::<FlowCanceledError>().is_some() {
                    self.store(step_id, "canceled", None, None, duration_ms).await?;
                    return Err(error);
                }

                let message = error_message(&error);
                self.store(step_id, "failed", None, Some(&message), duration_ms).await?;
                self.logger.info_with(
                    format!("Step \"{name}\" failed ({duration_ms}ms)"),
                    json!({ "step": name, "durationMs": duration_ms, "error": message }),
                );
                Err(error)
            }
        }
    }

    /// Suspends the run without holding a worker. The run resumes after `duration`
    /// and this call then returns immediately.
    pub async fn sleep(&self, name: &str, duration: Duration) -> Result<()> {
        if self.signal.is_cancelled() {
            bail!(FlowCanceledError);
        }

        let completed: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM step_results \
             WHERE flow_run_id = $1 AND step_name = $2 AND status = 'completed' LIMIT 1",
        )
        .bind(&self.flow_run_id)
        .bind(name)
        .fetch_optional(&self.db)
        .await?;
        if completed.is_some() {
            return Ok(());
        }

        let sleeping: Option<(Uuid, Option<Value>)> = sqlx::query_as(
            "SELECT id, output FROM step_results \
             WHERE flow_run_id = $1 AND step_name = $2 AND status = 'sleeping' LIMIT 1",
        )
        .bind(&self.flow_run_id)
        .bind(name)
        .fetch_optional(&self.db)
        .await?;

        let duration_ms = duration.as_millis().min(i32::MAX as u128) as i32;
        let resume_at = Utc::now() + chrono::Duration::milliseconds(duration.as_millis() as i64);

        if let Some((id, output)) = sleeping {
            let stored_resume_at = output
                .as_ref()
                .and_then(Value::as_str)
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                .map(|date| date.with_timezone(&Utc))
                .unwrap_or(resume_at);
            if stored_resume_at <= Utc::now() {
                self.store(id, "completed", None, None, duration_ms).await?;
                return Ok(());
            }
            bail!(SleepSignal {
                resume_at: stored_resume_at
            });
        }

        let new_id = Uuid::new_v4();
        let insert = format!(
            "INSERT INTO step_results (id, flow_run_id, step_name, attempt, status, output) \
             SELECT $7::uuid, $1::text, $2::text, $3::int, 'sleeping', $4::json WHERE {}",
            owned(1, 5, 6)
        );
        let inserted = retry(format!("suspending at step \"{name}\""), || async {
            let result = sqlx::query(&insert)
                .bind(&self.flow_run_id)
                .bind(name)
                .bind(self.attempt)
                .bind(Value::from(to_iso(&resume_at)).to_string())
                .bind(self.owner.as_ref().map(|owner| owner.worker_id.as_str()))
                .bind(self.owner.as_ref().and_then(|owner| owner.attempt))
                .bind(new_id)
                .execute(&self.db)
                .await;
            match result {
                Err(error) if is_unique_violation(&error) && self.is_stored(new_id).await => Ok(true),
                other => Ok(other?.rows_affected() > 0),
            }
        })
        .await?;
        self.require(inserted)?;
        bail!(SleepSignal { resume_at })
    }
}
