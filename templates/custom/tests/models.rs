use openshop::Model;
use openshop::testing::TestContext;
use openshop_template_custom::APP;
use openshop_template_custom::models::review::{NewReview, Review};

#[test]
fn the_review_struct_is_the_reviews_table() {
    assert_eq!(
        Review::table().create_sql(),
        "CREATE TABLE IF NOT EXISTS \"reviews\" (\n\
         \x20 \"id\" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,\n\
         \x20 \"shop\" text NOT NULL,\n\
         \x20 \"created_at\" timestamp with time zone DEFAULT now() NOT NULL,\n\
         \x20 \"updated_at\" timestamp with time zone DEFAULT now() NOT NULL,\n\
         \x20 \"product_id\" text NOT NULL,\n\
         \x20 \"customer_id\" text NOT NULL,\n\
         \x20 \"rating\" integer NOT NULL,\n\
         \x20 \"text\" text NOT NULL,\n\
         \x20 \"approved\" boolean DEFAULT false NOT NULL\n\
         );"
    );
}

#[openshop::test(APP)]
async fn reviews_are_read_and_written_per_shop(ctx: &TestContext) {
    // The rows of the shop of the test, as a handler gets them from its context.
    let reviews = Review::of(ctx);

    let review = reviews
        .insert(NewReview {
            product_id: "gid://shopify/Product/1".to_string(),
            customer_id: "gid://shopify/Customer/1".to_string(),
            rating: 5,
            text: "Great product!".to_string(),
            approved: None,
        })
        .await
        .unwrap();
    assert_eq!(review.shop, ctx.shop);
    assert!(!review.approved);

    let best = reviews
        .filter(Review::RATING.gte(4))
        .order_by(Review::CREATED_AT.desc())
        .all()
        .await
        .unwrap();
    assert_eq!(best, std::slice::from_ref(&review));

    let elsewhere = Review::for_shop(&ctx.db, "another-shop.myshopify.com");
    assert_eq!(elsewhere.find(review.id).await.unwrap(), None);
}
