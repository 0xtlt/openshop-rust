use super::types::{FunctionDef, type_label};
use crate::{components::page::Page, fetch::api_json_or_report, router::route};
use leptos::{prelude::*, task::spawn_local};

#[component]
pub fn FunctionList() -> impl IntoView {
    let functions = RwSignal::new(Vec::<FunctionDef>::new());

    spawn_local(async move {
        if let Some(definitions) = api_json_or_report("/api/functions", "the functions").await {
            functions.set(definitions);
        }
    });

    let card = |function: FunctionDef| {
        let modes = function
            .modes
            .as_ref()
            .filter(|modes| modes.len() > 1)
            .map(|modes| modes.join(", "));
        let handle = function.handle.clone();
        view! {
            <s-box padding="large-100" background="base" border="base" borderRadius="large">
                <s-stack direction="inline" justifyContent="space-between" alignItems="center">
                    <s-stack gap="small">
                        <s-heading>{function.label.clone()}</s-heading>
                        <s-stack direction="inline" gap="base">
                            <s-badge>{type_label(&function.kind).to_string()}</s-badge>
                            {modes.map(|modes| view! { <s-badge tone="info">{modes}</s-badge> })}
                        </s-stack>
                        <s-text color="subdued">{function.fields.len()} " config fields"</s-text>
                    </s-stack>
                    <s-button on:click=move |_| route(&format!("/functions/{handle}"))>"Manage"</s-button>
                </s-stack>
            </s-box>
        }
    };

    move || {
        let functions = functions.get();
        if functions.is_empty() {
            return view! {
                <Page heading="Functions">
                    <s-box padding="large-500">
                        <s-stack gap="base">
                            <s-heading>"No functions configured"</s-heading>
                            <s-paragraph>
                                "Define Shopify Functions in your openshop.config.ts to manage discount, cart transform, and other function instances from here."
                            </s-paragraph>
                        </s-stack>
                    </s-box>
                </Page>
            }
            .into_any();
        }
        view! {
            <Page heading="Functions">
                <s-stack gap="large-100">{functions.into_iter().map(card).collect_view()}</s-stack>
            </Page>
        }
        .into_any()
    }
}
