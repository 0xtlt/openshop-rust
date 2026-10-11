use openshop::testing::TestContext;
use openshop::{Value, json};
use openshop_demo_custom::APP;

#[openshop::test(APP)]
async fn health_returns_ok(ctx: &TestContext) {
    let response = ctx.http.get(ctx.url_for("/health")).send().await.unwrap();
    assert_eq!(response.status(), 200);

    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["timestamp"].is_string());
}

#[openshop::test(APP)]
async fn ping_reaches_a_public_server_route(ctx: &TestContext) {
    let response = ctx.http.get(ctx.url_for("/routes/ping")).send().await.unwrap();
    assert_eq!(response.status(), 200);

    let body: Value = response.json().await.unwrap();
    assert_eq!(body, json!({ "ok": true, "source": "server-route" }));
}
