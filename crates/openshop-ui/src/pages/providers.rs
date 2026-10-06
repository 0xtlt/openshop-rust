use crate::{
    components::config_field_renderer::ConfigFieldRenderer,
    components::page::Page,
    dom::locale_date_time,
    events::event_value,
    fetch::{api_json, api_request},
    http::Method,
    json::js_string_or_empty,
    provider_status::provider_connection_status,
    reactive::set_if_changed,
    types::{BannerMessage, ProviderSummary},
};
use indexmap::IndexMap;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{Value, json};

type Form = IndexMap<String, String>;

fn to_form(config: &IndexMap<String, Value>) -> Form {
    config
        .iter()
        .map(|(key, value)| (key.clone(), js_string_or_empty(value)))
        .collect()
}

#[component]
pub fn Providers() -> impl IntoView {
    let providers = RwSignal::new(Vec::<ProviderSummary>::new());
    let index = RwSignal::new(0_usize);
    let form = RwSignal::new(Form::new());
    let saving = RwSignal::new(false);
    let checking = RwSignal::new(false);
    let message = RwSignal::new(None::<BannerMessage>);

    let load = move || {
        spawn_local(async move {
            match api_json::<Vec<ProviderSummary>>("/api/providers").await {
                Ok(data) => {
                    let Some(selected) = index.try_get_untracked() else {
                        return;
                    };
                    if !data.is_empty() {
                        form.set(
                            data.get(selected)
                                .map(|provider| to_form(&provider.config))
                                .unwrap_or_default(),
                        );
                    }
                    set_if_changed(providers, data);
                }
                Err(error) => message.set(Some(BannerMessage::critical(error.to_string()))),
            }
        });
    };

    Effect::new(move |_| {
        index.track();
        load();
    });

    let switch_provider = move |new_index: usize| {
        index.set(new_index);
        form.set(providers.with_untracked(|providers| {
            providers
                .get(new_index)
                .map(|provider| to_form(&provider.config))
                .unwrap_or_default()
        }));
        message.set(None);
    };

    let update_field = Callback::new(move |(key, value): (String, String)| {
        form.update(|form| {
            form.insert(key, value);
        });
    });

    let current = Memo::new(move |_| providers.with(|providers| providers.get(index.get()).cloned()));
    let current_name =
        move || current.with_untracked(|provider| provider.as_ref().map(|provider| provider.name.clone()));

    let save = move |_| {
        let Some(name) = current_name() else { return };
        let body = json!({ "config": form.get_untracked() });
        saving.set(true);
        message.set(None);
        spawn_local(async move {
            if let Ok(response) = api_request(Method::Put, &format!("/api/providers/{name}"), Some(&body)).await {
                message.set(Some(if response.ok {
                    BannerMessage::success("Saved.")
                } else {
                    BannerMessage::critical(response.error().unwrap_or_else(|| "Error".to_string()))
                }));
                if response.ok {
                    load();
                }
            }
            saving.set(false);
        });
    };

    let check = move |_| {
        let Some(name) = current_name() else { return };
        checking.set(true);
        message.set(None);
        spawn_local(async move {
            let url = format!("/api/providers/{name}/check");
            if let Ok(response) = api_request(Method::Post, &url, None).await {
                let data = response.value().unwrap_or_default();
                let ok = data.get("ok").is_some_and(crate::json::is_truthy);
                message.set(Some(if ok {
                    BannerMessage::success("Connection OK!")
                } else {
                    BannerMessage::critical(response.error().unwrap_or_else(|| "Failed".to_string()))
                }));
                load();
            }
            checking.set(false);
        });
    };

    let has_providers = Memo::new(move |_| !providers.with(Vec::is_empty));
    let provider_names = Memo::new(move |_| {
        providers.with(|providers| {
            providers
                .iter()
                .map(|provider| provider.name.clone())
                .collect::<Vec<_>>()
        })
    });
    let fields = Memo::new(move |_| current.with(|provider| provider.as_ref().map(|provider| provider.fields.clone())));
    let connection = Memo::new(move |_| {
        current.with(|provider| {
            provider
                .as_ref()
                .map(|provider| provider_connection_status(provider.configured, provider.last_check_ok))
        })
    });
    let last_checked_at = Memo::new(move |_| {
        current.with(|provider| provider.as_ref().and_then(|provider| provider.last_checked_at.clone()))
    });

    let provider_page = move || {
        view! {
            <Page heading="Providers">
                <s-button-group slot="secondary-actions">
                    <s-button variant="secondary" on:click=check disabled=move || checking.get()>
                        {move || if checking.get() { "Testing…" } else { "Test connection" }}
                    </s-button>
                </s-button-group>
                <s-button slot="primary-action" variant="primary" on:click=save disabled=move || saving.get()>
                    {move || if saving.get() { "Saving…" } else { "Save" }}
                </s-button>

                {move || message.get().map(|message| view! { <s-banner tone=message.tone>{message.text}</s-banner> })}

                {move || {
                    let names = provider_names.get();
                    (names.len() > 1)
                        .then(|| {
                            view! {
                                <s-section>
                                    <s-select
                                        label="Provider"
                                        prop:value=move || index.get().to_string()
                                        on:change=move |event| {
                                            switch_provider(event_value(&event).parse().unwrap_or(0))
                                        }
                                    >
                                        {names
                                            .into_iter()
                                            .enumerate()
                                            .map(|(position, name)| {
                                                view! { <s-option value=position.to_string()>{name}</s-option> }
                                            })
                                            .collect_view()}
                                    </s-select>
                                </s-section>
                            }
                        })
                }}

                <s-section heading=move || {
                    current.with(|provider| provider.as_ref().map(|provider| provider.name.clone()))
                        .unwrap_or_else(|| "Provider".to_string())
                }>
                    {move || {
                        connection
                            .get()
                            .map(|connection| {
                                view! {
                                    <s-box paddingBlockEnd="base">
                                        <s-badge tone=connection.tone>{connection.label}</s-badge>
                                    </s-box>
                                }
                            })
                    }}
                    {move || {
                        fields
                            .get()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(key, field)| {
                                let value_key = key.clone();
                                let value = Signal::derive(move || {
                                    form.with(|form| form.get(&value_key).cloned().unwrap_or_default())
                                });
                                view! { <ConfigFieldRenderer field_key=key field value on_change=update_field /> }
                            })
                            .collect_view()
                    }}
                </s-section>

                {move || {
                    last_checked_at
                        .get()
                        .filter(|checked_at| !checked_at.is_empty())
                        .map(|checked_at| {
                            view! {
                                <s-section>
                                    <s-text color="subdued">"Last checked: " {locale_date_time(&checked_at)}</s-text>
                                </s-section>
                            }
                        })
                }}
            </Page>
        }
    };

    move || {
        if has_providers.get() {
            provider_page().into_any()
        } else {
            view! {
                <Page heading="Providers">
                    <s-section>
                        <s-text color="subdued">"No providers registered."</s-text>
                    </s-section>
                </Page>
            }
            .into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_saved_config_into_form_strings() {
        let config: IndexMap<String, Value> =
            serde_json::from_str(r#"{"zone":"eu","retries":3,"sandbox":true,"token":null}"#).unwrap();
        let form = to_form(&config);
        assert_eq!(
            form.iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect::<Vec<_>>(),
            [("zone", "eu"), ("retries", "3"), ("sandbox", "true"), ("token", "")]
        );
        assert_eq!(
            json!({ "config": form }).to_string(),
            r#"{"config":{"zone":"eu","retries":"3","sandbox":"true","token":""}}"#
        );
    }
}
