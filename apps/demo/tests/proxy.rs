mod common;

use openshop::json;

#[tokio::test]
async fn reviews_returns_liquid() {
    let ctx = common::context().await;
    let response = ctx.proxy().get("/reviews").send().await.unwrap();

    assert_eq!(response.status, 200);
    assert!(response.content_type.contains("application/liquid"));
    assert!(response.text.contains("Customer Reviews for {{ shop.name }}"));
    assert!(response.text.contains(&ctx.shop));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn api_reviews_returns_json() {
    let ctx = common::context().await;
    let response = ctx.proxy().get("/api/reviews").send().await.unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body["shop"], ctx.shop);
    assert_eq!(response.body["page"], 1);
    assert_eq!(response.body["reviews"].as_array().unwrap().len(), 2);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn api_reviews_reads_query_params() {
    let ctx = common::context().await;
    let response = ctx
        .proxy()
        .get("/api/reviews")
        .qs(&[("page", "2")])
        .send()
        .await
        .unwrap();

    assert_eq!(response.body["page"], 2);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn posting_a_review_as_anonymous_returns_an_error() {
    let ctx = common::context().await;
    let response = ctx
        .proxy()
        .post("/api/reviews")
        .json(json!({ "text": "Spam", "rating": 1 }))
        .send()
        .await
        .unwrap();

    assert!(response.body["error"].as_str().unwrap().contains("logged in"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn posting_a_review_as_an_authenticated_customer() {
    let ctx = common::context().await;
    let response = ctx
        .proxy()
        .post("/api/reviews")
        .as_customer("789456")
        .json(json!({ "text": "Great product!", "rating": 5 }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.body["ok"], true);
    assert_eq!(response.body["customerId"], "789456");
    assert_eq!(response.body["shop"], ctx.shop);
    assert_eq!(
        response.body["review"],
        json!({ "text": "Great product!", "rating": 5 })
    );
    ctx.shutdown().await.unwrap();
}
