mod common;

use openshop::testing::TestContext;
use openshop::{FlowRun, RunOutcome, Value, json};

use openshop_template_public::APP;
use openshop_template_public::admin::home::HomePage;
use openshop_template_public::admin::settings::SettingsPage;
use openshop_template_public::flows::check_stock::check_stock;
use openshop_template_public::models::stock_settings::StockSettings;

// The home has no segment of its own in the path of its functions.
const SUMMARY: &str = "/api/pages/custom/_rpc/loader/summary";
const CHECK_NOW: &str = "/api/pages/custom/_rpc/action/checkNow";
const SETTINGS: &str = "/api/pages/custom/settings/_rpc/loader/settings";
const SAVE_SETTINGS: &str = "/api/pages/custom/settings/_rpc/action/saveSettings";

/// Calls a loader or an action as the view of its page does.
async fn call(ctx: &TestContext, function: &str, input: Value) -> (u16, Value) {
    let response = ctx
        .post(function)
        .json(&json!({ "input": input }))
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}

#[openshop::test(APP)]
async fn a_merchant_reaches_the_pages_of_the_application_and_nothing_else(ctx: &TestContext) {
    let builtin: Value = ctx.get("/api/pages").send().await.unwrap().json().await.unwrap();
    assert!(builtin.as_object().unwrap().values().all(|mode| mode == "disabled"));
    // Nothing of the flows, the runs or the crons of the application.
    for path in ["/api/flows", "/api/runs", "/api/crons"] {
        assert_eq!(ctx.get(path).send().await.unwrap().status(), 404, "{path}");
    }

    let custom: Value = ctx.get("/api/pages/custom").send().await.unwrap().json().await.unwrap();
    // The home has no entry: the name of the app opens it.
    assert_eq!(
        custom["navigation"],
        json!([{ "label": "Settings", "path": SettingsPage::PATH }])
    );
    assert_eq!(
        custom["pages"],
        json!([
            { "id": "", "path": HomePage::PATH, "allowed": true },
            { "id": "settings", "path": SettingsPage::PATH, "allowed": true },
        ])
    );
}

#[openshop::test(APP)]
async fn the_home_shows_what_the_last_check_found(ctx: &TestContext) {
    common::answer_shopify(ctx);

    let (_, before) = call(ctx, SUMMARY, Value::Null).await;
    assert_eq!(
        before,
        json!({ "threshold": 5, "checking": false, "checkedAt": null, "lowStock": [] })
    );

    let outcome = ctx.run_flow(check_stock::NAME, json!({})).await.unwrap();
    assert_eq!(outcome, RunOutcome::Completed);

    let (_, after) = call(ctx, SUMMARY, Value::Null).await;
    assert!(after["checkedAt"].is_string());
    assert_eq!(
        after["lowStock"],
        json!([
            { "name": "Canvas tote - Natural", "sku": null, "available": 0 },
            { "name": "Linen shirt - M", "sku": "SHIRT-M", "available": 2 },
        ])
    );
}

#[openshop::test(APP)]
async fn check_now_queues_a_check_for_the_shop_of_the_session(ctx: &TestContext) {
    let (status, _) = call(ctx, CHECK_NOW, Value::Null).await;
    assert_eq!(status, 200);

    let queued = FlowRun::of(ctx).all().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].flow_name, check_stock::NAME);
    assert_eq!(queued[0].status, "pending");

    let (_, summary) = call(ctx, SUMMARY, Value::Null).await;
    assert_eq!(summary["checking"], true);
}

#[openshop::test(APP)]
async fn a_merchant_saves_the_threshold_of_their_shop(ctx: &TestContext) {
    let (_, defaults) = call(ctx, SETTINGS, Value::Null).await;
    assert_eq!(defaults, json!({ "threshold": StockSettings::DEFAULT_THRESHOLD }));

    for threshold in [12, 20] {
        let saved = call(ctx, SAVE_SETTINGS, json!({ "threshold": threshold })).await;
        assert_eq!(saved, (200, json!({ "threshold": threshold })));
    }
    // One row per shop, however many times its merchant saves.
    let rows = StockSettings::of(ctx).all().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].threshold, 20);
    assert_eq!(call(ctx, SETTINGS, Value::Null).await.1, json!({ "threshold": 20 }));

    let (status, refused) = call(ctx, SAVE_SETTINGS, json!({ "threshold": 0 })).await;
    assert_eq!(status, 422);
    assert_eq!(refused["code"], "INVALID_INPUT");
}
