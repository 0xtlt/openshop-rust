//! The view of a custom admin page: `#[view]` in an `#[admin_page]` block, next
//! to the loaders and the actions it calls.

use leptos::prelude::*;
use openshop::prelude::*;
use openshop::testing::{AdminTestOptions, TestContext, admin_context, invoke_admin_function};
use openshop::ui::{
    AdminActionOptions, AdminFunctionKind, AdminFunctionRef, PageView, RouteParams, use_action, use_loader,
};
use serde::{Deserialize, Serialize};

#[input]
struct SaveReview {
    title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Review {
    title: String,
    shop: String,
}

struct ReviewsPage;

#[admin_page("/reviews", nav = "Reviews")]
impl ReviewsPage {
    #[loader]
    async fn list_reviews(ctx: AdminContext) -> Result<Vec<Review>> {
        Ok(vec![Self::review("First", &ctx)])
    }

    #[action("save")]
    async fn save_review(ctx: AdminContext, input: SaveReview) -> Result<Review> {
        Ok(Self::review(&input.title, &ctx))
    }

    /// Not marked: a helper of what the server runs.
    fn review(title: &str, ctx: &AdminContext) -> Review {
        Review {
            title: title.to_string(),
            shop: ctx.shop.clone(),
        }
    }

    #[view]
    fn view() -> impl IntoView {
        let reviews = use_loader(Self::LIST_REVIEWS, || ());
        let save = use_action(Self::SAVE_REVIEW);
        let add = move |_| {
            leptos::task::spawn_local(async move {
                let input = SaveReview {
                    title: "Second".to_string(),
                };
                let options = AdminActionOptions::new().revalidate(Self::LIST_REVIEWS);
                let _ = save.invoke(input, options).await;
            });
        };

        view! {
            <s-page heading="Reviews">
                {move || {
                    reviews
                        .data
                        .get()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|review| view! { <s-paragraph>{review.title}</s-paragraph> })
                        .collect_view()
                }}
                <s-button on:click=add>"Add review"</s-button>
            </s-page>
        }
    }
}

struct ReviewPage;

#[admin_page("/reviews/:id")]
impl ReviewPage {
    #[loader]
    async fn review(ctx: AdminContext) -> Result<Option<String>> {
        Ok(ctx.params.get("id").cloned())
    }

    /// A page with a dynamic segment reads it from the route.
    #[view]
    fn view(params: RouteParams) -> impl IntoView {
        let id = params.get("id").unwrap_or_default().to_string();
        let review = use_loader(Self::REVIEW, || ());
        view! { <s-page heading=move || review.data.get().flatten().unwrap_or_else(|| id.clone())></s-page> }
    }
}

static APP: App = app! {
    admin_pages: [ReviewsPage, ReviewPage],
};

#[test]
fn a_view_names_the_functions_of_its_page_with_their_types() {
    // The constant of a function: its kind, its name, what it takes and what it returns.
    let list: AdminFunctionRef<(), Vec<Review>> = ReviewsPage::LIST_REVIEWS;
    let save: AdminFunctionRef<SaveReview, Review> = ReviewsPage::SAVE_REVIEW;
    let review: AdminFunctionRef<(), Option<String>> = ReviewPage::REVIEW;

    assert_eq!((list.kind(), list.name()), (AdminFunctionKind::Loader, "listReviews"));
    assert_eq!((save.kind(), save.name()), (AdminFunctionKind::Action, "save"));
    assert_eq!((review.kind(), review.name()), (AdminFunctionKind::Loader, "review"));
}

#[test]
fn a_page_with_a_view_is_a_page_of_the_bundle() {
    let reviews = ReviewsPage::page_view().unwrap();
    assert_eq!(reviews.route_pattern(), ReviewsPage::PATH);
    let review = ReviewPage::page_view().unwrap();
    assert_eq!((review.route_pattern(), review.id()), ("/reviews/:id", "reviews/[id]"));
}

#[openshop::test(APP)]
async fn the_server_answers_what_the_view_calls(ctx: &TestContext) {
    let config = ctx.app.config();
    let page = config
        .admin_pages
        .iter()
        .find(|page| page.path == ReviewsPage::PATH)
        .unwrap();
    assert_eq!(page.nav.as_deref(), Some("Reviews"));
    let context = admin_context(
        &ctx.app,
        AdminTestOptions {
            shop: ctx.shop.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // The types of a constant are the ones the function is called with.
    let listed: Vec<Review> =
        invoke_admin_function(&page.functions[ReviewsPage::LIST_REVIEWS.name()], context.clone(), ())
            .await
            .unwrap();
    assert_eq!(
        listed,
        [Review {
            title: "First".to_string(),
            shop: ctx.shop.clone(),
        }]
    );

    let input = SaveReview {
        title: "Second".to_string(),
    };
    let saved: Review = invoke_admin_function(&page.functions[ReviewsPage::SAVE_REVIEW.name()], context, input)
        .await
        .unwrap();
    assert_eq!(saved.title, "Second");
}
