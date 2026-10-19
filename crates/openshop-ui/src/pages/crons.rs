use crate::{
    components::page::Page,
    components::table_header::TableHeader,
    dom::{self, element_by_id, hide_modal, show_modal},
    fetch::{api_json, api_request},
    http::Method,
    json,
    types::{CronItem, CronShops},
};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{Value, json};

const INPUT_MODAL_ID: &str = "cron-input-modal";
const INPUT_PRE_ID: &str = "cron-input-pre";

fn shops_label(shops: Option<&CronShops>) -> String {
    match shops {
        Some(CronShops::Many(shops)) => shops.join(", "),
        Some(CronShops::One(shop)) => shop.clone(),
        None => "global".to_string(),
    }
}

fn show_input(cron: &CronItem) {
    if element_by_id(INPUT_MODAL_ID).is_none() {
        return;
    }
    let input = json::pretty(cron.input.as_ref().unwrap_or(&Value::Null));
    if let Some(pre) = element_by_id(INPUT_PRE_ID) {
        pre.set_text_content(Some(&input));
        let _ = pre.set_attribute("data-full", &input);
    }
    if let Some(title) = element_by_id("cron-input-title") {
        let name = cron.name.as_deref().unwrap_or(&cron.flow);
        let _ = title.set_attribute("title", &format!("Input — {name}"));
    }
    show_modal(INPUT_MODAL_ID);
}

#[component]
pub fn Crons() -> impl IntoView {
    let crons = RwSignal::new(Vec::<CronItem>::new());
    let toggling = RwSignal::new(None::<String>);
    let error = RwSignal::new(None::<String>);

    spawn_local(async move {
        match api_json::<Vec<CronItem>>("/api/crons").await {
            Ok(items) => {
                crons.set(items);
                error.set(None);
            }
            Err(failure) => error.set(Some(failure.to_string())),
        }
    });

    let toggle = move |key: String, enabled: bool| {
        toggling.set(Some(key.clone()));
        spawn_local(async move {
            let body = json!({ "key": key, "enabled": !enabled });
            // A request that gets no answer is reported by `api_request`.
            if let Ok(response) = api_request(Method::Post, "/api/crons/toggle", Some(&body)).await {
                if response.ok {
                    error.set(None);
                    crons.update(|crons| {
                        for cron in crons.iter_mut().filter(|cron| cron.key == key) {
                            cron.enabled = !cron.enabled;
                        }
                    });
                } else {
                    error.set(Some(response.error().unwrap_or_else(|| {
                        format!("Could not change the cron: the server answered {}", response.status)
                    })));
                }
            }
            toggling.set(None);
        });
    };

    let copy_input = |_| {
        let full = element_by_id(INPUT_PRE_ID).and_then(|pre| pre.get_attribute("data-full"));
        if let Some(full) = full.filter(|full| !full.is_empty()) {
            spawn_local(async move {
                let _ = dom::copy_text(&full).await;
            });
        }
    };

    let row = move |cron: CronItem| {
        let has_input = cron.input.as_ref().is_some_and(json::is_truthy);
        let (key, enabled) = (cron.key.clone(), cron.enabled);
        let is_toggling = {
            let key = key.clone();
            move || toggling.with(|toggling| toggling.as_deref() == Some(key.as_str()))
        };
        let toggle_label = {
            let is_toggling = is_toggling.clone();
            move || match (is_toggling(), enabled) {
                (true, _) => "...",
                (false, true) => "Disable",
                (false, false) => "Enable",
            }
        };
        view! {
            <s-table-row>
                <s-table-cell>
                    <strong>{cron.name.clone().unwrap_or_else(|| cron.flow.clone())}</strong>
                    {cron
                        .name
                        .as_ref()
                        .filter(|name| !name.is_empty())
                        .map(|_| view! { <div style="font-size:12px;color:#8c9196">{cron.flow.clone()}</div> })}
                </s-table-cell>
                <s-table-cell>
                    <code style="font-size:13px;background:#f1f1f1;padding:2px 6px;border-radius:4px">
                        {cron.schedule.clone()}
                    </code>
                </s-table-cell>
                <s-table-cell>{shops_label(cron.shops.as_ref())}</s-table-cell>
                <s-table-cell>
                    <s-badge tone=if enabled { "success" } else { "neutral" }>
                        {if enabled { "Enabled" } else { "Disabled" }}
                    </s-badge>
                </s-table-cell>
                <s-table-cell>
                    <span style="display:flex;gap:6px">
                        {has_input
                            .then(|| {
                                view! {
                                    <s-button variant="secondary" on:click=move |_| show_input(&cron)>
                                        "Input"
                                    </s-button>
                                }
                            })}
                        <s-button
                            variant=if enabled { "secondary" } else { "primary" }
                            on:click=move |_| toggle(key.clone(), enabled)
                            disabled=is_toggling
                        >
                            {toggle_label}
                        </s-button>
                    </span>
                </s-table-cell>
            </s-table-row>
        }
    };

    view! {
        <ui-modal id=INPUT_MODAL_ID>
            <div style="padding:16px">
                <div style="display:flex;gap:6px;margin-bottom:8px">
                    <s-button variant="secondary" on:click=copy_input>
                        "Copy"
                    </s-button>
                </div>
                <pre
                    id=INPUT_PRE_ID
                    style="font-family:ui-monospace, SFMono-Regular, Menlo, monospace;font-size:12px;line-height:1.5;background:#fafafa;border-radius:6px;padding:8px 12px;overflow:auto;max-height:60vh;white-space:pre-wrap;word-break:break-word;margin:0;border:1px solid #e3e3e3"
                ></pre>
            </div>
            <ui-title-bar id="cron-input-title" title="Input">
                <button on:click=|_| hide_modal(INPUT_MODAL_ID)>"Close"</button>
            </ui-title-bar>
        </ui-modal>

        <Page heading="Crons">
            {move || error.get().map(|error| view! { <s-banner tone="critical">{error}</s-banner> })}
            <s-section padding="none">
                <s-table>
                    <s-table-header-row>
                        <TableHeader list_slot="primary">"Name"</TableHeader>
                        <TableHeader list_slot="secondary">"Schedule"</TableHeader>
                        <TableHeader list_slot="labeled">"Shops"</TableHeader>
                        <TableHeader list_slot="inline">"Status"</TableHeader>
                        <TableHeader />
                    </s-table-header-row>
                    <s-table-body>
                        {move || {
                            crons
                                .with(Vec::is_empty)
                                .then(|| {
                                    view! {
                                        <s-table-row>
                                            <s-table-cell>
                                                <s-text color="subdued">"No crons configured"</s-text>
                                            </s-table-cell>
                                            <s-table-cell />
                                            <s-table-cell />
                                            <s-table-cell />
                                            <s-table-cell />
                                        </s-table-row>
                                    }
                                })
                        }}
                        <For each=move || crons.get() key=|cron| (cron.key.clone(), cron.enabled) children=row />
                    </s-table-body>
                </s-table>
            </s-section>
        </Page>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_shop_targets() {
        assert_eq!(shops_label(None), "global");
        assert_eq!(shops_label(Some(&CronShops::One("all".to_string()))), "all");
        assert_eq!(
            shops_label(Some(&CronShops::Many(vec![
                "a.myshopify.com".into(),
                "b.myshopify.com".into()
            ]))),
            "a.myshopify.com, b.myshopify.com"
        );
    }
}
