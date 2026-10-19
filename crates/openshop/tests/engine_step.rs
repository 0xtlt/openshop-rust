mod common;

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use common::{TEST_SHOP, TestApp, create_config, inject_write_fault, noop_flow, write_fault_hits};
use openshop::db::schema::StepResult;
use openshop::engine::{LeaseLostError, RunOwner, SleepSignal, StepExecutor};
use openshop::runtime::time::to_iso;
use openshop::{
    CancellationToken, Context, FlowCanceledError, FlowLogger, StepOptions, StepTimeoutError, anyhow, bail, json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use uuid::Uuid;

async fn new_test() -> TestApp {
    TestApp::new(create_config([noop_flow("test-flow")])).await
}

async fn create_run(test: &TestApp) -> Uuid {
    let (run_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO flow_runs (shop, flow_name, status) VALUES ($1, 'test-flow', 'running') RETURNING id",
    )
    .bind(TEST_SHOP)
    .fetch_one(&test.db)
    .await
    .unwrap();
    run_id
}

fn executor_with(
    test: &TestApp,
    run_id: Uuid,
    signal: Option<CancellationToken>,
    default_timeout: Option<Duration>,
    attempt: i32,
) -> StepExecutor {
    let logger = FlowLogger::new(test.db.clone(), run_id.to_string());
    StepExecutor::new(
        test.db.clone(),
        run_id.to_string(),
        logger,
        signal,
        default_timeout,
        attempt,
        None,
    )
}

fn executor(test: &TestApp, run_id: Uuid) -> StepExecutor {
    executor_with(test, run_id, None, None, 1)
}

async fn find_steps(test: &TestApp, run_id: Uuid) -> Vec<StepResult> {
    sqlx::query_as("SELECT * FROM step_results WHERE flow_run_id = $1 ORDER BY created_at, attempt")
        .bind(run_id.to_string())
        .fetch_all(&test.db)
        .await
        .unwrap()
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("condition not reached in time")
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Fetched {
    items: Vec<u32>,
}

#[tokio::test]
async fn executes_step_and_stores_result_in_db() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let step = executor(&test, run_id);

    let result: Fetched = step
        .run("fetch-data", || async { Ok(Fetched { items: vec![1, 2, 3] }) })
        .await
        .unwrap();
    assert_eq!(result, Fetched { items: vec![1, 2, 3] });

    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].flow_run_id, run_id.to_string());
    assert_eq!(stored[0].step_name, "fetch-data");
    assert_eq!(stored[0].status, "completed");
    assert_eq!(stored[0].attempt, 1);
    assert_eq!(stored[0].output, Some(json!({ "items": [1, 2, 3] })));
    assert_eq!(stored[0].error, None);
    assert!(stored[0].duration_ms.is_some());
    test.cleanup().await;
}

#[tokio::test]
async fn deterministic_replay_returns_cached_output() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let step = executor(&test, run_id);

    let calls = AtomicUsize::new(0);
    let compute = || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(42)
    };

    let first: i32 = step.run("compute", compute).await.unwrap();
    let second: i32 = step.run("compute", compute).await.unwrap();

    assert_eq!(first, 42);
    assert_eq!(second, 42);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(find_steps(&test, run_id).await.len(), 1);
    test.cleanup().await;
}

#[tokio::test]
async fn deterministic_replay_skips_completed_step_with_null_output() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let step = executor(&test, run_id);

    let calls = AtomicUsize::new(0);
    let compute = || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    };
    let first: Option<i32> = step.run("compute-null", compute).await.unwrap();
    let second: Option<i32> = step.run("compute-null", compute).await.unwrap();
    assert_eq!(first, None);
    assert_eq!(second, None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A step without a result is checkpointed the same way.
    let effect = || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    step.run("side-effect", effect).await.unwrap();
    step.run("side-effect", effect).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    test.cleanup().await;
}

#[tokio::test]
async fn step_timeout_returns_step_timeout_error() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let step = executor(&test, run_id);

    let error = step
        .run_with("slow", StepOptions::timeout(Duration::from_millis(50)), || async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(())
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<StepTimeoutError>(),
        Some(&StepTimeoutError {
            step_name: "slow".to_string(),
            timeout_ms: 50
        })
    );

    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored[0].status, "failed");
    assert_eq!(stored[0].error.as_deref(), Some("Step \"slow\" timed out after 50ms"));
    assert!(stored[0].duration_ms.is_some_and(|duration| duration >= 50));
    test.cleanup().await;
}

#[tokio::test]
async fn default_timeout_applies_unless_the_step_sets_its_own() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let step = executor_with(&test, run_id, None, Some(Duration::from_millis(40)), 1);
    let nap = || async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(())
    };

    let error = step.run("default", nap).await.unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<StepTimeoutError>()
            .map(|timeout| timeout.timeout_ms),
        Some(40)
    );

    step.run_with("patient", StepOptions::timeout(Duration::from_secs(10)), nap)
        .await
        .unwrap();
    test.cleanup().await;
}

#[tokio::test]
async fn cancellation_via_token_returns_flow_canceled_error() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let signal = CancellationToken::new();
    let step = executor_with(&test, run_id, Some(signal.clone()), None, 1);

    signal.cancel();

    let calls = AtomicUsize::new(0);
    let error = step
        .run("should-not-run", || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok("nope".to_string())
        })
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<FlowCanceledError>().is_some());
    assert_eq!(error.to_string(), "Flow was canceled");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let error = step
        .sleep("should-not-sleep", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<FlowCanceledError>().is_some());
    assert!(find_steps(&test, run_id).await.is_empty());
    test.cleanup().await;
}

#[tokio::test]
async fn cancellation_interrupts_an_in_flight_step() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let signal = CancellationToken::new();
    let step = executor_with(&test, run_id, Some(signal.clone()), None, 1);

    let started = Arc::new(Notify::new());
    let running = tokio::spawn({
        let (step, started) = (step.clone(), started.clone());
        async move {
            step.run("slow", || async move {
                started.notify_one();
                std::future::pending::<()>().await;
                Ok(())
            })
            .await
        }
    });
    within(started.notified()).await;
    signal.cancel();

    let error = within(running).await.unwrap().unwrap_err();
    assert!(error.downcast_ref::<FlowCanceledError>().is_some());

    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, "canceled");
    assert_eq!(stored[0].output, None);
    assert_eq!(stored[0].error, None);
    assert!(stored[0].duration_ms.is_some());
    test.cleanup().await;
}

#[tokio::test]
async fn result_of_a_step_finishing_after_cancellation_is_discarded() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let signal = CancellationToken::new();
    let step = executor_with(&test, run_id, Some(signal.clone()), None, 1);

    let error = step
        .run("finishing", || async {
            signal.cancel();
            Ok("too late".to_string())
        })
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<FlowCanceledError>().is_some());

    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored[0].status, "canceled");
    assert_eq!(stored[0].output, None);
    test.cleanup().await;
}

#[tokio::test]
async fn step_records_duration_ms() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let step = executor(&test, run_id);

    let done: String = step
        .run("quick", || async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok("done".to_string())
        })
        .await
        .unwrap();
    assert_eq!(done, "done");

    let stored = find_steps(&test, run_id).await;
    assert!(
        stored[0].duration_ms.is_some_and(|duration| duration > 10),
        "{:?}",
        stored[0].duration_ms
    );
    test.cleanup().await;
}

#[tokio::test]
async fn sleep_raises_sleep_signal_on_first_call() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let step = executor(&test, run_id);
    let before = Utc::now();

    let error = step.sleep("wait-a-bit", Duration::from_secs(60)).await.unwrap_err();
    let resume_at = error.downcast_ref::<SleepSignal>().expect("sleep signal").resume_at;
    assert!(resume_at >= before + chrono::Duration::seconds(60));
    assert!(resume_at <= Utc::now() + chrono::Duration::seconds(60));
    assert_eq!(error.to_string(), format!("Flow sleeping until {}", to_iso(&resume_at)));

    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].step_name, "wait-a-bit");
    assert_eq!(stored[0].status, "sleeping");
    assert_eq!(stored[0].attempt, 1);
    assert_eq!(stored[0].output, Some(json!(to_iso(&resume_at))));
    test.cleanup().await;
}

#[tokio::test]
async fn sleep_keeps_its_first_resume_time_until_it_has_passed() {
    let test = new_test().await;
    let run_id = create_run(&test).await;

    let error = executor(&test, run_id)
        .sleep("pause", Duration::from_secs(60))
        .await
        .unwrap_err();
    let resume_at = error.downcast_ref::<SleepSignal>().expect("sleep signal").resume_at;

    // A later attempt asking again does not push the resume time back, nor pull it in.
    let retry = executor_with(&test, run_id, None, None, 2);
    for duration in [Duration::from_secs(3_600), Duration::from_millis(1)] {
        let error = retry.sleep("pause", duration).await.unwrap_err();
        let again = error.downcast_ref::<SleepSignal>().expect("sleep signal").resume_at;
        assert_eq!(to_iso(&again), to_iso(&resume_at));
    }
    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, "sleeping");

    let past = to_iso(&(Utc::now() - chrono::Duration::seconds(1)));
    sqlx::query("UPDATE step_results SET output = to_json($1::text) WHERE flow_run_id = $2")
        .bind(past)
        .bind(run_id.to_string())
        .execute(&test.db)
        .await
        .unwrap();

    retry.sleep("pause", Duration::from_secs(60)).await.unwrap();
    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, "completed");
    assert_eq!(stored[0].duration_ms, Some(60_000));

    // Once completed, the sleep is skipped like any other checkpoint.
    retry.sleep("pause", Duration::from_secs(60)).await.unwrap();
    assert_eq!(find_steps(&test, run_id).await.len(), 1);
    test.cleanup().await;
}

#[tokio::test]
async fn sleep_returns_once_the_duration_has_elapsed() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let step = executor(&test, run_id);

    let error = step.sleep("blink", Duration::from_millis(40)).await.unwrap_err();
    let resume_at = error.downcast_ref::<SleepSignal>().expect("sleep signal").resume_at;

    let remaining = (resume_at - Utc::now()).to_std().unwrap_or_default();
    tokio::time::sleep(remaining + Duration::from_millis(5)).await;

    step.sleep("blink", Duration::from_millis(40)).await.unwrap();
    assert_eq!(find_steps(&test, run_id).await[0].status, "completed");
    test.cleanup().await;
}

#[tokio::test]
async fn step_with_failed_status_is_fenced_within_the_same_attempt() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let step = executor(&test, run_id);

    let error = step
        .run::<String, _, _>("flaky", || async { bail!("fail-1") })
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "fail-1");

    // A second call in the same attempt must not execute the step again.
    let calls = AtomicUsize::new(0);
    let error = step
        .run("flaky", || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        })
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "fail-1");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(find_steps(&test, run_id).await.len(), 1);
    test.cleanup().await;
}

#[tokio::test]
async fn step_with_failed_status_can_run_again_in_a_new_attempt() {
    let test = new_test().await;
    let run_id = create_run(&test).await;

    let attempt_one = executor_with(&test, run_id, None, None, 1);
    attempt_one
        .run::<String, _, _>("flaky", || async { bail!("fail-1") })
        .await
        .unwrap_err();

    let attempt_two = executor_with(&test, run_id, None, None, 2);
    let result: String = attempt_two
        .run("flaky", || async { Ok("ok".to_string()) })
        .await
        .unwrap();
    assert_eq!(result, "ok");

    let stored: Vec<(i32, String)> = find_steps(&test, run_id)
        .await
        .into_iter()
        .map(|step| (step.attempt, step.status))
        .collect();
    assert_eq!(stored, [(1, "failed".to_string()), (2, "completed".to_string())]);
    test.cleanup().await;
}

#[tokio::test]
async fn completed_step_is_replayed_in_later_attempts() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let calls = AtomicUsize::new(0);
    let fetch = || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Fetched { items: vec![7, 8] })
    };

    let first: Fetched = executor_with(&test, run_id, None, None, 1)
        .run("fetch", fetch)
        .await
        .unwrap();
    let replayed: Fetched = executor_with(&test, run_id, None, None, 2)
        .run("fetch", fetch)
        .await
        .unwrap();

    assert_eq!(first, replayed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].attempt, 1);
    test.cleanup().await;
}

#[tokio::test]
async fn checkpoints_are_scoped_to_their_run() {
    let test = new_test().await;
    let (run_a, run_b) = (create_run(&test).await, create_run(&test).await);

    let a: String = executor(&test, run_a)
        .run("compute", || async { Ok("from a".to_string()) })
        .await
        .unwrap();
    let b: String = executor(&test, run_b)
        .run("compute", || async { Ok("from b".to_string()) })
        .await
        .unwrap();

    assert_eq!((a.as_str(), b.as_str()), ("from a", "from b"));
    assert_eq!(find_steps(&test, run_b).await[0].output, Some(json!("from b")));
    test.cleanup().await;
}

#[tokio::test]
async fn failed_step_stores_the_error_with_its_context() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let step = executor(&test, run_id);

    let error = step
        .run::<(), _, _>("sync", || async {
            Err(anyhow!("connection refused")).context("while syncing products")
        })
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "while syncing products");

    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored[0].status, "failed");
    assert_eq!(
        stored[0].error.as_deref(),
        Some("while syncing products: connection refused")
    );
    assert_eq!(stored[0].output, None);
    assert!(stored[0].duration_ms.is_some());
    test.cleanup().await;
}

#[tokio::test]
async fn logs_the_lifecycle_of_each_step() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let logger = FlowLogger::new(test.db.clone(), run_id.to_string());
    let step = StepExecutor::new(test.db.clone(), run_id.to_string(), logger.clone(), None, None, 1, None);

    for _ in 0..2 {
        let _: u32 = step.run("load", || async { Ok(3) }).await.unwrap();
    }
    step.run::<(), _, _>("save", || async { bail!("disk full") })
        .await
        .unwrap_err();
    logger.flush().await;

    let logs: Vec<(String, Option<String>, Option<openshop::Value>)> =
        sqlx::query_as("SELECT level, message, payload FROM logs WHERE flow_run_id = $1 ORDER BY created_at")
            .bind(run_id.to_string())
            .fetch_all(&test.db)
            .await
            .unwrap();
    assert!(logs.iter().all(|(level, _, _)| level == "info"));
    let messages: Vec<&str> = logs.iter().filter_map(|(_, message, _)| message.as_deref()).collect();
    assert_eq!(messages.len(), 5, "{messages:?}");
    assert_eq!(messages[0], "Step \"load\" started");
    assert!(messages[1].starts_with("Step \"load\" completed ("), "{messages:?}");
    assert_eq!(messages[2], "Step \"load\" skipped (cached)");
    assert_eq!(messages[3], "Step \"save\" started");
    assert!(messages[4].starts_with("Step \"save\" failed ("), "{messages:?}");

    let failure = logs[4].2.as_ref().expect("payload");
    assert_eq!(failure["step"], "save");
    assert_eq!(failure["error"], "disk full");
    assert!(failure["durationMs"].is_number());
    test.cleanup().await;
}

/// The lines of a run with the transaction that wrote each of them: rows
/// inserted by one statement share it.
async fn find_log_writes(test: &TestApp, run_id: Uuid) -> Vec<(String, String, String)> {
    sqlx::query_as("SELECT level, message, xmin::text FROM logs WHERE flow_run_id = $1 ORDER BY created_at, ctid")
        .bind(run_id.to_string())
        .fetch_all(&test.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn writes_the_lines_logged_together_in_one_statement() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let logger = FlowLogger::new(test.db.clone(), run_id.to_string());

    logger.info_with("first", json!({ "position": 1 }));
    logger.warn_with("second", json!({ "position": 2 }));
    logger.error_with("third", json!({ "position": 3 }));
    logger.flush().await;

    let writes = find_log_writes(&test, run_id).await;
    let lines: Vec<(&str, &str)> = writes
        .iter()
        .map(|(level, message, _)| (level.as_str(), message.as_str()))
        .collect();
    assert_eq!(lines, [("info", "first"), ("warn", "second"), ("error", "third")]);
    assert!(
        writes.iter().all(|(_, _, statement)| *statement == writes[0].2),
        "{writes:?}"
    );

    let payloads: Vec<(openshop::Value,)> =
        sqlx::query_as("SELECT payload FROM logs WHERE flow_run_id = $1 ORDER BY created_at, ctid")
            .bind(run_id.to_string())
            .fetch_all(&test.db)
            .await
            .unwrap();
    let positions: Vec<&openshop::Value> = payloads.iter().map(|(payload,)| &payload["position"]).collect();
    assert_eq!(positions, [&json!(1), &json!(2), &json!(3)]);
    test.cleanup().await;
}

// `payload` is a `json` column: it keeps the text it is given. That text is the
// one the TypeScript runtime writes, `JSON.stringify(payload)`.
#[tokio::test]
async fn stores_a_payload_as_compact_json_with_its_keys_in_order() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let logger = FlowLogger::new(test.db.clone(), run_id.to_string());

    logger.info_with("summary", json!({ "revenueCents": 1200, "count": 3, "best": null }));
    logger.info("bare");
    logger.flush().await;

    let payloads: Vec<(String,)> =
        sqlx::query_as("SELECT payload::text FROM logs WHERE flow_run_id = $1 ORDER BY created_at, ctid")
            .bind(run_id.to_string())
            .fetch_all(&test.db)
            .await
            .unwrap();
    let payloads: Vec<&str> = payloads.iter().map(|(payload,)| payload.as_str()).collect();
    assert_eq!(payloads, [r#"{"revenueCents":1200,"count":3,"best":null}"#, "{}"]);
    test.cleanup().await;
}

#[tokio::test]
async fn writes_a_line_without_waiting_for_the_end_of_the_run() {
    let test = new_test().await;
    let run_id = create_run(&test).await;
    let logger = FlowLogger::new(test.db.clone(), run_id.to_string());

    // Nothing flushes: the line is written on its own, after a short delay.
    logger.info("still running");
    let deadline = Instant::now() + Duration::from_secs(10);
    while find_log_writes(&test, run_id).await.is_empty() {
        assert!(Instant::now() < deadline, "the line was not written");
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    logger.info("done");
    logger.flush().await;

    let writes = find_log_writes(&test, run_id).await;
    let messages: Vec<&str> = writes.iter().map(|(_, message, _)| message.as_str()).collect();
    assert_eq!(messages, ["still running", "done"]);
    assert_ne!(writes[0].2, writes[1].2, "the second line has its own statement");
    test.cleanup().await;
}

/// A run leased by `worker-a` at its first attempt, and a step executor that
/// acts for that claim.
async fn owned_executor(test: &TestApp) -> (Uuid, StepExecutor, CancellationToken) {
    let run_id = create_run(test).await;
    sqlx::query("UPDATE flow_runs SET worker_id = 'worker-a', attempts = 1 WHERE id = $1")
        .bind(run_id)
        .execute(&test.db)
        .await
        .unwrap();
    let (step, signal) = owner_of(test, run_id);
    (run_id, step, signal)
}

/// An execution of `worker-a` at attempt 1, with its own cancellation signal.
fn owner_of(test: &TestApp, run_id: Uuid) -> (StepExecutor, CancellationToken) {
    let signal = CancellationToken::new();
    let step = StepExecutor::new(
        test.db.clone(),
        run_id.to_string(),
        FlowLogger::new(test.db.clone(), run_id.to_string()),
        Some(signal.clone()),
        None,
        1,
        Some(RunOwner {
            worker_id: "worker-a".to_string(),
            attempt: Some(1),
        }),
    );
    (step, signal)
}

/// Another execution takes the run over.
async fn take_over(test: &TestApp, run_id: Uuid) {
    sqlx::query("UPDATE flow_runs SET attempts = 2 WHERE id = $1")
        .bind(run_id)
        .execute(&test.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_owner_stores_its_steps_while_it_holds_the_run() {
    let test = new_test().await;
    let (run_id, step, signal) = owned_executor(&test).await;

    let _: u32 = step.run("load", || async { Ok(3) }).await.unwrap();
    step.run::<(), _, _>("save", || async { bail!("disk full") })
        .await
        .unwrap_err();
    step.sleep("pause", Duration::from_secs(60)).await.unwrap_err();

    let stored: Vec<(String, String)> = find_steps(&test, run_id)
        .await
        .into_iter()
        .map(|step| (step.step_name, step.status))
        .collect();
    assert_eq!(
        stored,
        [("load", "completed"), ("save", "failed"), ("pause", "sleeping")]
            .map(|(name, status)| (name.to_string(), status.to_string()))
    );
    assert!(!signal.is_cancelled());
    test.cleanup().await;
}

#[tokio::test]
async fn a_step_of_an_execution_that_lost_its_run_stores_nothing() {
    let test = new_test().await;
    let (run_id, step, signal) = owned_executor(&test).await;

    // The step starts, then the run is taken over while it works.
    let error = step
        .run("charge", || async {
            take_over(&test, run_id).await;
            Ok(1)
        })
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<LeaseLostError>().is_some(), "{error:#}");
    assert!(signal.is_cancelled(), "the flow is told to stop");
    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, "running", "the result was not stored");
    assert_eq!(stored[0].output, None);

    // Neither does a step that starts afterwards, nor a sleep.
    let (step, _) = owner_of(&test, run_id);
    let error = step.run("notify", || async { Ok(2) }).await.unwrap_err();
    assert!(error.downcast_ref::<LeaseLostError>().is_some(), "{error:#}");
    let (step, _) = owner_of(&test, run_id);
    let error = step.sleep("pause", Duration::from_secs(60)).await.unwrap_err();
    assert!(error.downcast_ref::<LeaseLostError>().is_some(), "{error:#}");
    assert_eq!(find_steps(&test, run_id).await.len(), 1);
    test.cleanup().await;
}

#[tokio::test]
async fn retries_storing_a_step_after_a_transient_error() {
    let test = new_test().await;
    let (run_id, step, _) = owned_executor(&test).await;
    inject_write_fault(&test.db, "step_results", "NEW.status = 'completed'", 2).await;

    let runs = Arc::new(AtomicUsize::new(0));
    let counted = runs.clone();
    let value: u32 = step
        .run("charge", || async move {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(5)
        })
        .await
        .unwrap();

    assert_eq!(value, 5);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(write_fault_hits(&test.db, "step_results").await, 3);
    let stored = find_steps(&test, run_id).await;
    assert_eq!(stored[0].status, "completed");
    assert_eq!(stored[0].output, Some(json!(5)));
    test.cleanup().await;
}

#[tokio::test]
async fn a_step_of_a_run_canceled_from_the_api_still_records_its_cancellation() {
    let test = new_test().await;
    let (run_id, step, signal) = owned_executor(&test).await;

    // The API marks the run canceled, keeps its worker, and fires the signal.
    let error = step
        .run::<u32, _, _>("charge", || async {
            sqlx::query("UPDATE flow_runs SET status = 'canceled' WHERE id = $1")
                .bind(run_id)
                .execute(&test.db)
                .await?;
            signal.cancel();
            std::future::pending().await
        })
        .await
        .unwrap_err();

    assert!(error.downcast_ref::<FlowCanceledError>().is_some(), "{error:#}");
    assert_eq!(find_steps(&test, run_id).await[0].status, "canceled");
    test.cleanup().await;
}
