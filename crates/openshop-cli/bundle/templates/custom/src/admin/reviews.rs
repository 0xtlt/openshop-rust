//! The custom `/reviews` admin page: what the server answers, and the view it
//! renders with it.

use maud::html;
use openshop::prelude::*;
use serde::{Deserialize, Serialize};

#[input]
#[derive(Debug)]
pub struct SaveReview {
    #[schemars(length(min = 1))]
    pub title: String,
}

/// `Serialize` for the loader, `Deserialize` for a test that calls it with
/// `ctx.call_loader`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub title: String,
    pub shop: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewList {
    pub reviews: Vec<Review>,
}

pub struct ReviewsPage;

#[admin_page("/reviews", nav = "Reviews")]
impl ReviewsPage {
    #[access]
    async fn signed_in(ctx: AdminContext) -> Result<bool> {
        Ok(!ctx.actor.id.is_empty())
    }

    #[loader]
    async fn list_reviews(ctx: AdminContext) -> Result<ReviewList> {
        Ok(ReviewList {
            reviews: vec![Review {
                id: "example".to_string(),
                title: "Example review".to_string(),
                shop: ctx.shop,
            }],
        })
    }

    /// The form of the page: the page is rendered again, with a toast. An
    /// empty title is refused by the input schema, under its field.
    #[action(authorize = Self::signed_in)]
    async fn save_review(ctx: AdminContext, input: SaveReview) -> Result<Outcome> {
        ctx.logger
            .info_with("Review saved", json!({ "title": input.title, "savedBy": ctx.actor.id }));
        Ok(Outcome::toast(format!("Saved {}", input.title)))
    }

    /// Rendered by the server with what `list_reviews` returns, and the form
    /// the page refused, if it refused one.
    #[view]
    fn view(list_reviews: ReviewList, form: FormState) -> Markup {
        html! {
            s-page heading="Reviews" {
                @for review in &list_reviews.reviews {
                    s-section heading=(review.title) {
                        s-paragraph { (review.shop) }
                    }
                }
                s-section heading="New review" {
                    form method="post" {
                        input type="hidden" name="_action" value=(Self::SAVE_REVIEW);
                        s-stack gap="base" {
                            s-text-field
                                name="title"
                                label="Title"
                                value=(form.value_or("title", "Saved from custom admin"))
                                error=[form.error("title")] {}
                            s-button type="submit" variant="primary" { "Save" }
                        }
                    }
                }
            }
        }
    }
}
