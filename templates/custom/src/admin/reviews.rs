//! The custom `/reviews` admin page: what the server answers, and the view that shows it.

use leptos::prelude::*;
use leptos::task::spawn_local;
use openshop::prelude::*;
use openshop::ui::{AdminActionOptions, shopify, use_action, use_loader};
use serde::{Deserialize, Serialize};

#[input]
#[derive(Debug)]
pub struct SaveReview {
    #[schemars(length(min = 1))]
    pub title: String,
}

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

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedReview {
    pub ok: bool,
    pub title: String,
    pub saved_by: String,
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

    #[action(authorize = Self::signed_in)]
    async fn save_review(ctx: AdminContext, input: SaveReview) -> Result<SavedReview> {
        Ok(SavedReview {
            ok: true,
            title: input.title,
            saved_by: ctx.actor.id,
        })
    }

    #[view]
    fn view() -> impl IntoView {
        let reviews = use_loader(Self::LIST_REVIEWS, || ());
        let save = use_action(Self::SAVE_REVIEW);
        let save_example = move |_| {
            spawn_local(async move {
                let input = SaveReview {
                    title: "Saved from custom admin".to_string(),
                };
                let options = AdminActionOptions::new().revalidate(Self::LIST_REVIEWS);
                if let Ok(Some(saved)) = save.invoke(input, options).await {
                    shopify::toast(&format!("Saved {}", saved.title));
                }
            });
        };

        view! {
            <s-page heading="Reviews">
                {move || reviews.loading.get().then(|| view! { <s-spinner accessibilityLabel="Loading reviews" /> })}
                {move || reviews.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
                {move || {
                    reviews.data.get().map(|list| {
                        list.reviews
                            .into_iter()
                            .map(|review| {
                                view! {
                                    <s-section heading=review.title>
                                        <s-paragraph>{review.shop}</s-paragraph>
                                    </s-section>
                                }
                            })
                            .collect_view()
                    })
                }}
                <s-button variant="primary" loading=move || save.pending.get() on:click=save_example>
                    "Save example"
                </s-button>
                {move || save.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
            </s-page>
        }
    }
}
