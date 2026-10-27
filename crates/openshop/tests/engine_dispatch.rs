mod common;

use std::time::Duration;

use chrono::{DateTime, Utc};
use common::{TEST_SHOP, TestApp, create_config, find_run, noop_flow};
use openshop::builder::InputSchema;
use openshop::{
    Concurrency, DispatchOptions, FlowConcurrencyError, InvalidFlowInput, RetryPolicy, UnknownFlowError, Value, json,
};

#[tokio::test]
async fn creates_a_pending_flow_run() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let dispatched = test
        .app
        .dispatch_flow(DispatchOptions::new("test-flow", TEST_SHOP))
        .await
        .unwrap();
    assert_eq!(dispatched.status, "pending");

    let run = find_run(&test.db, dispatched.run_id).await;
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
            DispatchOptions::new("test-flow", TEST_SHOP)
                .input(json!({ "source": "config", "z": 1, "a": 2 }))
                .shopify_app("client-a")
                .parent_run_id("parent-1"),
        )
        .await
        .unwrap();

    let run = find_run(&test.db, dispatched.run_id).await;
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
            .dispatch_flow(DispatchOptions::new(flow_name, TEST_SHOP))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("Flow \"{flow_name}\" not found. Available: test-flow")
        );
    }
    test.cleanup().await;
}

fn order_flow() -> openshop::builder::Flow {
    noop_flow("order-flow").input_schema(InputSchema::custom(
        json!({ "type": "object", "required": ["orderId"] }),
        |value| match value.get("orderId") {
            Some(Value::Number(_)) => Ok(value),
            Some(Value::String(text)) if text == "explode" => panic!("validator exploded"),
            _ => Err("orderId must be a number".to_string()),
        },
    ))
}

async fn run_count(test: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM flow_runs")
        .fetch_one(&test.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn refuses_input_that_fails_the_input_schema_before_queueing_a_run() {
    let test = TestApp::new(create_config([order_flow()])).await;
    for input in [None, Some(json!({})), Some(json!({ "orderId": "12" }))] {
        let mut options = DispatchOptions::new("order-flow", TEST_SHOP);
        options.input = input;
        let error = test.app.dispatch_flow(options).await.unwrap_err();
        let invalid = error.downcast_ref::<InvalidFlowInput>().expect("typed error");
        assert_eq!(invalid.flow_name, "order-flow");
        assert_eq!(invalid.summary, "orderId must be a number");
        assert_eq!(
            error.to_string(),
            "Invalid input for flow \"order-flow\": orderId must be a number"
        );
    }
    assert_eq!(run_count(&test).await, 0);

    let queued = test
        .app
        .dispatch_flow(DispatchOptions::new("order-flow", TEST_SHOP).input(json!({ "orderId": 12 })))
        .await
        .unwrap();
    assert_eq!(
        find_run(&test.db, queued.run_id).await.input,
        Some(json!({ "orderId": 12 }))
    );
    test.cleanup().await;
}

#[tokio::test]
async fn a_validator_that_panics_fails_the_dispatch_not_the_caller() {
    let test = TestApp::new(create_config([order_flow()])).await;
    let error = test
        .app
        .dispatch_flow(DispatchOptions::new("order-flow", TEST_SHOP).input(json!({ "orderId": "explode" })))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<InvalidFlowInput>().is_none());
    assert_eq!(error.to_string(), "The input validator of flow \"order-flow\" panicked");
    assert_eq!(run_count(&test).await, 0);
    test.cleanup().await;
}

#[tokio::test]
async fn an_unknown_flow_is_a_typed_error() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let error = test
        .app
        .dispatch_flow(DispatchOptions::new("nope", TEST_SHOP))
        .await
        .unwrap_err();
    let unknown = error.downcast_ref::<UnknownFlowError>().expect("typed error");
    assert_eq!(unknown.flow_name, "nope");
    assert_eq!(unknown.available, ["test-flow"]);
    test.cleanup().await;
}

#[tokio::test]
async fn concurrency_reject_fails_when_flow_already_active() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let first = test
        .app
        .dispatch_flow(DispatchOptions::new("test-flow", TEST_SHOP))
        .await
        .unwrap();

    let error = test
        .app
        .dispatch_flow(DispatchOptions::new("test-flow", TEST_SHOP))
        .await
        .unwrap_err();
    let conflict = error.downcast_ref::<FlowConcurrencyError>().expect("concurrency error");
    assert_eq!(conflict.flow_name, "test-flow");
    assert_eq!(conflict.shop, TEST_SHOP);
    assert_eq!(conflict.existing_run_id, first.run_id.to_string());

    // Other shops and other apps are independent.
    test.app
        .dispatch_flow(DispatchOptions::new("test-flow", "other.myshopify.com"))
        .await
        .unwrap();
    test.app
        .dispatch_flow(DispatchOptions::new("test-flow", TEST_SHOP).shopify_app("client-b"))
        .await
        .unwrap();
    test.cleanup().await;
}

#[tokio::test]
async fn concurrent_dispatches_cannot_both_pass() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let attempts = (0..8).map(|_| {
        let app = test.app.clone();
        tokio::spawn(async move { app.dispatch_flow(DispatchOptions::new("test-flow", TEST_SHOP)).await })
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
        .dispatch_flow(DispatchOptions::new("concurrent-flow", TEST_SHOP))
        .await
        .unwrap();
    let b = test
        .app
        .dispatch_flow(DispatchOptions::new("concurrent-flow", TEST_SHOP))
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
        .dispatch_flow(DispatchOptions::new("timeout-flow", TEST_SHOP))
        .await
        .unwrap();
    let deadline: DateTime<Utc> = find_run(&test.db, dispatched.run_id)
        .await
        .deadline_at
        .expect("deadline");
    let delta = (deadline - before).num_milliseconds();
    assert!((29_000..=31_000).contains(&delta), "{delta}");

    let dispatched = test
        .app
        .dispatch_flow(DispatchOptions::new("no-timeout", TEST_SHOP))
        .await
        .unwrap();
    assert!(find_run(&test.db, dispatched.run_id).await.deadline_at.is_none());
    test.cleanup().await;
}

#[tokio::test]
async fn stores_the_merged_retry_policy() {
    let flow = noop_flow("test-flow").retry_policy(RetryPolicy::new().max_attempts(5));
    let config = openshop::OpenShopConfig::builder()
        .flow(flow)
        .retry_policy(
            RetryPolicy::new()
                .max_attempts(9)
                .initial_interval(Duration::from_millis(250)),
        )
        .build()
        .unwrap();
    let test = TestApp::new(config).await;

    let dispatched = test
        .app
        .dispatch_flow(DispatchOptions::new("test-flow", TEST_SHOP))
        .await
        .unwrap();
    assert_eq!(
        find_run(&test.db, dispatched.run_id).await.retry_policy,
        Some(json!({ "maxAttempts": 5, "initialIntervalMs": 250, "backoffCoefficient": 2.0, "maxIntervalMs": 30000 }))
    );

    let dispatched = test
        .app
        .dispatch_flow(
            DispatchOptions::new("test-flow", "other.myshopify.com").retry_policy(RetryPolicy::new().max_attempts(0)),
        )
        .await
        .unwrap();
    let policy = find_run(&test.db, dispatched.run_id).await.retry_policy.unwrap();
    assert_eq!(policy["maxAttempts"], 0);
    test.cleanup().await;
}

#[tokio::test]
async fn delayed_dispatch_sets_future_available_at() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let before = Utc::now();
    let dispatched = test
        .app
        .dispatch_flow(DispatchOptions::new("test-flow", TEST_SHOP).delay(Duration::from_millis(10_000)))
        .await
        .unwrap();

    let available_at = find_run(&test.db, dispatched.run_id).await.available_at.unwrap();
    assert!(available_at > before + chrono::Duration::milliseconds(5_000));
    test.cleanup().await;
}
