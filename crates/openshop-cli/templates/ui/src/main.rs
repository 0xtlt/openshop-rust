//! Admin UI bundle: the built-in OpenShop screens plus this app's custom pages.
//!
//! Build with `trunk build --release`. The server side of each page (access
//! rules, loaders and actions) is registered with `AdminPage` in the app.

use leptos::prelude::*;
use openshop_ui::App;
use openshop_ui::admin::{AdminFunctionRef, use_loader};
use serde::Deserialize;

#[derive(Clone, PartialEq, Deserialize)]
struct Greeting {
    message: String,
}

const GREETING: AdminFunctionRef<(), Greeting> = AdminFunctionRef::loader("greeting");

#[component]
fn HelloPage() -> impl IntoView {
    let greeting = use_loader(GREETING, || ());

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

fn main() {
    App::new().page("/hello", HelloPage).mount();
}
