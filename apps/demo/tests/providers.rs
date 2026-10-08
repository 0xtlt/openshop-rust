use openshop::testing::TestContext;
use openshop::{Value, json};
use openshop_demo::APP;
use openshop_demo::providers::warehouse::Warehouse;

#[openshop::test(APP)]
async fn lists_providers_with_their_fields(ctx: &TestContext) {
    let response = ctx.get("/api/providers").send().await.unwrap();
    assert_eq!(response.status(), 200);

    let providers: Vec<Value> = response.json().await.unwrap();
    assert_eq!(providers.len(), 1);
    let warehouse = &providers[0];
    assert_eq!(warehouse["name"], Warehouse::NAME);
    assert_eq!(warehouse["configured"], false);
    assert_eq!(warehouse["config"], json!({}));
    assert_eq!(
        warehouse["fields"]["apiKey"],
        json!({ "type": "password", "label": "API Key", "placeholder": "wh_live_...", "hasValue": false })
    );
    // A field without a label is called by its name.
    assert_eq!(warehouse["fields"]["batchSize"]["label"], "Batch size");
}

#[openshop::test(APP)]
async fn saves_and_checks_the_warehouse_configuration(ctx: &TestContext) {
    let saved = ctx
        .put("/api/providers/warehouse")
        .json(&json!({
            "config": { "apiUrl": "https://warehouse.example.com/api", "apiKey": "wh_live_1", "batchSize": "100" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);

    let checked = ctx.post("/api/providers/warehouse/check").send().await.unwrap();
    assert_eq!(checked.json::<Value>().await.unwrap(), json!({ "ok": true }));

    let providers: Vec<Value> = ctx.get("/api/providers").send().await.unwrap().json().await.unwrap();
    let warehouse = &providers[0];
    assert_eq!(warehouse["configured"], true);
    assert_eq!(warehouse["lastCheckOk"], true);
    assert_eq!(
        warehouse["config"],
        json!({ "apiUrl": "https://warehouse.example.com/api", "batchSize": 100 })
    );
    assert_eq!(warehouse["fields"]["apiKey"]["hasValue"], true);
}

#[openshop::test(APP)]
async fn refuses_a_batch_size_out_of_range(ctx: &TestContext) {
    let refused = ctx
        .put("/api/providers/warehouse")
        .json(&json!({
            "config": { "apiUrl": "https://warehouse.example.com/api", "apiKey": "wh_live_1", "batchSize": "5000" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 400);
}
