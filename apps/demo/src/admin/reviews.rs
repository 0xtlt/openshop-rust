//! Server side of the custom `/reviews` admin page.

use openshop::{AdminServerContext, Result, Value, admin_page, json};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveReviewInput {
    #[schemars(length(min = 1))]
    pub title: String,
}

pub struct ReviewsPage;

#[admin_page("/reviews")]
impl ReviewsPage {
    #[access]
    async fn signed_in(ctx: AdminServerContext) -> Result<bool> {
        Ok(!ctx.actor.id.is_empty())
    }

    #[loader]
    async fn list_reviews(ctx: AdminServerContext) -> Result<Value> {
        Ok(json!({
            "reviews": [{ "id": "example", "title": "Example review", "shop": ctx.shop }],
        }))
    }

    #[action(authorize = Self::signed_in)]
    async fn save_review(ctx: AdminServerContext, input: SaveReviewInput) -> Result<Value> {
        Ok(json!({ "ok": true, "title": input.title, "savedBy": ctx.actor.id }))
    }
}
