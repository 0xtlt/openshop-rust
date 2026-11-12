use openshop::prelude::*;
use serde::{Deserialize, Serialize};

/// The query of `GET /api/reviews`.
#[derive(Debug, Deserialize)]
pub struct ListReviews {
    pub page: Option<u32>,
}

#[proxy(GET, "/api/reviews")]
pub async fn list_reviews(ctx: ProxyContext, query: ListReviews) -> Result<Value> {
    Ok(json!({
        "shop": ctx.shop,
        "page": query.page.filter(|page| *page > 0).unwrap_or(1),
        "reviews": [
            { "id": 1, "author": "Alice", "rating": 5, "text": "Great product!" },
            { "id": 2, "author": "Bob", "rating": 4, "text": "Very good quality." },
        ],
    }))
}

/// The body of `POST /api/reviews`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SubmittedReview {
    pub text: String,
    pub rating: u8,
}

#[proxy(POST, "/api/reviews")]
pub async fn submit_review(ctx: ProxyContext, review: SubmittedReview) -> Result<Value> {
    let Some(customer_id) = ctx.customer_id else {
        return Ok(json!({ "error": "Must be logged in to submit a review" }));
    };
    Ok(json!({ "ok": true, "shop": ctx.shop, "customerId": customer_id, "review": review }))
}
