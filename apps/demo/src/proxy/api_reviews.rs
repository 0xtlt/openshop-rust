use openshop::{ProxyDefinition, json};

pub fn api_reviews() -> ProxyDefinition {
    ProxyDefinition::json()
        .get(|ctx| async move {
            let page = ctx
                .query
                .get("page")
                .and_then(|page| page.parse::<u32>().ok())
                .filter(|page| *page > 0)
                .unwrap_or(1);
            Ok(json!({
                "shop": ctx.shop,
                "page": page,
                "reviews": [
                    { "id": 1, "author": "Alice", "rating": 5, "text": "Great product!" },
                    { "id": 2, "author": "Bob", "rating": 4, "text": "Very good quality." },
                ],
            }))
        })
        .post(|ctx| async move {
            let Some(customer_id) = ctx.customer_id else {
                return Ok(json!({ "error": "Must be logged in to submit a review" }));
            };
            Ok(json!({ "ok": true, "shop": ctx.shop, "customerId": customer_id, "review": ctx.body }))
        })
}
