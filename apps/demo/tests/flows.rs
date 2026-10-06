mod common;

use openshop::engine::RunFlowResult;
use openshop::json;
use openshop_demo::providers::warehouse::Warehouse;

#[tokio::test]
async fn lists_flows() {
    let ctx = common::context().await;
    let response = ctx.get("/api/flows").send().await.unwrap();
    assert_eq!(response.status(), 200);

    let flows: Vec<openshop::Value> = response.json().await.unwrap();
    let names: Vec<&str> = flows.iter().filter_map(|flow| flow["name"].as_str()).collect();
    assert_eq!(names, vec!["syncOrders", "countVariants"]);
    assert_eq!(flows[0]["crons"].as_array().unwrap().len(), 2);
    assert_eq!(flows[0]["inputSchema"]["properties"]["limit"]["minimum"], 1);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn runs_sync_orders_with_faked_warehouse() {
    let ctx = common::context().await;
    let push = ctx.fakes.for_call(Warehouse::PUSH);
    push.returns(false);

    let result = ctx.run_flow("syncOrders", json!({ "limit": 5 })).await.unwrap();

    assert_eq!(result, RunFlowResult::Completed);
    assert!(push.called());
    assert_eq!(push.call_count(), 1);
    let records = push.last_call().unwrap().args;
    assert_eq!(records[0]["ref"], "#1001");
    assert_eq!(records[0]["amount"], "EUR");
    assert_eq!(records[0]["lineCount"], 1);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn handles_warehouse_failure() {
    let ctx = common::context().await;
    ctx.fakes.for_call(Warehouse::PUSH).rejects("503 Service Unavailable");

    let result = ctx.run_flow("syncOrders", json!({ "limit": 1 })).await.unwrap();

    assert!(matches!(
        result,
        RunFlowResult::Failed { ref error, .. } if error.contains("503 Service Unavailable")
    ));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejects_invalid_input() {
    let ctx = common::context().await;

    for input in [json!({}), json!({ "limit": 0 }), json!({ "limit": "five" })] {
        let result = ctx.run_flow("syncOrders", input).await.unwrap();
        assert!(matches!(
            result,
            RunFlowResult::Failed { ref error, .. } if error.starts_with("Invalid input for flow \"syncOrders\"")
        ));
    }
    assert!(!ctx.fakes.for_call(Warehouse::PUSH).called());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn runs_count_variants() {
    let ctx = common::context().await;

    let result = ctx
        .run_flow("countVariants", json!({ "productId": "gid://shopify/Product/1" }))
        .await
        .unwrap();
    assert_eq!(result, RunFlowResult::Completed);

    let (summary,) = build_summary_output(&ctx).await;
    assert_eq!(summary["title"], "Test product");
    assert_eq!(summary["totalVariants"], 1);
    assert_eq!(summary["imageCount"], 0);
    assert_eq!(summary["variantSkus"][0]["sku"], "SKU-1");
    ctx.shutdown().await.unwrap();
}

async fn build_summary_output(ctx: &openshop::testing::TestContext) -> (openshop::Value,) {
    openshop::sqlx::query_as("SELECT output FROM step_results WHERE step_name = 'build-summary'")
        .fetch_one(&ctx.db)
        .await
        .unwrap()
}
