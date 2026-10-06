//! Server side of the custom `/reviews` admin page.

use openshop::{AdminPage, define_admin_page_access, define_typed_admin_action, json};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveReviewInput {
    #[schemars(length(min = 1))]
    pub title: String,
}

pub fn page() -> AdminPage {
    let page_access = define_admin_page_access(|ctx| async move { Ok(!ctx.actor.id.is_empty()) });

    AdminPage::new("/reviews")
        .access(page_access.clone())
        .loader("listReviews", |ctx, _input| async move {
            Ok(json!({
                "reviews": [{ "id": "example", "title": "Example review", "shop": ctx.shop }],
            }))
        })
        .function(
            "saveReview",
            define_typed_admin_action(|ctx, input: SaveReviewInput| async move {
                Ok(json!({ "ok": true, "title": input.title, "savedBy": ctx.actor.id }))
            })
            .authorize(page_access),
        )
}
