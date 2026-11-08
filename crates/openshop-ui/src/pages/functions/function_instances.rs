use super::{
    model::{can_create_instance, instance_is_active, instance_label, instance_state},
    types::{FunctionDef, FunctionInstance, type_label},
};
use crate::{
    components::page::Page,
    components::table_header::TableHeader,
    fetch::{api_fetch, api_json_or_report},
    json::js_string,
    router::route,
};
use futures_util::future::join;
use leptos::{prelude::*, task::spawn_local};
use openshop_shared::rpc::encode_uri_component;

fn config_summary(instance: &FunctionInstance) -> String {
    let summary = instance
        .config
        .iter()
        .map(|(key, value)| format!("{key}: {}", js_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    if summary.is_empty() { "—".to_string() } else { summary }
}

#[component]
pub fn FunctionInstances(handle: String) -> impl IntoView {
    let instances = RwSignal::new(Vec::<FunctionInstance>::new());
    let definition = RwSignal::new(None::<FunctionDef>);
    let loading = RwSignal::new(true);
    let error = RwSignal::new(None::<String>);
    let handle = StoredValue::new(handle);
    let new_instance_path = move || format!("/functions/{}/new", handle.get_value());

    let wanted_handle = handle.get_value();
    spawn_local(async move {
        let handle = wanted_handle;
        let (definitions, listed) = join(
            api_json_or_report::<Vec<FunctionDef>>("/api/functions", "the functions"),
            api_fetch(&format!("/api/functions/{handle}/instances")),
        )
        .await;
        // Every way the load can fail sets `error`: without it, a failure
        // would read as a function that has no instance yet, and offer to
        // create the first one of a singleton.
        let failure = match (definitions, listed) {
            (None, _) => Some("The functions could not be loaded.".to_string()),
            (Some(definitions), listed) => {
                definition.set(definitions.into_iter().find(|definition| definition.handle == handle));
                match listed {
                    Err(failure) => Some(failure.to_string()),
                    Ok(listed) if !listed.ok => Some(listed.error().unwrap_or_else(|| {
                        format!(
                            "The instances could not be loaded: the server answered {}",
                            listed.status
                        )
                    })),
                    Ok(listed) => match listed.json() {
                        Ok(items) => {
                            instances.set(items);
                            None
                        }
                        Err(failure) => Some(failure.to_string()),
                    },
                }
            }
        };
        error.set(failure);
        loading.set(false);
    });

    let type_name = move || {
        definition.with(|definition| {
            definition
                .as_ref()
                .map(|definition| type_label(&definition.kind).to_string())
                .unwrap_or_default()
        })
    };
    let can_create = Memo::new(move |_| {
        let loaded = !loading.get() && error.with(Option::is_none);
        definition
            .with(|definition| instances.with(|instances| can_create_instance(definition.as_ref(), instances, loaded)))
    });

    let row = move |instance: FunctionInstance| {
        let path = format!(
            "/functions/{}/{}",
            handle.get_value(),
            encode_uri_component(&instance.id)
        );
        view! {
            <s-table-row>
                <s-table-cell>{instance_label(&instance)}</s-table-cell>
                <s-table-cell>
                    <s-badge tone=if instance_is_active(&instance) { "success" } else { "warning" }>
                        {instance_state(&instance)}
                    </s-badge>
                </s-table-cell>
                <s-table-cell>
                    <s-text color="subdued">{config_summary(&instance)}</s-text>
                </s-table-cell>
                <s-table-cell>
                    <s-button variant="secondary" on:click=move |_| route(&path)>
                        "See"
                    </s-button>
                </s-table-cell>
            </s-table-row>
        }
    };

    let content = move || {
        if loading.get() {
            return view! {
                <s-box padding="large-500">
                    <s-text color="subdued">"Loading instances from Shopify..."</s-text>
                </s-box>
            }
            .into_any();
        }
        if error.with(Option::is_some) {
            return view! {
                <s-box padding="large-500">
                    <s-text color="subdued">"The instances could not be loaded."</s-text>
                </s-box>
            }
            .into_any();
        }
        let items = instances.get();
        if items.is_empty() {
            return view! {
                <s-box padding="large-500" background="subdued" borderRadius="large">
                    <s-stack gap="base" alignItems="center">
                        <s-heading>"No instances yet"</s-heading>
                        <s-paragraph>
                            "Create your first " {type_name}
                            " instance to start using this function. Each instance has its own configuration stored as metafields on Shopify."
                        </s-paragraph>
                        {move || {
                            can_create
                                .get()
                                .then(|| {
                                    view! {
                                        <s-button variant="primary" on:click=move |_| route(&new_instance_path())>
                                            "Create first instance"
                                        </s-button>
                                    }
                                })
                        }}
                    </s-stack>
                </s-box>
            }
            .into_any();
        }
        let count = format!("{} instance{}", items.len(), if items.len() != 1 { "s" } else { "" });
        view! {
            <s-section>
                <s-text color="subdued">{count} " — " {type_name}</s-text>
                <s-table>
                    <s-table-header-row>
                        <TableHeader list_slot="primary">
                            {move || {
                                let is_cart_transform = definition
                                    .with(|definition| {
                                        definition.as_ref().is_some_and(|definition| definition.kind == "cart-transform")
                                    });
                                if is_cart_transform { "Label" } else { "Title" }
                            }}
                        </TableHeader>
                        <TableHeader list_slot="inline">"Status"</TableHeader>
                        <TableHeader list_slot="secondary">"Configuration"</TableHeader>
                        <TableHeader />
                    </s-table-header-row>
                    <s-table-body>{items.into_iter().map(row).collect_view()}</s-table-body>
                </s-table>
            </s-section>
        }
        .into_any()
    };

    view! {
        <Page heading=Signal::derive(move || {
            definition
                .with(|definition| definition.as_ref().map(|definition| definition.label.clone()))
                .unwrap_or_else(|| handle.get_value())
        })>
            <s-link
                slot="breadcrumb-actions"
                href="/functions"
                on:click=|event| {
                    event.prevent_default();
                    route("/functions");
                }
            >
                "Functions"
            </s-link>
            {move || {
                can_create
                    .get()
                    .then(|| {
                        view! {
                            <s-button
                                slot="primary-action"
                                variant="primary"
                                on:click=move |_| route(&new_instance_path())
                            >
                                "Create instance"
                            </s-button>
                        }
                    })
            }}

            {move || error.get().map(|error| view! { <s-banner tone="critical">{error}</s-banner> })}

            {content}
        </Page>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summarizes_instance_configuration() {
        let instance: FunctionInstance = serde_json::from_value(json!({
            "id": "gid://shopify/DiscountAutomaticNode/1",
            "title": "Volume",
            "config": { "threshold": 3, "percent": "10", "stack": false }
        }))
        .unwrap();
        assert_eq!(config_summary(&instance), "threshold: 3, percent: 10, stack: false");
        let empty: FunctionInstance = serde_json::from_value(json!({ "id": "1" })).unwrap();
        assert_eq!(config_summary(&empty), "—");
    }
}
