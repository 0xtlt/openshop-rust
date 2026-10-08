use openshop::json;
use openshop::testing::TestContext;
use openshop_demo::APP;

#[openshop::test(APP)]
async fn reviews_returns_the_liquid_template_with_its_variables(ctx: &TestContext) {
    let response = ctx.proxy().get("/reviews").send().await.unwrap();

    assert_eq!(response.status, 200);
    assert!(response.content_type.contains("application/liquid"));
    // The variable the handler assigns, then the file as it is written.
    assert!(
        response
            .text
            .starts_with(&format!("{{%- assign app_shop = \"{}\" -%}}\n", ctx.shop)),
        "{}",
        response.text
    );
    assert!(response.text.contains("Customer Reviews for {{ shop.name }}"));
    assert!(response.text.contains("from {{ app_shop }}"));
}

#[openshop::test(APP)]
async fn api_reviews_returns_json(ctx: &TestContext) {
    let response = ctx.proxy().get("/api/reviews").send().await.unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body["shop"], ctx.shop);
    assert_eq!(response.body["page"], 1);
    assert_eq!(response.body["reviews"].as_array().unwrap().len(), 2);
}

#[openshop::test(APP)]
async fn api_reviews_reads_its_typed_query(ctx: &TestContext) {
    let second = ctx
        .proxy()
        .get("/api/reviews")
        .qs(&[("page", "2")])
        .send()
        .await
        .unwrap();
    assert_eq!(second.body["page"], 2);

    // A query that does not fit the type is refused before the handler runs.
    let refused = ctx
        .proxy()
        .get("/api/reviews")
        .qs(&[("page", "two")])
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status, 422);
}

#[openshop::test(APP)]
async fn posting_a_review_as_anonymous_returns_an_error(ctx: &TestContext) {
    let response = ctx
        .proxy()
        .post("/api/reviews")
        .json(json!({ "text": "Spam", "rating": 1 }))
        .send()
        .await
        .unwrap();

    assert!(response.body["error"].as_str().unwrap().contains("logged in"));
}

#[openshop::test(APP)]
async fn posting_a_review_as_an_authenticated_customer(ctx: &TestContext) {
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
}

#[openshop::test(APP)]
async fn a_review_that_is_not_one_is_refused(ctx: &TestContext) {
    let response = ctx
        .proxy()
        .post("/api/reviews")
        .as_customer("789456")
        .json(json!({ "text": "No rating" }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status, 422);
}
