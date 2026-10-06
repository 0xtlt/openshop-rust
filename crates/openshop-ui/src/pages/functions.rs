mod function_instances;
mod function_list;
mod model;
mod types;

use crate::{
    components::config_field_renderer::ConfigFieldRenderer,
    components::page::Page,
    events::{event_checked, event_value},
    fetch::{api_fetch, api_request},
    http::Method,
    json::js_string_or_empty,
    query::QueryParams,
    router::route,
    types::BannerMessage,
    url::decode_uri_component,
};
use function_instances::FunctionInstances;
use function_list::FunctionList;
use indexmap::IndexMap;
use leptos::{prelude::*, task::spawn_local};
use leptos_router::hooks::use_params_map;
use model::has_config_fields;
use openshop_shared::rpc::encode_uri_component;
use serde_json::json;
use types::{FunctionDef, FunctionInstance, type_label};

const DELETE_MODAL_ID: &str = "delete-function-instance-modal";

#[component]
pub fn Functions() -> impl IntoView {
    let params = use_params_map();
    let param = move |name: &'static str| {
        Memo::new(move |_| params.with(|params| params.get(name)).filter(|value| !value.is_empty()))
    };
    let handle = param("handle");
    let action = param("action");

    move || match (handle.get(), action.get()) {
        (Some(handle), Some(action)) if action == "new" => view! { <FunctionForm handle /> }.into_any(),
        (Some(handle), Some(instance_id)) => view! { <FunctionForm handle instance_id /> }.into_any(),
        (Some(handle), None) => view! { <FunctionInstances handle /> }.into_any(),
        (None, _) => view! { <FunctionList /> }.into_any(),
    }
}

fn instance_endpoint(handle: &str, instance_id: Option<&str>) -> String {
    match instance_id {
        Some(id) => format!("/api/functions/{handle}/instances/{}", encode_uri_component(id)),
        None => format!("/api/functions/{handle}/instances"),
    }
}

fn delete_endpoint(handle: &str, instance_id: &str, definition: Option<&FunctionDef>, mode: &str) -> String {
    let mut params = QueryParams::new();
    if definition.is_some_and(|definition| definition.kind == "discount") {
        params.set("mode", mode);
    }
    params.append_to(&instance_endpoint(handle, Some(instance_id)))
}

/// Create / edit form of a function instance.
#[component]
fn FunctionForm(handle: String, #[prop(optional)] instance_id: Option<String>) -> impl IntoView {
    let definition = RwSignal::new(None::<FunctionDef>);
    let form = RwSignal::new(IndexMap::<String, String>::new());
    let mode = RwSignal::new("automatic".to_string());
    let block_on_failure = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let deleting = RwSignal::new(false);
    let message = RwSignal::new(None::<BannerMessage>);
    let is_edit = instance_id.is_some();
    let handle = StoredValue::new(handle);
    let instance_id = StoredValue::new(instance_id);
    let list_path = move || format!("/functions/{}", handle.get_value());
    let update_field = Callback::new(move |(key, value): (String, String)| {
        form.update(|form| {
            form.insert(key, value);
        });
    });

    let wanted_handle = handle.get_value();
    spawn_local(async move {
        if let Ok(Ok(definitions)) = api_fetch("/api/functions")
            .await
            .map(|response| response.json::<Vec<FunctionDef>>())
            && let Some(found) = definitions
                .into_iter()
                .find(|definition| definition.handle == wanted_handle)
        {
            if let Some(first) = found.modes.as_ref().and_then(|modes| modes.first()) {
                mode.set(first.clone());
            }
            definition.set(Some(found));
        }
    });
    if let Some(instance_id) = instance_id.get_value() {
        let url = instance_endpoint(&handle.get_value(), None);
        spawn_local(async move {
            let Ok(Ok(instances)) = api_fetch(&url)
                .await
                .map(|response| response.json::<Vec<FunctionInstance>>())
            else {
                return;
            };
            let wanted = decode_uri_component(&instance_id);
            let Some(instance) = instances.into_iter().find(|instance| instance.id == wanted) else {
                return;
            };
            form.set(
                instance
                    .config
                    .iter()
                    .map(|(key, value)| (key.clone(), js_string_or_empty(value)))
                    .collect(),
            );
            if let Some(block) = instance.block_on_failure {
                block_on_failure.set(block);
            }
        });
    }

    let save = move |_| {
        let Some(kind) =
            definition.with_untracked(|definition| definition.as_ref().map(|definition| definition.kind.clone()))
        else {
            return;
        };
        let mut body = json!({ "config": form.get_untracked(), "mode": mode.get_untracked() });
        if kind == "cart-transform" {
            body["blockOnFailure"] = json!(block_on_failure.get_untracked());
        }
        let endpoint = instance_endpoint(&handle.get_value(), instance_id.get_value().as_deref());
        let list_path = list_path();
        saving.set(true);
        message.set(None);
        spawn_local(async move {
            let method = if is_edit { Method::Put } else { Method::Post };
            if let Ok(response) = api_request(method, &endpoint, Some(&body)).await {
                if response.ok {
                    message.set(Some(BannerMessage::success(if is_edit {
                        "Updated."
                    } else {
                        "Created."
                    })));
                    if !is_edit {
                        route(&list_path);
                    }
                } else {
                    let error = response.error().unwrap_or_else(|| "Error".to_string());
                    message.set(Some(BannerMessage::critical(error)));
                }
            }
            saving.set(false);
        });
    };

    let remove = move |_| {
        let Some(instance_id) = instance_id.get_value() else {
            return;
        };
        let handle = handle.get_value();
        let endpoint = definition.with_untracked(|definition| {
            delete_endpoint(&handle, &instance_id, definition.as_ref(), &mode.get_untracked())
        });
        deleting.set(true);
        spawn_local(async move {
            if let Ok(response) = api_request(Method::Delete, &endpoint, None).await {
                if response.ok {
                    route(&format!("/functions/{handle}"));
                } else {
                    let error = response.error().unwrap_or_else(|| "Delete failed".to_string());
                    message.set(Some(BannerMessage::critical(error)));
                }
            }
            deleting.set(false);
        });
    };

    let form_page = move |definition: FunctionDef| {
        let FunctionDef {
            label,
            kind,
            modes,
            supports_update,
            fields,
            ..
        } = definition.clone();
        let read_only = is_edit && !supports_update;
        let heading = if is_edit {
            "Edit instance".to_string()
        } else {
            format!("New {}", type_label(&kind))
        };
        let modes = modes.filter(|modes| modes.len() > 1);
        view! {
            {is_edit
                .then(|| {
                    view! {
                        <s-modal
                            id=DELETE_MODAL_ID
                            heading="Delete instance"
                            accessibility-label="Confirm function instance deletion"
                        >
                            <s-text>"This will permanently remove this function instance from Shopify."</s-text>
                            <s-button
                                slot="primary-action"
                                variant="primary"
                                tone="critical"
                                on:click=remove
                                disabled=move || deleting.get()
                            >
                                {move || if deleting.get() { "Deleting..." } else { "Delete" }}
                            </s-button>
                            <s-button
                                slot="secondary-actions"
                                variant="secondary"
                                commandFor=DELETE_MODAL_ID
                                command="--hide"
                            >
                                "Cancel"
                            </s-button>
                        </s-modal>
                    }
                })}
            <Page heading=heading>
                <s-link
                    slot="breadcrumb-actions"
                    href=list_path()
                    on:click=move |event| {
                        event.prevent_default();
                        route(&list_path());
                    }
                >
                    {label}
                </s-link>
                <s-button
                    slot="primary-action"
                    variant="primary"
                    on:click=save
                    disabled=move || saving.get() || read_only
                >
                    {move || match (saving.get(), is_edit) {
                        (true, true) => "Saving...",
                        (true, false) => "Creating...",
                        (false, true) => "Save",
                        (false, false) => "Create",
                    }}
                </s-button>

                <s-stack gap="large-100">
                    {move || message.get().map(|message| view! { <s-banner tone=message.tone>{message.text}</s-banner> })}

                    {read_only
                        .then(|| {
                            view! {
                                <s-banner tone="warning">
                                    "This function type does not support updates. Delete and recreate to change configuration."
                                </s-banner>
                            }
                        })}

                    {(kind == "cart-transform")
                        .then(|| {
                            view! {
                                <s-box padding="large-100" background="base" border="base" borderRadius="large">
                                    <s-checkbox
                                        label="Block checkout when the Cart Transform fails"
                                        prop:checked=move || block_on_failure.get()
                                        disabled=is_edit
                                        on:change=move |event| block_on_failure.set(event_checked(&event))
                                    />
                                </s-box>
                            }
                        })}

                    {modes
                        .map(|modes| {
                            view! {
                                <s-box padding="large-100" background="base" border="base" borderRadius="large">
                                    <s-select
                                        label="Discount mode"
                                        prop:value=move || mode.get()
                                        on:change=move |event| mode.set(event_value(&event))
                                    >
                                        {modes
                                            .into_iter()
                                            .map(|mode| {
                                                let label = if mode == "automatic" {
                                                    "Automatic discount"
                                                } else {
                                                    "Discount code"
                                                };
                                                view! { <s-option value=mode>{label}</s-option> }
                                            })
                                            .collect_view()}
                                    </s-select>
                                </s-box>
                            }
                        })}

                    {has_config_fields(&definition)
                        .then(|| {
                            view! {
                                <s-box padding="large-100" background="base" border="base" borderRadius="large">
                                    <s-stack gap="base">
                                        <s-heading>"Configuration"</s-heading>
                                        {fields
                                            .into_iter()
                                            .map(|(key, field)| {
                                                let value_key = key.clone();
                                                let value = Signal::derive(move || {
                                                    form.with(|form| form.get(&value_key).cloned().unwrap_or_default())
                                                });
                                                view! {
                                                    <ConfigFieldRenderer field_key=key field value on_change=update_field />
                                                }
                                            })
                                            .collect_view()}
                                    </s-stack>
                                </s-box>
                            }
                        })}

                    {is_edit
                        .then(|| {
                            view! {
                                <s-box padding="large-100" background="base" border="base" borderRadius="large">
                                    <s-stack gap="base">
                                        <s-heading>"Danger zone"</s-heading>
                                        <s-paragraph>
                                            "This will permanently remove this function instance from Shopify."
                                        </s-paragraph>
                                        <s-button
                                            variant="secondary"
                                            tone="critical"
                                            commandFor=DELETE_MODAL_ID
                                            command="--show"
                                            disabled=move || deleting.get()
                                        >
                                            {move || if deleting.get() { "Deleting..." } else { "Delete instance" }}
                                        </s-button>
                                    </s-stack>
                                </s-box>
                            }
                        })}
                </s-stack>
            </Page>
        }
    };

    move || match definition.get() {
        Some(definition) => form_page(definition).into_any(),
        None => view! {
            <Page heading="Loading...">
                <s-link slot="breadcrumb-actions" href=list_path()>
                    "Function"
                </s-link>
                <s-box padding="large-500">
                    <s-text color="subdued">"Loading..."</s-text>
                </s-box>
            </Page>
        }
        .into_any(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(kind: &str) -> FunctionDef {
        serde_json::from_value(json!({
            "key": "volume", "label": "Volume", "type": kind, "handle": "volume",
            "supportsUpdate": true, "fields": {}
        }))
        .unwrap()
    }

    #[test]
    fn builds_instance_endpoints() {
        assert_eq!(instance_endpoint("volume", None), "/api/functions/volume/instances");
        assert_eq!(
            instance_endpoint("volume", Some("gid://shopify/DiscountNode/1")),
            "/api/functions/volume/instances/gid%3A%2F%2Fshopify%2FDiscountNode%2F1"
        );
    }

    #[test]
    fn sends_the_discount_mode_when_deleting() {
        assert_eq!(
            delete_endpoint("volume", "1", Some(&definition("discount")), "code"),
            "/api/functions/volume/instances/1?mode=code"
        );
        assert_eq!(
            delete_endpoint("bundle", "1", Some(&definition("cart-transform")), "automatic"),
            "/api/functions/bundle/instances/1"
        );
        assert_eq!(
            delete_endpoint("volume", "1", None, "automatic"),
            "/api/functions/volume/instances/1"
        );
    }
}
