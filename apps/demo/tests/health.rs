mod common;

use openshop::{Value, json};

#[tokio::test]
async fn health_returns_ok() {
    let ctx = common::context().await;
    let response = ctx.http.get(format!("{}/health", ctx.url)).send().await.unwrap();
    assert_eq!(response.status(), 200);

    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["timestamp"].is_string());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn ping_reaches_a_public_server_route() {
    let ctx = common::context().await;
    let response = ctx.http.get(format!("{}/routes/ping", ctx.url)).send().await.unwrap();
    assert_eq!(response.status(), 200);

    let body: Value = response.json().await.unwrap();
    assert_eq!(body, json!({ "ok": true, "source": "server-route" }));
    ctx.shutdown().await.unwrap();
}
