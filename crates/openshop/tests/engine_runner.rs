mod common;

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use common::{TEST_SHOP, TestApp, create_config, noop_flow};
use openshop::builder::Flow;
use openshop::db::schema::{FlowRun, LogRow, StepResult};
use openshop::engine::{RunFlowOptions, cancel_run, run_flow};
use openshop::{
    Concurrency, DispatchOptions, ErrorContext, FlowContext, FlowTimeoutError, OpenShopConfig, RetryPolicy, RunOutcome,
    StepOptions, StepTimeoutError, bail, json,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Notify;
use uuid::Uuid;

const FLOW: &str = "test-flow";

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

async fn find_logs(test: &TestApp, run_id: Uuid) -> Vec<LogRow> {
    sqlx::query_as("SELECT * FROM logs WHERE flow_run_id = $1 ORDER BY created_at")
        .bind(run_id.to_string())
        .fetch_all(&test.db)
        .await
        .unwrap()
}

fn messages(logs: &[LogRow]) -> Vec<&str> {
    logs.iter().filter_map(|log| log.message.as_deref()).collect()
}

async fn dispatch(test: &TestApp) -> Uuid {
    test.app
        .dispatch_flow(DispatchOptions::new(FLOW, TEST_SHOP))
        .await
        .unwrap()
        .run_id
}

/// Queues a run whose first failure is final.
async fn dispatch_without_retry(test: &TestApp) -> Uuid {
    test.app
        .dispatch_flow(DispatchOptions::new(FLOW, TEST_SHOP).retry_policy(RetryPolicy::new().max_attempts(0)))
        .await
        .unwrap()
        .run_id
}

/// Marks the run the way a worker does when it claims it.
async fn claim(test: &TestApp, run_id: Uuid) {
    sqlx::query("UPDATE flow_runs SET status = 'running', attempts = attempts + 1 WHERE id = $1")
        .bind(run_id)
        .execute(&test.db)
        .await
        .unwrap();
}

fn options(run_id: Uuid) -> RunFlowOptions {
    RunFlowOptions::new(run_id, FLOW, TEST_SHOP)
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("condition not reached in time")
}

#[tokio::test]
async fn successful_flow_completes() {
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        let logger = ctx.logger.clone();
        let greeted: String = ctx
            .step
            .run("greet", || async move {
                logger.info("hello");
                Ok("world".to_string())
            })
            .await?;
        assert_eq!(greeted, "world");
        Ok(())
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test).await;

    let result = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(result, RunOutcome::Completed);
    assert_eq!(result.status(), "completed");

    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "completed");
    assert!(run.completed_at.is_some());
    assert_eq!(run.worker_id, None);
    assert_eq!(run.error, None);
    test.cleanup().await;
}

#[tokio::test]
async fn returns_lease_lost_when_the_worker_no_longer_owns_the_run() {
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        sqlx::query("UPDATE flow_runs SET worker_id = 'worker-b' WHERE worker_id = 'worker-a'")
            .execute(&ctx.db)
            .await?;
        Ok(())
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test).await;
    sqlx::query(
        "UPDATE flow_runs SET status = 'running', worker_id = 'worker-a', attempts = 1, \
         available_at = now() + interval '30 seconds' WHERE id = $1",
    )
    .bind(run_id)
    .execute(&test.db)
    .await
    .unwrap();

    let result = run_flow(&test.app, options(run_id).worker_id("worker-a").attempt(1))
        .await
        .unwrap();
    assert_eq!(result, RunOutcome::LeaseLost);
    assert_eq!(result.status(), "lease_lost");

    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "running");
    assert_eq!(run.worker_id.as_deref(), Some("worker-b"));
    assert!(run.completed_at.is_none());
    test.cleanup().await;
}

#[tokio::test]
async fn an_execution_whose_run_was_claimed_again_by_the_same_worker_cannot_record_its_outcome() {
    // The same worker claims the run again, with the same attempts as the first
    // claim since a retry reset them: only the lease tells the executions apart.
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        sqlx::query("UPDATE flow_runs SET lease_id = gen_random_uuid() WHERE worker_id = 'worker-a'")
            .execute(&ctx.db)
            .await?;
        if ctx.input["then"] == "fail" {
            bail!("boom");
        }
        Ok(())
    })
    .concurrency(Concurrency::Allow);
    let test = TestApp::new(create_config([flow])).await;

    for then in ["complete", "fail"] {
        let run_id = dispatch(&test).await;
        let lease_id = Uuid::new_v4();
        sqlx::query(
            "UPDATE flow_runs SET status = 'running', worker_id = 'worker-a', attempts = 1, lease_id = $2 WHERE id = $1",
        )
        .bind(run_id)
        .bind(lease_id)
        .execute(&test.db)
        .await
        .unwrap();

        let result = run_flow(
            &test.app,
            options(run_id)
                .input(json!({ "then": then }))
                .worker_id("worker-a")
                .attempt(1)
                .lease_id(Some(lease_id)),
        )
        .await
        .unwrap();
        assert_eq!(result, RunOutcome::LeaseLost, "{then}");

        let run = find_run(&test, run_id).await;
        assert_eq!(run.status, "running", "{then}");
        assert_eq!(run.worker_id.as_deref(), Some("worker-a"), "{then}");
        assert_eq!(run.attempts, 1, "{then}");
        assert_eq!(run.error, None, "{then}");
        assert!(run.completed_at.is_none(), "{then}");
    }
    test.cleanup().await;
}

#[tokio::test]
async fn lost_lease_never_records_a_failure_or_a_sleep() {
    let reported = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        sqlx::query("UPDATE flow_runs SET worker_id = 'worker-b' WHERE worker_id = 'worker-a'")
            .execute(&ctx.db)
            .await?;
        if ctx.input["then"] == "sleep" {
            ctx.step.sleep("pause", Duration::from_secs(60)).await?;
        }
        bail!("boom")
    })
    .concurrency(Concurrency::Allow);
    let config = OpenShopConfig::builder()
        .flow(flow)
        .on_error({
            let reported = reported.clone();
            move |_error, _context| {
                reported.fetch_add(1, Ordering::SeqCst);
                async {}
            }
        })
        .build()
        .unwrap();
    let test = TestApp::new(config).await;

    for then in ["fail", "sleep"] {
        let run_id = dispatch(&test).await;
        sqlx::query("UPDATE flow_runs SET status = 'running', worker_id = 'worker-a', attempts = 1 WHERE id = $1")
            .bind(run_id)
            .execute(&test.db)
            .await
            .unwrap();
        let available_at = find_run(&test, run_id).await.available_at;

        let result = run_flow(
            &test.app,
            options(run_id)
                .input(json!({ "then": then }))
                .worker_id("worker-a")
                .attempt(1),
        )
        .await
        .unwrap();
        assert_eq!(result, RunOutcome::LeaseLost, "{then}");

        let run = find_run(&test, run_id).await;
        assert_eq!(run.status, "running", "{then}");
        assert_eq!(run.worker_id.as_deref(), Some("worker-b"), "{then}");
        assert_eq!(run.error, None, "{then}");
        assert_eq!(run.available_at, available_at, "{then}");
        assert!(run.completed_at.is_none(), "{then}");
    }
    assert_eq!(reported.load(Ordering::SeqCst), 0);
    test.cleanup().await;
}

#[tokio::test]
async fn failing_flow_sets_status_to_failed() {
    let flow = Flow::new(FLOW, |_ctx: FlowContext| async move { bail!("boom") });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch_without_retry(&test).await;

    let result = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(
        result,
        RunOutcome::Failed {
            error: "boom".to_string(),
            will_retry: false
        }
    );
    assert_eq!(result.status(), "failed");

    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "failed");
    assert_eq!(run.error.as_deref(), Some("boom"));
    assert!(run.completed_at.is_some());
    test.cleanup().await;
}

#[tokio::test]
async fn flow_with_retry_stays_pending_after_failure() {
    let flow = Flow::new(FLOW, |_ctx: FlowContext| async move { bail!("retry me") })
        .retry_policy(RetryPolicy::new().max_attempts(3));
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test).await;
    let before = Utc::now();

    let result = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(
        result,
        RunOutcome::Failed {
            error: "retry me".to_string(),
            will_retry: true
        }
    );

    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "pending");
    assert_eq!(run.error.as_deref(), Some("retry me"));
    assert!(run.completed_at.is_none());
    // The run was never claimed, so the backoff is the one before a first attempt: 500ms.
    let available_at = run.available_at.expect("retry time");
    assert!(
        available_at >= before + chrono::Duration::milliseconds(400),
        "{available_at}"
    );
    test.cleanup().await;
}

#[tokio::test]
async fn cancellation_sets_status_to_canceled() {
    let slow_started = Arc::new(Notify::new());
    let flow = Flow::new(FLOW, {
        let slow_started = slow_started.clone();
        move |ctx: FlowContext| {
            let slow_started = slow_started.clone();
            async move {
                let _: String = ctx.step.run("setup", || async { Ok("ok".to_string()) }).await?;
                ctx.step
                    .run("slow", || async move {
                        slow_started.notify_one();
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        Ok(())
                    })
                    .await
            }
        }
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch_without_retry(&test).await;
    assert!(
        !cancel_run(&run_id.to_string()),
        "nothing to cancel before the run starts"
    );

    let running = tokio::spawn({
        let app = test.app.clone();
        async move { run_flow(&app, options(run_id)).await }
    });
    within(slow_started.notified()).await;
    assert!(cancel_run(&run_id.to_string()));

    // The in-flight step is interrupted instead of running to its end.
    let result = within(running).await.unwrap().unwrap();
    assert_eq!(result, RunOutcome::Canceled);
    assert!(
        !cancel_run(&run_id.to_string()),
        "the token is forgotten once the run ends"
    );

    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "canceled");
    assert!(run.completed_at.is_some());
    assert_eq!(run.error, None);

    let steps = find_steps(&test, run_id).await;
    let statuses: Vec<(&str, &str)> = steps
        .iter()
        .map(|step| (step.step_name.as_str(), step.status.as_str()))
        .collect();
    assert_eq!(statuses, [("setup", "completed"), ("slow", "canceled")]);
    assert!(steps[1].duration_ms.is_some());

    let logs = find_logs(&test, run_id).await;
    assert_eq!(messages(&logs).last(), Some(&"Flow \"test-flow\" was canceled"));
    test.cleanup().await;
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LimitInput {
    limit: u32,
    #[serde(default)]
    label: Option<String>,
}

type SeenInputs = Arc<Mutex<Vec<(u32, Option<String>)>>>;

fn limit_flow(seen: SeenInputs) -> Flow {
    Flow::new(FLOW, move |ctx: FlowContext| {
        let seen = seen.clone();
        async move {
            let input: LimitInput = ctx.input()?;
            seen.lock().unwrap().push((input.limit, input.label));
            Ok(())
        }
    })
    .input::<LimitInput>()
    .concurrency(Concurrency::Allow)
}

#[tokio::test]
async fn input_validation_rejects_invalid_input() {
    let seen = SeenInputs::default();
    let test = TestApp::new(create_config([limit_flow(seen.clone())])).await;

    for input in [json!({ "limit": "not-a-number" }), json!({}), json!({ "limit": -1 })] {
        let run_id = dispatch_without_retry(&test).await;
        let result = run_flow(&test.app, options(run_id).input(input.clone())).await.unwrap();
        let RunOutcome::Failed { error, will_retry } = result else {
            panic!("expected a failure for {input}, got {result:?}");
        };
        assert!(error.starts_with("Invalid input for flow \"test-flow\": "), "{error}");
        assert!(!will_retry);

        let run = find_run(&test, run_id).await;
        assert_eq!(run.status, "failed");
        assert_eq!(run.error.as_deref(), Some(error.as_str()));
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "the flow must not run with invalid input"
    );
    test.cleanup().await;
}

#[tokio::test]
async fn typed_input_is_validated_and_exposed_to_the_flow() {
    let seen = SeenInputs::default();
    let test = TestApp::new(create_config([limit_flow(seen.clone())])).await;

    for input in [json!({ "limit": 5 }), json!({ "limit": 7, "label": "nightly" })] {
        let run_id = dispatch(&test).await;
        let result = run_flow(&test.app, options(run_id).input(input)).await.unwrap();
        assert_eq!(result, RunOutcome::Completed);
    }
    assert_eq!(*seen.lock().unwrap(), [(5, None), (7, Some("nightly".to_string()))]);
    test.cleanup().await;
}

#[tokio::test]
async fn flow_creates_logs_in_db() {
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        let logger = ctx.logger.clone();
        ctx.step
            .run("work", || async move {
                logger.info_with("test message", json!({ "key": "val" }));
                Ok(())
            })
            .await
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test).await;

    run_flow(&test.app, options(run_id)).await.unwrap();

    // No waiting: the logger is flushed before `run_flow` returns.
    let logs = find_logs(&test, run_id).await;
    let test_log = logs
        .iter()
        .find(|log| log.message.as_deref() == Some("test message"))
        .expect("test message");
    assert_eq!(test_log.level, "info");
    assert_eq!(test_log.payload, Some(json!({ "key": "val" })));

    let messages = messages(&logs);
    assert_eq!(messages.len(), 5, "{messages:?}");
    assert_eq!(messages[0], "Flow \"test-flow\" started");
    assert_eq!(messages[1], "Step \"work\" started");
    assert_eq!(messages[2], "test message");
    assert!(messages[3].starts_with("Step \"work\" completed ("), "{messages:?}");
    assert_eq!(messages[4], "Flow \"test-flow\" completed");
    assert_eq!(logs[0].payload, Some(json!({ "flowName": "test-flow" })));
    test.cleanup().await;
}

#[tokio::test]
async fn logs_are_fully_persisted_when_run_flow_returns() {
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        for line in 0..200 {
            ctx.logger.info_with(format!("line {line}"), json!({ "line": line }));
        }
        ctx.logger.warn_with("running low", json!({ "left": 3 }));
        ctx.logger.error_with("gave up", json!({ "code": "E42" }));
        bail!("boom")
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch_without_retry(&test).await;

    run_flow(&test.app, options(run_id)).await.unwrap();

    let logs = find_logs(&test, run_id).await;
    // 200 lines, the warning, the error, and the "started" and "failed" lines of the runner.
    assert_eq!(logs.len(), 204);
    let lines: std::collections::BTreeSet<i64> = logs
        .iter()
        .filter_map(|log| log.payload.as_ref()?.get("line")?.as_i64())
        .collect();
    assert!(lines.into_iter().eq(0..200));

    let by_message = |message: &str| {
        logs.iter()
            .find(|log| log.message.as_deref() == Some(message))
            .unwrap_or_else(|| panic!("missing log line {message:?}"))
    };
    assert_eq!(by_message("running low").level, "warn");
    assert_eq!(by_message("gave up").level, "error");
    assert_eq!(by_message("gave up").payload, Some(json!({ "code": "E42" })));
    let failed = by_message("Flow \"test-flow\" failed: boom");
    assert_eq!(failed.level, "error");
    assert_eq!(
        failed.payload,
        Some(json!({ "flowName": "test-flow", "error": "boom", "willRetry": false }))
    );
    test.cleanup().await;
}

#[tokio::test]
async fn flow_timeout_causes_failure() {
    let flow = Flow::new(FLOW, |_ctx: FlowContext| async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
    })
    .timeout(Duration::from_millis(100));
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch_without_retry(&test).await;

    let started = Instant::now();
    let result = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(
        result,
        RunOutcome::Failed {
            error: "Flow \"test-flow\" timed out after 100ms".to_string(),
            will_retry: false
        }
    );
    assert!(started.elapsed() < Duration::from_secs(10));

    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "failed");
    assert!(run.error.unwrap().contains("timed out"));
    test.cleanup().await;
}

#[tokio::test]
async fn a_step_interrupted_by_the_flow_timeout_is_marked_failed() {
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        ctx.step
            .run("slow", || async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(())
            })
            .await
    })
    // Long enough for the step to be recorded as running before the flow is interrupted.
    .timeout(Duration::from_secs(1));
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch_without_retry(&test).await;

    let result = run_flow(&test.app, options(run_id)).await.unwrap();
    assert!(matches!(result, RunOutcome::Failed { .. }));

    // The step row is closed in the background once its future is dropped.
    let step = within(async {
        loop {
            let recorded = find_steps(&test, run_id).await.into_iter().next();
            if let Some(step) = recorded.filter(|step| step.status != "running") {
                break step;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(step.status, "failed");
    assert_eq!(step.error.as_deref(), Some("Step was interrupted"));
    test.cleanup().await;
}

#[tokio::test]
async fn completing_after_a_failed_attempt_clears_the_stored_error() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new(FLOW, {
        let attempts = attempts.clone();
        move |_ctx: FlowContext| {
            let attempts = attempts.clone();
            async move {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    openshop::bail!("first attempt fails");
                }
                Ok(())
            }
        }
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test).await;

    let first = run_flow(&test.app, options(run_id)).await.unwrap();
    assert!(matches!(first, RunOutcome::Failed { will_retry: true, .. }));
    assert_eq!(
        find_run(&test, run_id).await.error.as_deref(),
        Some("first attempt fails")
    );

    let second = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(second, RunOutcome::Completed);
    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "completed");
    assert_eq!(run.error, None);
    test.cleanup().await;
}

#[tokio::test]
async fn flow_with_multiple_steps_stores_all_outputs() {
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        let a: String = ctx.step.run("step-a", || async { Ok("alpha".to_string()) }).await?;
        let _: openshop::Value = ctx
            .step
            .run("step-b", || async { Ok(json!({ "prev": a, "val": "beta" })) })
            .await?;
        Ok(())
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test).await;
    run_flow(&test.app, options(run_id)).await.unwrap();

    let steps = find_steps(&test, run_id).await;
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].step_name, "step-a");
    assert_eq!(steps[0].output, Some(json!("alpha")));
    assert_eq!(steps[1].step_name, "step-b");
    assert_eq!(steps[1].output, Some(json!({ "prev": "alpha", "val": "beta" })));
    assert!(steps.iter().all(|step| step.status == "completed" && step.attempt == 1));
    test.cleanup().await;
}

#[tokio::test]
async fn fails_on_unknown_flow() {
    let test = TestApp::new(create_config([noop_flow(FLOW)])).await;
    let error = run_flow(&test.app, RunFlowOptions::new(Uuid::new_v4(), "missing", TEST_SHOP))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "Flow \"missing\" not found. Available: test-flow");
    test.cleanup().await;
}

#[tokio::test]
async fn flow_context_carries_the_shop_the_app_and_the_input() {
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        let context = json!({ "shop": ctx.shop, "app": ctx.shopify_app, "input": ctx.input });
        let _: openshop::Value = ctx.step.run("context", || async { Ok(context) }).await?;
        Ok(())
    })
    .concurrency(Concurrency::Allow);
    let test = TestApp::new(create_config([flow])).await;

    let defaulted = dispatch(&test).await;
    run_flow(&test.app, options(defaulted)).await.unwrap();
    assert_eq!(
        find_steps(&test, defaulted).await[0].output,
        Some(json!({ "shop": TEST_SHOP, "app": "default", "input": {} }))
    );

    let explicit = dispatch(&test).await;
    run_flow(
        &test.app,
        RunFlowOptions::new(explicit, FLOW, "other.myshopify.com")
            .input(json!({ "limit": 3 }))
            .shopify_app("client-a"),
    )
    .await
    .unwrap();
    assert_eq!(
        find_steps(&test, explicit).await[0].output,
        Some(json!({ "shop": "other.myshopify.com", "app": "client-a", "input": { "limit": 3 } }))
    );
    test.cleanup().await;
}

#[tokio::test]
async fn sleeping_flow_resumes_once_the_resume_time_has_passed() {
    let before_runs = Arc::new(AtomicUsize::new(0));
    let after_runs = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new(FLOW, {
        let (before_runs, after_runs) = (before_runs.clone(), after_runs.clone());
        move |ctx: FlowContext| {
            let (before_runs, after_runs) = (before_runs.clone(), after_runs.clone());
            async move {
                ctx.step
                    .run("before", || async move {
                        before_runs.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .await?;
                ctx.step.sleep("pause", Duration::from_secs(60)).await?;
                ctx.step
                    .run("after", || async move {
                        after_runs.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .await
            }
        }
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test).await;
    let before = Utc::now();

    let first = run_flow(&test.app, options(run_id)).await.unwrap();
    let RunOutcome::Sleeping { resume_at } = first else {
        panic!("expected the run to sleep, got {first:?}");
    };
    assert_eq!(first.status(), "sleeping");
    assert!(resume_at >= before + chrono::Duration::seconds(60));

    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "sleeping");
    assert_eq!(run.worker_id, None);
    assert!(run.completed_at.is_none());
    let drift = (run.available_at.expect("resume time") - resume_at).num_milliseconds();
    assert!(drift.abs() <= 1, "{drift}");

    let steps = find_steps(&test, run_id).await;
    assert_eq!(steps.len(), 2);
    assert_eq!(
        (steps[1].step_name.as_str(), steps[1].status.as_str()),
        ("pause", "sleeping")
    );
    let stored = steps[1]
        .output
        .as_ref()
        .and_then(|output| output.as_str())
        .expect("resume time");
    let stored = chrono::DateTime::parse_from_rfc3339(stored)
        .unwrap()
        .with_timezone(&Utc);
    assert!((stored - resume_at).num_milliseconds().abs() <= 1);

    // Too early: the run goes back to sleep until the stored time, without repeating anything.
    let early = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(early, RunOutcome::Sleeping { resume_at: stored });
    assert_eq!(before_runs.load(Ordering::SeqCst), 1);
    assert_eq!(after_runs.load(Ordering::SeqCst), 0);

    // Move the stored resume time into the past instead of waiting a minute.
    let past = (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    sqlx::query("UPDATE step_results SET output = to_json($1::text) WHERE flow_run_id = $2 AND step_name = 'pause'")
        .bind(past)
        .bind(run_id.to_string())
        .execute(&test.db)
        .await
        .unwrap();

    let resumed = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(resumed, RunOutcome::Completed);
    assert_eq!(before_runs.load(Ordering::SeqCst), 1);
    assert_eq!(after_runs.load(Ordering::SeqCst), 1);

    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "completed");
    assert!(run.completed_at.is_some());
    let statuses: Vec<(String, String)> = find_steps(&test, run_id)
        .await
        .into_iter()
        .map(|step| (step.step_name, step.status))
        .collect();
    let expected = [("before", "completed"), ("pause", "completed"), ("after", "completed")]
        .map(|(name, status)| (name.to_string(), status.to_string()));
    assert_eq!(statuses, expected);
    test.cleanup().await;
}

#[tokio::test]
async fn retry_replays_completed_steps_instead_of_running_them_again() {
    let charges = Arc::new(AtomicUsize::new(0));
    let notifications = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new(FLOW, {
        let (charges, notifications) = (charges.clone(), notifications.clone());
        move |ctx: FlowContext| {
            let (charges, notifications) = (charges.clone(), notifications.clone());
            async move {
                let receipt: usize = ctx
                    .step
                    .run(
                        "charge",
                        || async move { Ok(charges.fetch_add(1, Ordering::SeqCst) + 1) },
                    )
                    .await?;
                let _: String = ctx
                    .step
                    .run("notify", || async move {
                        if notifications.fetch_add(1, Ordering::SeqCst) == 0 {
                            bail!("smtp down");
                        }
                        Ok(format!("receipt-{receipt}"))
                    })
                    .await?;
                Ok(())
            }
        }
    })
    .retry_policy(RetryPolicy::new().max_attempts(3));
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test).await;

    claim(&test, run_id).await;
    let first = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(
        first,
        RunOutcome::Failed {
            error: "smtp down".to_string(),
            will_retry: true
        }
    );
    assert_eq!(find_run(&test, run_id).await.status, "pending");

    claim(&test, run_id).await;
    let second = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(second, RunOutcome::Completed);
    assert_eq!(
        charges.load(Ordering::SeqCst),
        1,
        "the completed step must not run twice"
    );
    assert_eq!(notifications.load(Ordering::SeqCst), 2);

    let steps: Vec<(String, i32, String, Option<openshop::Value>)> = find_steps(&test, run_id)
        .await
        .into_iter()
        .map(|step| (step.step_name, step.attempt, step.status, step.output))
        .collect();
    assert_eq!(
        steps,
        [
            ("charge".to_string(), 1, "completed".to_string(), Some(json!(1))),
            ("notify".to_string(), 1, "failed".to_string(), None),
            (
                "notify".to_string(),
                2,
                "completed".to_string(),
                Some(json!("receipt-1"))
            ),
        ]
    );
    let logs = find_logs(&test, run_id).await;
    assert!(messages(&logs).contains(&"Step \"charge\" skipped (cached)"));

    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "completed");
    assert_eq!(run.attempts, 2);
    test.cleanup().await;
}

#[tokio::test]
async fn retries_stop_once_max_attempts_is_reached() {
    let flow = Flow::new(FLOW, |_ctx: FlowContext| async move { bail!("still broken") }).retry_policy(
        RetryPolicy::new()
            .max_attempts(2)
            .initial_interval(Duration::from_millis(1)),
    );
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test).await;

    claim(&test, run_id).await;
    let first = run_flow(&test.app, options(run_id)).await.unwrap();
    assert!(
        matches!(first, RunOutcome::Failed { will_retry: true, .. }),
        "{first:?}"
    );
    assert_eq!(find_run(&test, run_id).await.status, "pending");

    claim(&test, run_id).await;
    let second = run_flow(&test.app, options(run_id)).await.unwrap();
    assert!(
        matches!(second, RunOutcome::Failed { will_retry: false, .. }),
        "{second:?}"
    );
    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "failed");
    assert_eq!(run.error.as_deref(), Some("still broken"));
    assert!(run.completed_at.is_some());
    test.cleanup().await;
}

#[tokio::test]
async fn retry_is_skipped_when_it_would_land_past_the_deadline() {
    let flow = Flow::new(FLOW, |_ctx: FlowContext| async move { bail!("boom") })
        .timeout(Duration::from_secs(5))
        .retry_policy(
            RetryPolicy::new()
                .max_attempts(5)
                .initial_interval(Duration::from_millis(20_000)),
        );
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch(&test).await;

    claim(&test, run_id).await;
    let result = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(
        result,
        RunOutcome::Failed {
            error: "boom".to_string(),
            will_retry: false
        }
    );
    assert_eq!(find_run(&test, run_id).await.status, "failed");
    test.cleanup().await;
}

#[tokio::test]
async fn flow_step_timeout_applies_to_every_step() {
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        // An explicit step timeout wins over the flow default.
        ctx.step
            .run_with("patient", StepOptions::timeout(Duration::from_secs(10)), || async {
                tokio::time::sleep(Duration::from_millis(120)).await;
                Ok(())
            })
            .await?;
        ctx.step
            .run("slow", || async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(())
            })
            .await
    })
    .step_timeout(Duration::from_millis(40));
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch_without_retry(&test).await;

    let result = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(
        result,
        RunOutcome::Failed {
            error: "Step \"slow\" timed out after 40ms".to_string(),
            will_retry: false
        }
    );
    let steps = find_steps(&test, run_id).await;
    assert_eq!(
        (steps[0].step_name.as_str(), steps[0].status.as_str()),
        ("patient", "completed")
    );
    assert_eq!(
        (steps[1].step_name.as_str(), steps[1].status.as_str()),
        ("slow", "failed")
    );
    assert_eq!(steps[1].error.as_deref(), Some("Step \"slow\" timed out after 40ms"));
    test.cleanup().await;
}

#[derive(Debug, PartialEq)]
struct Reported {
    message: String,
    context: ErrorContext,
    step_timeout: Option<StepTimeoutError>,
    flow_timeout: Option<FlowTimeoutError>,
}

#[tokio::test]
async fn on_error_hook_receives_the_error_and_the_flow_name() {
    let reported: Arc<Mutex<Vec<Reported>>> = Arc::default();
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        match ctx.input["mode"].as_str() {
            Some("fail") => bail!("boom"),
            Some("slow-step") => {
                ctx.step
                    .run_with("slow", StepOptions::timeout(Duration::from_millis(20)), || async {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        Ok(())
                    })
                    .await
            }
            Some("sleep") => ctx.step.sleep("pause", Duration::from_secs(60)).await,
            _ => Ok(()),
        }
    })
    .retry_policy(
        RetryPolicy::new()
            .max_attempts(3)
            .initial_interval(Duration::from_millis(1)),
    )
    .concurrency(Concurrency::Allow);
    let timed_flow = Flow::new("timed-flow", |_ctx: FlowContext| async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
    })
    .timeout(Duration::from_millis(50));
    let config = OpenShopConfig::builder()
        .flow(flow)
        .flow(timed_flow)
        .on_error({
            let reported = reported.clone();
            move |error, context| {
                let reported = reported.clone();
                async move {
                    reported.lock().unwrap().push(Reported {
                        message: error.to_string(),
                        context,
                        step_timeout: error.downcast_ref::<StepTimeoutError>().cloned(),
                        flow_timeout: error.downcast_ref::<FlowTimeoutError>().cloned(),
                    });
                }
            }
        })
        .build()
        .unwrap();
    let test = TestApp::new(config).await;
    let run = |flow: &'static str, mode: &'static str| {
        let test = &test;
        async move {
            let run_id = test
                .app
                .dispatch_flow(DispatchOptions::new(flow, TEST_SHOP))
                .await
                .unwrap()
                .run_id;
            claim(test, run_id).await;
            let options = RunFlowOptions::new(run_id, flow, TEST_SHOP).input(json!({ "mode": mode }));
            run_flow(&test.app, options).await.unwrap()
        }
    };
    let context = |flow: &str| ErrorContext {
        flow: Some(flow.to_string()),
        step: None,
    };

    // Only failures are reported, including the ones that will be retried.
    assert_eq!(run(FLOW, "ok").await, RunOutcome::Completed);
    assert!(matches!(run(FLOW, "sleep").await, RunOutcome::Sleeping { .. }));
    assert!(reported.lock().unwrap().is_empty());

    let failed = run(FLOW, "fail").await;
    assert!(
        matches!(failed, RunOutcome::Failed { will_retry: true, .. }),
        "{failed:?}"
    );
    assert_eq!(
        reported.lock().unwrap().pop(),
        Some(Reported {
            message: "boom".to_string(),
            context: context(FLOW),
            step_timeout: None,
            flow_timeout: None,
        })
    );

    // Typed engine errors can be recovered from the reported error.
    run(FLOW, "slow-step").await;
    assert_eq!(
        reported.lock().unwrap().pop(),
        Some(Reported {
            message: "Step \"slow\" timed out after 20ms".to_string(),
            context: context(FLOW),
            step_timeout: Some(StepTimeoutError {
                step_name: "slow".to_string(),
                timeout_ms: 20
            }),
            flow_timeout: None,
        })
    );

    run("timed-flow", "any").await;
    assert_eq!(
        reported.lock().unwrap().pop(),
        Some(Reported {
            message: "Flow \"timed-flow\" timed out after 50ms".to_string(),
            context: context("timed-flow"),
            step_timeout: None,
            flow_timeout: Some(FlowTimeoutError {
                flow_name: "timed-flow".to_string(),
                timeout_ms: 50
            }),
        })
    );
    assert!(reported.lock().unwrap().is_empty());
    test.cleanup().await;
}

fn exploding_hook(_error: Arc<openshop::Error>, _context: ErrorContext) -> std::future::Ready<()> {
    panic!("hook exploded before the first await")
}

#[tokio::test]
async fn panicking_on_error_hook_does_not_change_the_outcome() {
    let failing = || Flow::new(FLOW, |_ctx: FlowContext| async move { bail!("boom") });
    let configs = [
        OpenShopConfig::builder()
            .flow(failing())
            .on_error(|_error, _context| async { panic!("hook exploded") }),
        OpenShopConfig::builder().flow(failing()).on_error(exploding_hook),
    ];

    for config in configs {
        let test = TestApp::new(config.build().unwrap()).await;
        let run_id = dispatch_without_retry(&test).await;

        let result = run_flow(&test.app, options(run_id)).await.unwrap();
        assert_eq!(
            result,
            RunOutcome::Failed {
                error: "boom".to_string(),
                will_retry: false
            }
        );
        assert_eq!(find_run(&test, run_id).await.status, "failed");
        test.cleanup().await;
    }
}

#[tokio::test]
async fn panicking_flow_is_recorded_as_failed() {
    let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
        let items: Vec<u32> = ctx.step.run("load", || async { Ok(Vec::new()) }).await?;
        let first = items[0];
        ctx.logger.info_with("unreachable", json!({ "first": first }));
        Ok(())
    });
    let test = TestApp::new(create_config([flow])).await;
    let run_id = dispatch_without_retry(&test).await;

    let result = run_flow(&test.app, options(run_id)).await.unwrap();
    let RunOutcome::Failed { error, will_retry } = result else {
        panic!("expected a failure, got {result:?}");
    };
    assert!(error.starts_with("Flow panicked: index out of bounds"), "{error}");
    assert!(!will_retry);

    let run = find_run(&test, run_id).await;
    assert_eq!(run.status, "failed");
    assert_eq!(run.error.as_deref(), Some(error.as_str()));
    assert!(run.completed_at.is_some());
    assert!(
        !cancel_run(&run_id.to_string()),
        "the token is forgotten after a panic too"
    );

    let logs = find_logs(&test, run_id).await;
    assert_eq!(logs.last().map(|log| log.level.as_str()), Some("error"));
    test.cleanup().await;
}

fn explode(_ctx: FlowContext) -> std::future::Ready<openshop::Result<()>> {
    panic!("exploded before the first await")
}

#[tokio::test]
async fn flow_panicking_before_returning_its_future_is_recorded_as_failed() {
    let test = TestApp::new(create_config([Flow::new(FLOW, explode)])).await;
    let run_id = dispatch_without_retry(&test).await;

    let result = run_flow(&test.app, options(run_id)).await.unwrap();
    assert_eq!(
        result,
        RunOutcome::Failed {
            error: "Flow panicked: exploded before the first await".to_string(),
            will_retry: false
        }
    );
    assert_eq!(find_run(&test, run_id).await.status, "failed");
    test.cleanup().await;
}
