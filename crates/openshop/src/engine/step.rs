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

use super::errors::{FlowCanceledError, SleepSignal, StepTimeoutError};
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

#[derive(Clone)]
pub struct StepExecutor {
    db: PgPool,
    flow_run_id: String,
    logger: FlowLogger,
    signal: CancellationToken,
    default_timeout: Option<Duration>,
    attempt: i32,
}

impl StepExecutor {
    pub fn new(
        db: PgPool,
        flow_run_id: impl Into<String>,
        logger: FlowLogger,
        signal: Option<CancellationToken>,
        default_timeout: Option<Duration>,
        attempt: i32,
    ) -> Self {
        Self {
            db,
            flow_run_id: flow_run_id.into(),
            logger,
            signal: signal.unwrap_or_default(),
            default_timeout,
            attempt,
        }
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

        let inserted: Result<(Uuid,), sqlx::Error> = sqlx::query_as(
            "INSERT INTO step_results (flow_run_id, step_name, attempt, status) \
             VALUES ($1, $2, $3, 'running') RETURNING id",
        )
        .bind(&self.flow_run_id)
        .bind(name)
        .bind(self.attempt)
        .fetch_one(&self.db)
        .await;

        let step_id = match inserted {
            Ok((id,)) => id,
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
                    _ => Err(insert_error.into()),
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
                sqlx::query(
                    "UPDATE step_results SET status = 'completed', output = $1::json, duration_ms = $2 WHERE id = $3",
                )
                .bind(output.to_string())
                .bind(duration_ms)
                .bind(step_id)
                .execute(&self.db)
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
                    sqlx::query("UPDATE step_results SET status = 'canceled', duration_ms = $1 WHERE id = $2")
                        .bind(duration_ms)
                        .bind(step_id)
                        .execute(&self.db)
                        .await?;
                    return Err(error);
                }

                let message = error_message(&error);
                sqlx::query("UPDATE step_results SET status = 'failed', error = $1, duration_ms = $2 WHERE id = $3")
                    .bind(&message)
                    .bind(duration_ms)
                    .bind(step_id)
                    .execute(&self.db)
                    .await?;
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
                sqlx::query("UPDATE step_results SET status = 'completed', duration_ms = $1 WHERE id = $2")
                    .bind(duration_ms)
                    .bind(id)
                    .execute(&self.db)
                    .await?;
                return Ok(());
            }
            bail!(SleepSignal {
                resume_at: stored_resume_at
            });
        }

        sqlx::query(
            "INSERT INTO step_results (flow_run_id, step_name, attempt, status, output) \
             VALUES ($1, $2, $3, 'sleeping', $4::json)",
        )
        .bind(&self.flow_run_id)
        .bind(name)
        .bind(self.attempt)
        .bind(Value::from(to_iso(&resume_at)).to_string())
        .execute(&self.db)
        .await?;
        bail!(SleepSignal { resume_at })
    }
}
