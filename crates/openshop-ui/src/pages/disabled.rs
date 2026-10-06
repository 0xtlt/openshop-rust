use crate::components::page::Page;
use leptos::prelude::*;

#[component]
pub fn Disabled() -> impl IntoView {
    view! {
        <Page heading="Page unavailable">
            <s-banner tone="warning">"This page is disabled."</s-banner>
        </Page>
    }
}
