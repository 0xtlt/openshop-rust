mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{TEST_SHOP, TestApp, create_config, noop_flow};
use openshop::db::schema::{FlowRun, StepResult};
use openshop::{
    CancellationToken, Concurrency, DispatchOptions, Flow, FlowContext, InputSchema, OpenShop, OpenShopConfig,
    RetryPolicy, Worker, WorkerConfig, bail, cancel_run, json,
};
use tokio::sync::{Barrier, Notify};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(15);

/// A worker that polls fast enough for tests.
fn worker_with(app: &OpenShop, config: WorkerConfig) -> Worker {
    Worker::new(
        app.clone(),
        config
            .poll_interval(Duration::from_millis(20))
            .poll_max_interval(Duration::from_millis(40)),
    )
}

fn worker(app: &OpenShop, concurrency: usize) -> Worker {
    worker_with(app, WorkerConfig::new().concurrency(concurrency))
}

/// A worker whose poll interval outlasts the test: after its first poll and
/// its first deadline sweep, only a run that finishes makes it claim again.
fn worker_without_polling(app: &OpenShop, concurrency: usize) -> Worker {
    let config = WorkerConfig::new()
        .concurrency(concurrency)
        .poll_interval(Duration::from_millis(60_000))
        .poll_max_interval(Duration::from_millis(60_000));
    Worker::new(app.clone(), config)
}

async fn dispatch(test: &TestApp, flow: &str) -> Uuid {
    dispatch_with(test, DispatchOptions::new(flow, TEST_SHOP)).await
}

async fn dispatch_with(test: &TestApp, params: DispatchOptions) -> Uuid {
    test.app.dispatch_flow(params).await.unwrap().run_id
}

fn without_retry() -> RetryPolicy {
    RetryPolicy::new().max_attempts(0)
}

async fn find_run(test: &TestApp, run_id: Uuid) -> FlowRun {
    sqlx::query_as("SELECT * FROM flow_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(&test.db)
        .await
        .unwrap()
}

async fn find_steps(test: &TestApp, run_id: Uuid) -> Vec<StepResult> {
    sqlx::query_as("SELECT * FROM step_results WHERE flow_run_id = $1 ORDER BY created_at, attempt")
        .bind(run_id.to_string())
        .fetch_all(&test.db)
        .await
        .unwrap()
}

async fn wait_for_status(test: &TestApp, run_id: Uuid, status: &str) -> FlowRun {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let run = find_run(test, run_id).await;
        if run.status == status {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "run is still \"{}\" (error: {:?}), expected \"{status}\"",
            run.status,
            run.error
        );
        tokio::time::sleep(POLL).await;
    }
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(POLL).await;
    }
}

/// Lets `duration` pass for the timers of this runtime, without waiting for it.
async fn skip(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}

/// A flow that reports when it starts, then blocks until `release` is cancelled.
fn gated_flow(name: &str, started: Arc<Notify>, release: CancellationToken) -> Flow {
    Flow::new(name, move |_ctx: FlowContext| {
        let (started, release) = (started.clone(), release.clone());
        async move {
            started.notify_one();
            release.cancelled().await;
            Ok(())
        }
    })
}

#[tokio::test]
async fn picks_up_and_completes_a_pending_run() {
    let test = TestApp::new(create_config([noop_flow("quick-flow")])).await;
    let run_id = dispatch(&test, "quick-flow").await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    assert!(worker.is_running());
    let run = wait_for_status(&test, run_id, "completed").await;
    worker.stop().await;

    assert_eq!(run.attempts, 1);
    assert_eq!(run.worker_id, None);
    assert!(run.started_at.is_some());
    assert!(run.completed_at.is_some());
    test.cleanup().await;
}

#[tokio::test]
async fn handles_failing_flow_gracefully() {
    let fail_flow = Flow::new("fail-flow", |_ctx: FlowContext| async move { bail!("worker-fail") });
    let test = TestApp::new(create_config([fail_flow, noop_flow("quick-flow")])).await;
    let failing = dispatch_with(
        &test,
        DispatchOptions::new("fail-flow", TEST_SHOP).retry_policy(without_retry()),
    )
    .await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    let run = wait_for_status(&test, failing, "failed").await;
    assert_eq!(run.error.as_deref(), Some("worker-fail"));
    assert_eq!(run.worker_id, None);
    assert!(run.completed_at.is_some());

    // The worker keeps going after a failure.
    let next = dispatch(&test, "quick-flow").await;
    wait_for_status(&test, next, "completed").await;
    assert!(worker.is_running());
    worker.stop().await;
    test.cleanup().await;
}

#[tokio::test]
async fn processes_multiple_runs_concurrently() {
    // Each run waits for the other one: they only finish if both are in flight at once.
    let barrier = Arc::new(Barrier::new(2));
    let flow = Flow::new("slow-flow", move |_ctx: FlowContext| {
        let barrier = barrier.clone();
        async move {
            barrier.wait().await;
            Ok(())
        }
    })
    .concurrency(Concurrency::Allow);
    let test = TestApp::new(create_config([flow])).await;
    let a = dispatch(&test, "slow-flow").await;
    let b = dispatch(&test, "slow-flow").await;

    let worker = worker(&test.app, 5);
    worker.start().await;
    wait_for_status(&test, a, "completed").await;
    wait_for_status(&test, b, "completed").await;
    worker.stop().await;
    test.cleanup().await;
}

#[tokio::test]
async fn never_runs_more_than_its_concurrency_limit() {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new("slow-flow", {
        let (in_flight, peak) = (in_flight.clone(), peak.clone());
        move |_ctx: FlowContext| {
            let (in_flight, peak) = (in_flight.clone(), peak.clone());
            async move {
                peak.fetch_max(in_flight.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(40)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }
        }
    })
    .concurrency(Concurrency::Allow);
    // The limit comes from the application, the polling from the worker's own settings.
    let config = OpenShopConfig::builder()
        .flow(flow)
        .worker(
            WorkerConfig::new()
                .concurrency(1)
                .poll_interval(Duration::from_millis(30_000)),
        )
        .build()
        .unwrap();
    let test = TestApp::new(config).await;
    let mut runs = Vec::new();
    for _ in 0..4 {
        runs.push(dispatch(&test, "slow-flow").await);
    }

    let worker = worker_with(&test.app, WorkerConfig::new());
    worker.start().await;
    for run_id in runs {
        wait_for_status(&test, run_id, "completed").await;
    }
    worker.stop().await;

    assert_eq!(peak.load(Ordering::SeqCst), 1);
    test.cleanup().await;
}

#[tokio::test]
async fn starts_the_next_run_as_soon_as_a_slot_is_free() {
    let test = TestApp::new(create_config([noop_flow("quick-flow").concurrency(Concurrency::Allow)])).await;
    let mut runs = Vec::new();
    for _ in 0..4 {
        runs.push(dispatch(&test, "quick-flow").await);
    }

    // The single slot is taken by the first run: the others start because a
    // run finished, not because the worker polled again.
    let worker = worker_without_polling(&test.app, 1);
    worker.start().await;
    for run_id in runs {
        wait_for_status(&test, run_id, "completed").await;
    }
    worker.stop().await;
    test.cleanup().await;
}

#[tokio::test]
async fn fills_its_free_slots_with_one_claim() {
    let started = Arc::new(AtomicUsize::new(0));
    let release = CancellationToken::new();
    let flow = Flow::new("slow-flow", {
        let (started, release) = (started.clone(), release.clone());
        move |_ctx: FlowContext| {
            let (started, release) = (started.clone(), release.clone());
            async move {
                started.fetch_add(1, Ordering::SeqCst);
                release.cancelled().await;
                Ok(())
            }
        }
    })
    .concurrency(Concurrency::Allow);
    let test = TestApp::new(create_config([flow])).await;
    let mut runs = Vec::new();
    for _ in 0..5 {
        runs.push(dispatch(&test, "slow-flow").await);
    }

    let worker = worker(&test.app, 3);
    worker.start().await;
    wait_until("three runs to start", || started.load(Ordering::SeqCst) == 3).await;
    assert_eq!(worker.active_count(), 3);

    // The three oldest runs were claimed, by a single statement: they share its timestamp.
    let mut started_at = Vec::new();
    for run_id in &runs[..3] {
        let run = find_run(&test, *run_id).await;
        assert_eq!((run.status.as_str(), run.attempts), ("running", 1));
        started_at.push(run.started_at.expect("started"));
    }
    assert!(started_at.iter().all(|at| *at == started_at[0]), "{started_at:?}");
    for run_id in &runs[3..] {
        let run = find_run(&test, *run_id).await;
        assert_eq!((run.status.as_str(), run.attempts), ("pending", 0));
    }

    release.cancel();
    for run_id in runs {
        wait_for_status(&test, run_id, "completed").await;
    }
    worker.stop().await;
    test.cleanup().await;
}

#[tokio::test]
async fn graceful_stop_waits_for_active_runs() {
    let started = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let flow = gated_flow("slow-flow", started.clone(), release.clone()).concurrency(Concurrency::Allow);
    let test = TestApp::new(create_config([flow])).await;
    let active = dispatch(&test, "slow-flow").await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    tokio::time::timeout(TIMEOUT, started.notified())
        .await
        .expect("run claimed");
    assert_eq!(worker.active_count(), 1);
    let queued = dispatch(&test, "slow-flow").await;

    let stopping = tokio::spawn({
        let worker = worker.clone();
        async move { worker.stop().await }
    });
    wait_until("the worker to stop claiming", || !worker.is_running()).await;
    assert!(!stopping.is_finished(), "stop must wait for the active run");
    release.cancel();
    tokio::time::timeout(TIMEOUT, stopping)
        .await
        .expect("worker stopped")
        .unwrap();

    assert!(!worker.is_running());
    assert_eq!(worker.active_count(), 0);
    assert_eq!(find_run(&test, active).await.status, "completed");
    // A stopping worker does not start new runs.
    let queued = find_run(&test, queued).await;
    assert_eq!((queued.status.as_str(), queued.attempts), ("pending", 0));
    test.cleanup().await;
}

#[tokio::test]
async fn handles_unregistered_flow_name() {
    let test = TestApp::new(create_config([noop_flow("quick-flow")])).await;
    let run_id = dispatch(&test, "quick-flow").await;

    // The worker runs an application definition that no longer has the flow.
    let worker = worker(&test.sibling(create_config(Vec::new())), 1);
    worker.start().await;
    let run = wait_for_status(&test, run_id, "failed").await;
    worker.stop().await;

    assert_eq!(run.error.as_deref(), Some("Flow \"quick-flow\" not registered"));
    assert_eq!(run.worker_id, None);
    assert!(run.completed_at.is_some());
    test.cleanup().await;
}

#[tokio::test]
async fn can_be_restarted_after_stop() {
    let test = TestApp::new(create_config([noop_flow("quick-flow")])).await;
    let worker = worker(&test.app, 1);
    assert!(!worker.is_running());

    worker.start().await;
    worker.start().await;
    assert!(worker.is_running());
    worker.stop().await;
    assert!(!worker.is_running());

    let run_id = dispatch(&test, "quick-flow").await;
    worker.start().await;
    wait_for_status(&test, run_id, "completed").await;
    worker.stop().await;
    assert_eq!(worker.active_count(), 0);
    test.cleanup().await;
}

#[tokio::test]
async fn passes_the_input_the_shop_and_the_app_to_the_flow() {
    let flow = Flow::new("echo-flow", |ctx: FlowContext| async move {
        let context = json!({ "shop": ctx.shop, "app": ctx.shopify_app, "input": ctx.input });
        let _: openshop::Value = ctx.step.run("context", || async { Ok(context) }).await?;
        Ok(())
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch_with(
        &test,
        DispatchOptions::new("echo-flow", "other.myshopify.com")
            .input(json!({ "limit": 3, "tags": ["a", "b"] }))
            .shopify_app("client-a"),
    )
    .await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    wait_for_status(&test, run_id, "completed").await;
    worker.stop().await;

    assert_eq!(
        find_steps(&test, run_id).await[0].output,
        Some(json!({
            "shop": "other.myshopify.com",
            "app": "client-a",
            "input": { "limit": 3, "tags": ["a", "b"] }
        }))
    );
    test.cleanup().await;
}

#[tokio::test]
async fn retries_a_failed_run_and_replays_its_completed_steps() {
    let charges = Arc::new(AtomicUsize::new(0));
    let tries = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new("flaky-flow", {
        let (charges, tries) = (charges.clone(), tries.clone());
        move |ctx: FlowContext| {
            let (charges, tries) = (charges.clone(), tries.clone());
            async move {
                let _: usize = ctx
                    .step
                    .run(
                        "charge",
                        || async move { Ok(charges.fetch_add(1, Ordering::SeqCst) + 1) },
                    )
                    .await?;
                if tries.fetch_add(1, Ordering::SeqCst) == 0 {
                    bail!("flaky network");
                }
                Ok(())
            }
        }
    })
    .retry_policy(
        RetryPolicy::new()
            .max_attempts(3)
            .initial_interval(Duration::from_millis(20)),
    );
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test, "flaky-flow").await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    let run = wait_for_status(&test, run_id, "completed").await;
    worker.stop().await;

    assert_eq!(run.attempts, 2);
    assert_eq!(tries.load(Ordering::SeqCst), 2);
    assert_eq!(
        charges.load(Ordering::SeqCst),
        1,
        "the completed step must not run twice"
    );
    let steps = find_steps(&test, run_id).await;
    assert_eq!(steps.len(), 1);
    assert_eq!((steps[0].attempt, steps[0].status.as_str()), (1, "completed"));
    test.cleanup().await;
}

#[tokio::test]
async fn gives_up_after_max_attempts() {
    let tries = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new("fail-flow", {
        let tries = tries.clone();
        move |_ctx: FlowContext| {
            let tries = tries.clone();
            async move { bail!("failure #{}", tries.fetch_add(1, Ordering::SeqCst) + 1) }
        }
    })
    .retry_policy(
        RetryPolicy::new()
            .max_attempts(3)
            .initial_interval(Duration::from_millis(10)),
    );
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test, "fail-flow").await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    let run = wait_for_status(&test, run_id, "failed").await;
    worker.stop().await;

    assert_eq!(run.attempts, 3);
    assert_eq!(run.error.as_deref(), Some("failure #3"));
    assert_eq!(tries.load(Ordering::SeqCst), 3);
    assert!(run.completed_at.is_some());
    test.cleanup().await;
}

#[tokio::test]
async fn cancel_run_interrupts_a_run_in_flight_and_frees_the_worker() {
    let started = Arc::new(Notify::new());
    let flow = Flow::new("long-flow", {
        let started = started.clone();
        move |ctx: FlowContext| {
            let started = started.clone();
            async move {
                ctx.step
                    .run("forever", || async move {
                        started.notify_one();
                        std::future::pending::<()>().await;
                        Ok(())
                    })
                    .await
            }
        }
    });
    let test = TestApp::new(create_config([flow, noop_flow("quick-flow")])).await;
    let run_id = dispatch(&test, "long-flow").await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    tokio::time::timeout(TIMEOUT, started.notified())
        .await
        .expect("run claimed");
    assert!(cancel_run(&run_id.to_string()));

    // A canceled run is final, whatever its retry policy allows.
    let run = wait_for_status(&test, run_id, "canceled").await;
    assert_eq!(run.attempts, 1);
    assert_eq!(run.worker_id, None);
    assert!(run.completed_at.is_some());
    assert_eq!(find_steps(&test, run_id).await[0].status, "canceled");

    let next = dispatch(&test, "quick-flow").await;
    wait_for_status(&test, next, "completed").await;
    worker.stop().await;
    assert_eq!(find_run(&test, run_id).await.status, "canceled");
    test.cleanup().await;
}

#[tokio::test]
async fn resumes_a_sleeping_run_after_its_resume_time() {
    let before_runs = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new("nap-flow", {
        let before_runs = before_runs.clone();
        move |ctx: FlowContext| {
            let before_runs = before_runs.clone();
            async move {
                ctx.step
                    .run("before", || async move {
                        before_runs.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .await?;
                ctx.step.sleep("nap", Duration::from_millis(150)).await?;
                let _: String = ctx.step.run("after", || async { Ok("rested".to_string()) }).await?;
                Ok(())
            }
        }
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test, "nap-flow").await;

    let worker = worker(&test.app, 1);
    let started = Instant::now();
    worker.start().await;
    let run = wait_for_status(&test, run_id, "completed").await;
    let elapsed = started.elapsed();
    worker.stop().await;

    // One attempt up to the sleep, one after it; the worker was free in between.
    assert_eq!(run.attempts, 2);
    assert_eq!(before_runs.load(Ordering::SeqCst), 1);
    assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");

    let steps: Vec<(String, String, i32)> = find_steps(&test, run_id)
        .await
        .into_iter()
        .map(|step| (step.step_name, step.status, step.attempt))
        .collect();
    let expected = [
        ("before", "completed", 1),
        ("nap", "completed", 1),
        ("after", "completed", 2),
    ]
    .map(|(name, status, attempt)| (name.to_string(), status.to_string(), attempt));
    assert_eq!(steps, expected);

    let (sleeping_logs,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM logs WHERE flow_run_id = $1 AND message = 'Flow \"nap-flow\" sleeping'")
            .bind(run_id.to_string())
            .fetch_one(&test.db)
            .await
            .unwrap();
    assert_eq!(sleeping_logs, 1);
    test.cleanup().await;
}

#[tokio::test]
async fn does_not_claim_runs_scheduled_in_the_future() {
    let flow = noop_flow("quick-flow").concurrency(Concurrency::Allow);
    let test = TestApp::new(create_config([flow])).await;
    let delayed = dispatch_with(
        &test,
        DispatchOptions::new("quick-flow", TEST_SHOP).delay(Duration::from_millis(60_000)),
    )
    .await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    // Two rounds of immediate runs give the worker every chance to pick the delayed one.
    for _ in 0..2 {
        let immediate = dispatch(&test, "quick-flow").await;
        wait_for_status(&test, immediate, "completed").await;
    }
    worker.stop().await;

    let run = find_run(&test, delayed).await;
    assert_eq!(run.status, "pending");
    assert_eq!(run.attempts, 0);
    assert!(run.started_at.is_none());
    test.cleanup().await;
}

#[tokio::test]
async fn claims_runs_in_availability_order() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let flow = Flow::new("ordered-flow", {
        let order = order.clone();
        move |ctx: FlowContext| {
            let order = order.clone();
            async move {
                order.lock().unwrap().push(ctx.input["n"].as_i64());
                Ok(())
            }
        }
    })
    .concurrency(Concurrency::Allow);
    let test = TestApp::new(create_config([flow])).await;

    let mut runs = Vec::new();
    for (n, hours_ago) in [(1, 1), (2, 3), (3, 2)] {
        let run_id = dispatch_with(
            &test,
            DispatchOptions::new("ordered-flow", TEST_SHOP).input(json!({ "n": n })),
        )
        .await;
        sqlx::query("UPDATE flow_runs SET available_at = now() - make_interval(hours => $2) WHERE id = $1")
            .bind(run_id)
            .bind(hours_ago)
            .execute(&test.db)
            .await
            .unwrap();
        runs.push(run_id);
    }

    let worker = worker(&test.app, 1);
    worker.start().await;
    for run_id in runs {
        wait_for_status(&test, run_id, "completed").await;
    }
    worker.stop().await;

    assert_eq!(*order.lock().unwrap(), [Some(2), Some(3), Some(1)]);
    test.cleanup().await;
}

#[tokio::test]
async fn expires_runs_past_their_deadline() {
    let executions = Arc::new(AtomicUsize::new(0));
    let guarded = Flow::new("guarded-flow", {
        let executions = executions.clone();
        move |_ctx: FlowContext| {
            executions.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        }
    })
    .timeout(Duration::from_secs(60))
    .concurrency(Concurrency::Allow);
    let test = TestApp::new(create_config([guarded, noop_flow("control-flow")])).await;

    let mut expired = Vec::new();
    for state in [
        "status = 'pending'",
        "status = 'sleeping', available_at = now() + interval '1 hour'",
        "status = 'running', worker_id = 'gone', attempts = 1, available_at = now() + interval '1 hour'",
    ] {
        let run_id = dispatch(&test, "guarded-flow").await;
        sqlx::query(&format!(
            "UPDATE flow_runs SET deadline_at = now() - interval '1 hour', {state} WHERE id = $1"
        ))
        .bind(run_id)
        .execute(&test.db)
        .await
        .unwrap();
        expired.push(run_id);
    }
    let in_time = dispatch(&test, "guarded-flow").await;
    let control = dispatch(&test, "control-flow").await;

    let worker = worker(&test.app, 2);
    worker.start().await;
    wait_for_status(&test, control, "completed").await;
    wait_for_status(&test, in_time, "completed").await;
    worker.stop().await;

    for run_id in expired {
        let run = find_run(&test, run_id).await;
        assert_eq!(run.status, "failed");
        assert_eq!(run.error.as_deref(), Some(TIMED_OUT));
        assert_eq!(run.available_at, None);
        assert_eq!(run.worker_id, None);
        assert!(run.completed_at.is_some());
    }
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "expired runs must not be executed"
    );
    test.cleanup().await;
}

const TIMED_OUT: &str = "Flow timed out — exceeded maximum allowed duration";

#[tokio::test]
async fn does_not_start_a_run_whose_deadline_passed_since_the_last_sweep() {
    let executions = Arc::new(AtomicUsize::new(0));
    let guarded = Flow::new("guarded-flow", {
        let executions = executions.clone();
        move |_ctx: FlowContext| {
            executions.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        }
    })
    .timeout(Duration::from_secs(60))
    .concurrency(Concurrency::Allow);
    let started = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let test = TestApp::new(create_config([
        guarded,
        gated_flow("gate-flow", started.clone(), release.clone()),
        noop_flow("control-flow"),
    ]))
    .await;

    // The worker sweeps on start, then not again before the test ends.
    let swept = dispatch(&test, "guarded-flow").await;
    sqlx::query("UPDATE flow_runs SET deadline_at = now() - interval '1 hour' WHERE id = $1")
        .bind(swept)
        .execute(&test.db)
        .await
        .unwrap();
    let gate = dispatch(&test, "gate-flow").await;
    let worker = worker_without_polling(&test.app, 1);
    worker.start().await;
    tokio::time::timeout(TIMEOUT, started.notified())
        .await
        .expect("run claimed");
    wait_for_status(&test, swept, "failed").await;

    // A run becomes claimable with its deadline already behind it, ahead of another one in the queue.
    let overdue = dispatch_with(
        &test,
        DispatchOptions::new("guarded-flow", TEST_SHOP).delay(Duration::from_millis(60_000)),
    )
    .await;
    sqlx::query(
        "UPDATE flow_runs SET available_at = now() - interval '1 minute', deadline_at = now() - interval '1 second' \
         WHERE id = $1",
    )
    .bind(overdue)
    .execute(&test.db)
    .await
    .unwrap();
    let control = dispatch(&test, "control-flow").await;

    release.cancel();
    wait_for_status(&test, gate, "completed").await;
    wait_for_status(&test, control, "completed").await;
    worker.stop().await;

    // The claims skipped it, and no sweep has failed it yet.
    let run = find_run(&test, overdue).await;
    assert_eq!((run.status.as_str(), run.attempts), ("pending", 0));
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "expired runs must not be executed"
    );

    // The next sweep does.
    worker.start().await;
    let run = wait_for_status(&test, overdue, "failed").await;
    worker.stop().await;
    assert_eq!(run.error.as_deref(), Some(TIMED_OUT));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    test.cleanup().await;
}

#[tokio::test]
async fn expires_runs_past_their_deadline_while_every_slot_is_busy() {
    let started = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let guarded = noop_flow("guarded-flow").timeout(Duration::from_secs(60));
    let test = TestApp::new(create_config([
        guarded,
        gated_flow("gate-flow", started.clone(), release.clone()),
    ]))
    .await;
    let gate = dispatch(&test, "gate-flow").await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    tokio::time::timeout(TIMEOUT, started.notified())
        .await
        .expect("run claimed");

    let overdue = dispatch(&test, "guarded-flow").await;
    sqlx::query("UPDATE flow_runs SET deadline_at = now() - interval '1 second' WHERE id = $1")
        .bind(overdue)
        .execute(&test.db)
        .await
        .unwrap();

    // The worker has no slot to claim with, and still fails the run.
    let run = wait_for_status(&test, overdue, "failed").await;
    assert_eq!(worker.active_count(), 1);
    assert_eq!(run.error.as_deref(), Some(TIMED_OUT));
    assert_eq!((run.attempts, run.available_at, run.worker_id), (0, None, None));
    assert!(run.completed_at.is_some());

    release.cancel();
    wait_for_status(&test, gate, "completed").await;
    worker.stop().await;
    test.cleanup().await;
}

#[tokio::test]
async fn takes_over_a_run_whose_lease_expired() {
    let executions = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new("stuck-flow", {
        let executions = executions.clone();
        move |ctx: FlowContext| {
            let executions = executions.clone();
            async move {
                let _: usize = ctx.step.run("once", || async { Ok(1) }).await?;
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test, "stuck-flow").await;

    // A worker claimed the run, checkpointed a step, then died without releasing it.
    sqlx::query(
        "UPDATE flow_runs SET status = 'running', worker_id = 'dead-worker', attempts = 1, \
           started_at = now(), available_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(run_id)
    .execute(&test.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO step_results (flow_run_id, step_name, attempt, status, output) \
         VALUES ($1, 'once', 1, 'completed', '1'::json)",
    )
    .bind(run_id.to_string())
    .execute(&test.db)
    .await
    .unwrap();

    let worker = worker_with(&test.app, WorkerConfig::new().concurrency(1));
    worker.start().await;
    let taken_over = wait_for_status(&test, run_id, "completed").await;
    worker.stop().await;

    assert_eq!(taken_over.attempts, 2);
    assert_eq!(taken_over.worker_id, None);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let steps = find_steps(&test, run_id).await;
    assert_eq!(steps.len(), 1, "the second attempt replays the checkpoint");
    test.cleanup().await;
}

// A lease is counted in wall-clock time, a heartbeat in the time of the runtime.
// A flow that really outlasts a short lease leaves each heartbeat half a lease
// to arrive, which a busy machine does not always grant. Skipping runtime time
// makes the heartbeats due at once instead, under a lease long enough to wait.
#[tokio::test]
async fn a_live_worker_keeps_a_lease_shorter_than_its_flow() {
    const LEASE: Duration = Duration::from_secs(14);
    // Past half a lease: a heartbeat is due. One sent on the default interval
    // would be 7 seconds away, one sent once per lease 6 seconds away.
    const SKIPPED: Duration = Duration::from_secs(8);
    // Time left to the heartbeat that is due, shorter than both.
    const AT_ONCE: Duration = Duration::from_secs(5);

    let started = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let test = TestApp::new(create_config([gated_flow(
        "slow-flow",
        started.clone(),
        release.clone(),
    )]))
    .await;
    let run_id = dispatch(&test, "slow-flow").await;
    let short_lease = || WorkerConfig::new().concurrency(1).lease_duration(LEASE);

    let first = worker_with(&test.app, short_lease());
    let second = worker_with(&test.app, short_lease());
    first.start().await;
    tokio::time::timeout(TIMEOUT, started.notified())
        .await
        .expect("run claimed");
    second.start().await;

    // Heartbeats follow the lease duration: every half lease extends it, and
    // the second worker never sees the run as abandoned.
    for _ in 0..2 {
        let leased_until = find_run(&test, run_id).await.available_at.expect("lease");
        skip(SKIPPED).await;
        let deadline = Instant::now() + AT_ONCE;
        loop {
            let run = find_run(&test, run_id).await;
            if run.available_at.is_some_and(|available_at| available_at > leased_until) {
                assert_eq!(run.status, "running");
                assert_eq!(run.worker_id.as_deref(), Some(first.worker_id()));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no heartbeat extended the lease after half of its duration"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    release.cancel();
    let completed = wait_for_status(&test, run_id, "completed").await;
    first.stop().await;
    second.stop().await;

    assert_eq!(completed.attempts, 1);
    test.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workers_never_execute_the_same_run_twice() {
    const RUNS: i64 = 24;
    let executed = Arc::new(Mutex::new(Vec::new()));
    let flow = Flow::new("counted-flow", {
        let executed = executed.clone();
        move |ctx: FlowContext| {
            let executed = executed.clone();
            async move {
                executed.lock().unwrap().push(ctx.input["n"].as_i64().unwrap());
                tokio::time::sleep(Duration::from_millis(5)).await;
                Ok(())
            }
        }
    })
    .concurrency(Concurrency::Allow);
    let test = TestApp::new(create_config([flow])).await;
    for n in 0..RUNS {
        dispatch_with(
            &test,
            DispatchOptions::new("counted-flow", TEST_SHOP).input(json!({ "n": n })),
        )
        .await;
    }

    let workers = [worker(&test.app, 3), worker(&test.app, 3)];
    for worker in &workers {
        worker.start().await;
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let (completed,): (i64,) = sqlx::query_as("SELECT count(*) FROM flow_runs WHERE status = 'completed'")
            .fetch_one(&test.db)
            .await
            .unwrap();
        if completed == RUNS {
            break;
        }
        assert!(Instant::now() < deadline, "only {completed} of {RUNS} runs completed");
        tokio::time::sleep(POLL).await;
    }
    for worker in &workers {
        worker.stop().await;
    }

    let mut executed = executed.lock().unwrap().clone();
    executed.sort_unstable();
    assert_eq!(executed, (0..RUNS).collect::<Vec<_>>());
    let (max_attempts,): (i32,) = sqlx::query_as("SELECT max(attempts) FROM flow_runs")
        .fetch_one(&test.db)
        .await
        .unwrap();
    assert_eq!(max_attempts, 1);
    test.cleanup().await;
}

fn explode(_ctx: FlowContext) -> std::future::Ready<openshop::Result<()>> {
    panic!("exploded before the first await")
}

#[tokio::test]
async fn records_a_panicking_flow_as_failed_and_frees_its_slot() {
    let panicking = Flow::new("panic-flow", |ctx: FlowContext| async move {
        let divisor = ctx.input["divisor"].as_u64().unwrap_or(0);
        ctx.logger.info_with("divided", json!({ "result": 10 / divisor }));
        Ok(())
    });
    let test = TestApp::new(create_config([
        panicking,
        Flow::new("explode-flow", explode),
        noop_flow("quick-flow"),
    ]))
    .await;
    let worker = worker(&test.app, 1);
    worker.start().await;

    for (flow, error) in [
        ("panic-flow", "Flow panicked: attempt to divide by zero"),
        ("explode-flow", "Flow panicked: exploded before the first await"),
    ] {
        let run_id = dispatch_with(
            &test,
            DispatchOptions::new(flow, TEST_SHOP).retry_policy(without_retry()),
        )
        .await;
        let run = wait_for_status(&test, run_id, "failed").await;
        assert_eq!(run.error.as_deref(), Some(error));
        assert_eq!(run.worker_id, None);

        // With a single slot, the next run only starts if the panic released it.
        let next = dispatch(&test, "quick-flow").await;
        wait_for_status(&test, next, "completed").await;
    }
    worker.stop().await;
    assert_eq!(worker.active_count(), 0);
    test.cleanup().await;
}

#[tokio::test]
async fn survives_a_panic_outside_of_the_flow_body() {
    let validated = noop_flow("validated-flow")
        .input_schema(InputSchema::custom(json!({ "type": "object" }), |_input| {
            panic!("validator exploded")
        }));
    let test = TestApp::new(create_config([validated, noop_flow("quick-flow")])).await;
    let worker = worker(&test.app, 1);
    worker.start().await;

    let run_id = dispatch_with(
        &test,
        DispatchOptions::new("validated-flow", TEST_SHOP).retry_policy(without_retry()),
    )
    .await;
    let run = wait_for_status(&test, run_id, "failed").await;
    assert_eq!(run.error.as_deref(), Some("Worker error: panicked: validator exploded"));
    assert_eq!(run.worker_id, None);

    let next = dispatch(&test, "quick-flow").await;
    wait_for_status(&test, next, "completed").await;
    worker.stop().await;
    assert_eq!(worker.active_count(), 0);
    test.cleanup().await;
}

/// Lets 16 seconds pass for the timers of this runtime, enough for one heartbeat.
async fn skip_to_next_heartbeat() {
    skip(Duration::from_secs(16)).await;
}

#[tokio::test]
async fn heartbeat_extends_the_lease_of_a_long_run() {
    let started = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let test = TestApp::new(create_config([gated_flow(
        "long-flow",
        started.clone(),
        release.clone(),
    )]))
    .await;
    let run_id = dispatch(&test, "long-flow").await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    tokio::time::timeout(TIMEOUT, started.notified())
        .await
        .expect("run claimed");
    let leased_until = find_run(&test, run_id).await.available_at.expect("lease");

    skip_to_next_heartbeat().await;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let run = find_run(&test, run_id).await;
        if run.available_at.is_some_and(|available_at| available_at > leased_until) {
            assert_eq!(run.status, "running");
            assert_eq!(run.worker_id.as_deref(), Some(worker.worker_id()));
            break;
        }
        assert!(Instant::now() < deadline, "the lease was not extended");
        tokio::time::sleep(POLL).await;
    }

    release.cancel();
    wait_for_status(&test, run_id, "completed").await;
    worker.stop().await;
    test.cleanup().await;
}

#[tokio::test]
async fn worker_that_lost_its_lease_cancels_the_flow_on_the_next_heartbeat() {
    let started = Arc::new(Notify::new());
    let flow = Flow::new("long-flow", {
        let started = started.clone();
        move |ctx: FlowContext| {
            let started = started.clone();
            async move {
                ctx.step
                    .run("forever", || async move {
                        started.notify_one();
                        std::future::pending::<()>().await;
                        Ok(())
                    })
                    .await
            }
        }
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test, "long-flow").await;

    let worker = worker(&test.app, 1);
    worker.start().await;
    tokio::time::timeout(TIMEOUT, started.notified())
        .await
        .expect("run claimed");
    assert_eq!(worker.active_count(), 1);

    // Another worker took the run over, with a lease far in the future.
    sqlx::query("UPDATE flow_runs SET worker_id = 'thief', available_at = now() + interval '1 hour' WHERE id = $1")
        .bind(run_id)
        .execute(&test.db)
        .await
        .unwrap();
    let stolen = find_run(&test, run_id).await;

    skip_to_next_heartbeat().await;
    wait_until("the flow to be interrupted", || worker.active_count() == 0).await;
    worker.stop().await;

    let steps = find_steps(&test, run_id).await;
    assert_eq!(
        (steps[0].step_name.as_str(), steps[0].status.as_str()),
        ("forever", "canceled")
    );
    // The run itself now belongs to the other worker and is left untouched.
    assert_eq!(find_run(&test, run_id).await, stolen);
    test.cleanup().await;
}
