mod common;

use openshop::testing::TestContext;
use openshop::{RunOutcome, Value, json};
use openshop_demo::APP;
use openshop_demo::flows::count_variants::count_variants;
use openshop_demo::flows::sync_orders::sync_orders;
use openshop_demo::providers::warehouse::Warehouse;

#[openshop::test(APP)]
async fn lists_flows(ctx: &TestContext) {
    let response = ctx.get("/api/flows").send().await.unwrap();
    assert_eq!(response.status(), 200);

    let flows: Vec<Value> = response.json().await.unwrap();
    let names: Vec<&str> = flows.iter().filter_map(|flow| flow["name"].as_str()).collect();
    assert_eq!(names, vec![sync_orders::NAME, count_variants::NAME]);
    assert_eq!(flows[0]["crons"].as_array().unwrap().len(), 2);
    assert_eq!(flows[0]["inputSchema"]["properties"]["limit"]["minimum"], 1);
}

#[openshop::test(APP)]
async fn runs_sync_orders_with_faked_warehouse(ctx: &TestContext) {
    common::answer_shopify(ctx);
    let push = ctx.fakes.for_call(Warehouse::PUSH);
    push.returns(false);

    let outcome = ctx.run_flow(sync_orders::NAME, json!({ "limit": 5 })).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(push.call_count(), 1);
    // The arguments come back as the method receives them.
    let records = push.last_call().unwrap().args;
    assert_eq!(records[0].reference, "#1001");
    assert_eq!(records[0].amount, "42.00");
    assert_eq!(records[0].line_count, 1);
}

#[openshop::test(APP)]
async fn handles_warehouse_failure(ctx: &TestContext) {
    common::answer_shopify(ctx);
    ctx.fakes.for_call(Warehouse::PUSH).rejects("503 Service Unavailable");

    let outcome = ctx.run_flow(sync_orders::NAME, json!({ "limit": 1 })).await.unwrap();

    assert!(matches!(
        outcome,
        RunOutcome::Failed { ref error, .. } if error.contains("503 Service Unavailable")
    ));
}

#[openshop::test(APP)]
async fn rejects_invalid_input(ctx: &TestContext) {
    common::answer_shopify(ctx);

    for input in [json!({}), json!({ "limit": 0 }), json!({ "limit": "five" })] {
        let outcome = ctx.run_flow(sync_orders::NAME, input).await.unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Failed { ref error, .. } if error.starts_with("Invalid input for flow \"syncOrders\"")
        ));
    }
    assert!(!ctx.fakes.for_call(Warehouse::PUSH).called());
}

#[openshop::test(APP)]
async fn runs_count_variants(ctx: &TestContext) {
    common::answer_shopify(ctx);

    let outcome = ctx
        .run_flow(count_variants::NAME, json!({ "productId": "gid://shopify/Product/1" }))
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Completed);

    let (summary,): (Value,) =
        openshop::sqlx::query_as("SELECT output FROM step_results WHERE step_name = 'build-summary'")
            .fetch_one(&ctx.db)
            .await
            .unwrap();
    assert_eq!(summary["title"], "Test product");
    assert_eq!(summary["totalVariants"], 1);
    assert_eq!(summary["imageCount"], 0);
    assert_eq!(summary["variantSkus"][0]["sku"], "SKU-1");
}
