mod common;

use openshop::{Value, json};

#[tokio::test]
async fn lists_the_reviews_page_in_navigation() {
    let ctx = common::context().await;
    let body: Value = ctx.get("/api/pages/custom").send().await.unwrap().json().await.unwrap();

    assert_eq!(body["navigation"], json!([{ "label": "Reviews", "path": "/reviews" }]));
    assert_eq!(
        body["pages"],
        json!([{ "id": "reviews", "path": "/reviews", "allowed": true }])
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn calls_the_reviews_loader_and_action() {
    let ctx = common::context().await;

    let listed: Value = ctx
        .post("/api/pages/custom/reviews/_rpc/loader/listReviews")
        .json(&json!({ "input": null }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["reviews"][0]["shop"], ctx.shop);

    let saved = ctx
        .post("/api/pages/custom/reviews/_rpc/action/saveReview")
        .json(&json!({ "input": { "title": "Saved from custom admin" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    assert_eq!(
        saved.json::<Value>().await.unwrap(),
        json!({ "ok": true, "title": "Saved from custom admin", "savedBy": "1" })
    );

    let rejected = ctx
        .post("/api/pages/custom/reviews/_rpc/action/saveReview")
        .json(&json!({ "input": { "title": "" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 422);
    assert_eq!(rejected.json::<Value>().await.unwrap()["code"], "INVALID_INPUT");
    ctx.shutdown().await.unwrap();
}
