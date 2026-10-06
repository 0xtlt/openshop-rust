//! The demo's admin UI: every built-in screen plus the custom `/reviews` page, whose server
//! side lives in `apps/demo/src/admin/reviews.rs`.

use leptos::{prelude::*, task::spawn_local};
use openshop_ui::{
    App,
    admin::{AdminActionOptions, AdminFunctionRef, use_action, use_loader},
    shopify,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Deserialize)]
struct Review {
    title: String,
    shop: String,
}

#[derive(Clone, PartialEq, Deserialize)]
struct ReviewList {
    reviews: Vec<Review>,
}

#[derive(Serialize)]
struct SaveReview {
    title: String,
}

#[derive(Clone, Deserialize)]
struct SavedReview {
    title: String,
}

const LIST_REVIEWS: AdminFunctionRef<(), ReviewList> = AdminFunctionRef::loader("listReviews");
const SAVE_REVIEW: AdminFunctionRef<SaveReview, SavedReview> = AdminFunctionRef::action("saveReview");

#[component]
fn ReviewsPage() -> impl IntoView {
    let reviews = use_loader(LIST_REVIEWS, || ());
    let save = use_action(SAVE_REVIEW);
    let save_example = move |_| {
        spawn_local(async move {
            let input = SaveReview {
                title: "Saved from custom admin".to_string(),
            };
            let options = AdminActionOptions::new().revalidate(LIST_REVIEWS);
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

fn main() {
    App::new().page("/reviews", ReviewsPage).mount();
}
