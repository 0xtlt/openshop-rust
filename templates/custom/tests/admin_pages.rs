use openshop::testing::{TestContext, toast, toast_error};
use openshop::{Outcome, Value, json};

use openshop_template_custom::APP;
use openshop_template_custom::admin::reviews::{ReviewList, ReviewsPage};

#[openshop::test(APP)]
async fn lists_the_reviews_page_in_navigation(ctx: &TestContext) {
    let body: Value = ctx.get("/api/pages/custom").send().await.unwrap().json().await.unwrap();

    // The page declares its own entry: `#[admin_page("/reviews", nav = "Reviews")]`.
    assert_eq!(
        body["navigation"],
        json!([{ "label": "Reviews", "path": ReviewsPage::PATH }])
    );
    assert_eq!(
        body["pages"],
        json!([{ "id": "reviews", "path": "/reviews", "allowed": true }])
    );

    // The menu of the admin: the built-in pages, then the page of the application.
    let page = ctx.page(ReviewsPage::PATH).send().await.unwrap().text().await.unwrap();
    assert!(
        page.contains(r#"<a href="/reviews">Reviews</a></ui-nav-menu>"#),
        "{page}"
    );
}

#[openshop::test(APP)]
async fn renders_the_reviews_and_saves_one(ctx: &TestContext) {
    let listed: ReviewList = ctx.call_loader(ReviewsPage::PATH, "listReviews", ()).await.unwrap();
    assert_eq!(listed.reviews[0].shop, ctx.shop);

    let page = ctx.page(ReviewsPage::PATH).send().await.unwrap().text().await.unwrap();
    assert!(page.contains(r#"<s-section heading="Example review">"#), "{page}");

    let saved: Outcome = ctx
        .call_action(
            ReviewsPage::PATH,
            "saveReview",
            json!({ "title": "Saved from custom admin" }),
        )
        .await
        .unwrap();
    assert_eq!(saved, Outcome::toast("Saved Saved from custom admin"));

    // The form of the page: the toast, and an empty title refused under its field.
    let sent = ctx
        .submit(
            ReviewsPage::PATH,
            &[("_action", ReviewsPage::SAVE_REVIEW), ("title", "Great")],
        )
        .send()
        .await
        .unwrap();
    assert_eq!(sent.status(), 200);
    assert_eq!(toast(&sent).as_deref(), Some("Saved Great"));
    assert_eq!(toast_error(&sent), None);
    let refused = ctx
        .submit(
            ReviewsPage::PATH,
            &[("_action", ReviewsPage::SAVE_REVIEW), ("title", "")],
        )
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 422);
    let page = refused.text().await.unwrap();
    assert!(page.contains(r#"value="" error="Required""#), "{page}");
}
