mod common;

use std::time::Duration;

use chrono::{DateTime, Utc};
use common::{TEST_SHOP, TestApp, create_config, noop_flow};
use openshop::db::schema::FlowRun;
use openshop::{Concurrency, DispatchFlowParams, DispatchOptions, FlowConcurrencyError, PartialRetryPolicy, json};
use uuid::Uuid;

async fn find_run(test: &TestApp, run_id: Uuid) -> FlowRun {
    sqlx::query_as("SELECT * FROM flow_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(&test.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn creates_a_pending_flow_run() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let dispatched = test
        .app
        .dispatch_flow(DispatchFlowParams::new("test-flow", TEST_SHOP))
        .await
        .unwrap();
    assert_eq!(dispatched.status, "pending");

    let run = find_run(&test, dispatched.run_id).await;
    assert_eq!(run.status, "pending");
    assert_eq!(run.flow_name, "test-flow");
    assert_eq!(run.shop, TEST_SHOP);
    assert_eq!(run.app_handle, "default");
    assert_eq!(run.input, Some(json!({})));
    assert_eq!(run.attempts, 0);
    test.cleanup().await;
}

#[tokio::test]
async fn stores_input_and_app_handle() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let dispatched = test
        .app
        .dispatch_flow(
            DispatchFlowParams::new("test-flow", TEST_SHOP)
                .input(json!({ "source": "config", "z": 1, "a": 2 }))
                .shopify_app("client-a")
                .parent_run_id("parent-1"),
        )
        .await
        .unwrap();

    let run = find_run(&test, dispatched.run_id).await;
    assert_eq!(run.input, Some(json!({ "source": "config", "z": 1, "a": 2 })));
    assert_eq!(run.app_handle, "client-a");
    assert_eq!(run.parent_run_id.as_deref(), Some("parent-1"));
    test.cleanup().await;
}

#[tokio::test]
async fn fails_on_unknown_flow() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    for flow_name in ["unknown", "constructor", "toString", "__proto__"] {
        let error = test
            .app
            .dispatch_flow(DispatchFlowParams::new(flow_name, TEST_SHOP))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("Flow \"{flow_name}\" not found. Available: test-flow")
        );
    }
    test.cleanup().await;
}

#[tokio::test]
async fn concurrency_reject_fails_when_flow_already_active() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let first = test
        .app
        .dispatch_flow(DispatchFlowParams::new("test-flow", TEST_SHOP))
        .await
        .unwrap();

    let error = test
        .app
        .dispatch_flow(DispatchFlowParams::new("test-flow", TEST_SHOP))
        .await
        .unwrap_err();
    let conflict = error.downcast_ref::<FlowConcurrencyError>().expect("concurrency error");
    assert_eq!(conflict.flow_name, "test-flow");
    assert_eq!(conflict.shop, TEST_SHOP);
    assert_eq!(conflict.existing_run_id, first.run_id.to_string());

    // Other shops and other apps are independent.
    test.app
        .dispatch_flow(DispatchFlowParams::new("test-flow", "other.myshopify.com"))
        .await
        .unwrap();
    test.app
        .dispatch_flow(DispatchFlowParams::new("test-flow", TEST_SHOP).shopify_app("client-b"))
        .await
        .unwrap();
    test.cleanup().await;
}

#[tokio::test]
async fn concurrent_dispatches_cannot_both_pass() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let attempts = (0..8).map(|_| {
        let app = test.app.clone();
        tokio::spawn(async move { app.dispatch_flow(DispatchFlowParams::new("test-flow", TEST_SHOP)).await })
    });
    let mut accepted = 0;
    for attempt in attempts {
        if attempt.await.unwrap().is_ok() {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 1);
    test.cleanup().await;
}

#[tokio::test]
async fn concurrency_allow_permits_multiple_runs() {
    let flow = noop_flow("concurrent-flow").concurrency(Concurrency::Allow);
    let test = TestApp::new(create_config([flow])).await;
    let a = test
        .app
        .dispatch_flow(DispatchFlowParams::new("concurrent-flow", TEST_SHOP))
        .await
        .unwrap();
    let b = test
        .app
        .dispatch_flow(DispatchFlowParams::new("concurrent-flow", TEST_SHOP))
        .await
        .unwrap();
    assert_ne!(a.run_id, b.run_id);
    test.cleanup().await;
}

#[tokio::test]
async fn sets_deadline_from_flow_timeout() {
    let flow = noop_flow("timeout-flow").timeout(Duration::from_millis(30_000));
    let test = TestApp::new(create_config([flow, noop_flow("no-timeout")])).await;
    let before = Utc::now();
    let dispatched = test
        .app
        .dispatch_flow(DispatchFlowParams::new("timeout-flow", TEST_SHOP))
        .await
        .unwrap();
    let deadline: DateTime<Utc> = find_run(&test, dispatched.run_id).await.deadline_at.expect("deadline");
    let delta = (deadline - before).num_milliseconds();
    assert!((29_000..=31_000).contains(&delta), "{delta}");

    let dispatched = test
        .app
        .dispatch_flow(DispatchFlowParams::new("no-timeout", TEST_SHOP))
        .await
        .unwrap();
    assert!(find_run(&test, dispatched.run_id).await.deadline_at.is_none());
    test.cleanup().await;
}

#[tokio::test]
async fn stores_the_merged_retry_policy() {
    let flow = noop_flow("test-flow").retry_policy(PartialRetryPolicy::new().max_attempts(5));
    let config = openshop::OpenShopConfig::builder()
        .flow(flow)
        .retry_policy(PartialRetryPolicy::new().max_attempts(9).initial_interval_ms(250))
        .build()
        .unwrap();
    let test = TestApp::new(config).await;

    let dispatched = test
        .app
        .dispatch_flow(DispatchFlowParams::new("test-flow", TEST_SHOP))
        .await
        .unwrap();
    assert_eq!(
        find_run(&test, dispatched.run_id).await.retry_policy,
        Some(json!({ "maxAttempts": 5, "initialIntervalMs": 250, "backoffCoefficient": 2.0, "maxIntervalMs": 30000 }))
    );

    let dispatched = test
        .app
        .dispatch_flow(
            DispatchFlowParams::new("test-flow", "other.myshopify.com").options(DispatchOptions {
                retry_policy: Some(PartialRetryPolicy::new().max_attempts(0)),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let policy = find_run(&test, dispatched.run_id).await.retry_policy.unwrap();
    assert_eq!(policy["maxAttempts"], 0);
    test.cleanup().await;
}

#[tokio::test]
async fn delayed_dispatch_sets_future_available_at() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let before = Utc::now();
    let dispatched = test
        .app
        .dispatch_flow(
            DispatchFlowParams::new("test-flow", TEST_SHOP).options(DispatchOptions {
                delay_ms: Some(10_000),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    let available_at = find_run(&test, dispatched.run_id).await.available_at.unwrap();
    assert!(available_at > before + chrono::Duration::milliseconds(5_000));
    test.cleanup().await;
}
