//! The `/hello` admin page: what the server answers, and the view that shows it.

use leptos::prelude::*;
use openshop::prelude::*;
use openshop::ui::use_loader;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Greeting {
    pub message: String,
}

pub struct HelloPage;

#[admin_page("/hello", nav = "Hello")]
impl HelloPage {
    #[loader]
    async fn greeting(ctx: AdminContext) -> Result<Greeting> {
        Ok(Greeting {
            message: format!("Hello {}", ctx.shop),
        })
    }

    #[view]
    fn view() -> impl IntoView {
        // `Self::GREETING` names the loader, with the type it returns.
        let greeting = use_loader(Self::GREETING, || ());

        view! {
            <s-page heading="Hello">
                {move || greeting.loading.get().then(|| view! { <s-spinner accessibilityLabel="Loading" /> })}
                {move || greeting.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
                {move || {
                    greeting.data.get().map(|greeting| {
                        view! {
                            <s-section>
                                <s-paragraph>{greeting.message}</s-paragraph>
                            </s-section>
                        }
                    })
                }}
            </s-page>
        }
    }
}
