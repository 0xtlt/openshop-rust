mod flow_list_table;
mod modals;
mod runs_table;
mod schema_example;

use crate::{
    components::page::Page,
    dom::{self, every, hide_modal, show_modal},
    events::event_value,
    fetch::{api_fetch, api_json, api_request},
    http::Method,
    json,
    query::QueryParams,
    reactive::set_if_changed,
    router::route,
    shopify,
    types::{FlowRun, FlowSummary},
};
use flow_list_table::FlowListTable;
use gloo_timers::callback::Timeout;
use leptos::{prelude::*, task::spawn_local};
use leptos_router::hooks::use_params_map;
use modals::{DELETE_MODAL_ID, DeleteRunsModal, RUN_MODAL_ID, RunFlowModal};
use runs_table::RunsTable;
use schema_example::schema_to_example;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const PAGE_SIZE: usize = 25;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunStarted {
    run_id: String,
}

#[derive(Deserialize)]
struct BulkDeleted {
    #[serde(default)]
    deleted: u64,
    #[serde(default)]
    skipped: u64,
}

fn runs_url(filter: Option<&str>, search: &str, page: usize) -> String {
    let mut params = QueryParams::new();
    params.set("limit", &(PAGE_SIZE + 1).to_string());
    params.set("offset", &(page * PAGE_SIZE).to_string());
    if !search.is_empty() {
        params.set("search", search);
    }
    let base = match filter {
        Some(flow) => format!("/api/flows/{flow}/runs"),
        None => "/api/runs".to_string(),
    };
    params.append_to(&base)
}

fn search_url(pathname: &str, current_search: &str, value: &str) -> String {
    let mut params = QueryParams::parse(current_search);
    if value.is_empty() {
        params.delete("q");
    } else {
        params.set("q", value);
    }
    params.append_to(pathname)
}

fn deleted_message(result: &BulkDeleted) -> String {
    let skipped = match result.skipped {
        0 => String::new(),
        skipped => format!(", {skipped} skipped (active)"),
    };
    format!("Deleted {} run(s){skipped}", result.deleted)
}

#[component]
pub fn Flows() -> impl IntoView {
    let params = use_params_map();
    // The runs filter comes only from the URL route param.
    let filter = Memo::new(move |_| params.with(|params| params.get("name")).filter(|name| !name.is_empty()));
    let flows = RwSignal::new(Vec::<FlowSummary>::new());
    let runs = RwSignal::new(Vec::<FlowRun>::new());
    let selected = RwSignal::new(None::<String>);
    let triggering = RwSignal::new(false);
    let loading_runs = RwSignal::new(false);
    let page = RwSignal::new(0_usize);
    let checked_ids = RwSignal::new(Vec::<String>::new());
    let deleting = RwSignal::new(false);
    let refresh_key = RwSignal::new(0_u32);
    let error = RwSignal::new(None::<String>);
    let input_json = RwSignal::new("{}".to_string());
    let input_error = RwSignal::new(None::<String>);
    let initial_query = QueryParams::parse(&dom::location_search())
        .get("q")
        .unwrap_or_default()
        .to_string();
    let search_input = RwSignal::new(initial_query.clone());
    let search = RwSignal::new(initial_query);
    let debounce = StoredValue::new_local(None::<Timeout>);

    let on_search_input = move |event: web_sys::Event| {
        let value = event_value(&event);
        search_input.set(value.clone());
        dom::replace_url(&search_url(&dom::location_pathname(), &dom::location_search(), &value));
        debounce.set_value(Some(Timeout::new(300, move || search.set(value))));
    };

    spawn_local(async move {
        if let Ok(Ok(items)) = api_fetch("/api/flows").await.map(|response| response.json()) {
            flows.set(items);
        }
    });

    // Reset page and selection when the filter or search changes.
    Effect::new(move |_| {
        filter.track();
        search.track();
        page.set(0);
        checked_ids.set(Vec::new());
    });
    Effect::new(move |_| {
        page.track();
        checked_ids.set(Vec::new());
    });

    Effect::new(move |_| {
        refresh_key.track();
        let url = runs_url(filter.get().as_deref(), &search.get(), page.get());
        loading_runs.set(true);
        let in_flight = Rc::new(Cell::new(false));
        let canceled = Arc::new(AtomicBool::new(false));
        on_cleanup({
            let canceled = Arc::clone(&canceled);
            move || canceled.store(true, Ordering::Relaxed)
        });
        let load = move || {
            if in_flight.replace(true) {
                return;
            }
            let (in_flight, canceled, url) = (Rc::clone(&in_flight), Arc::clone(&canceled), url.clone());
            spawn_local(async move {
                let result = api_json::<Vec<FlowRun>>(&url).await;
                in_flight.set(false);
                if canceled.load(Ordering::Relaxed) {
                    return;
                }
                match result {
                    Ok(data) => {
                        set_if_changed(runs, data);
                        set_if_changed(error, None);
                    }
                    Err(failure) => error.set(Some(failure.to_string())),
                }
                loading_runs.set(false);
            });
        };
        load();
        every(5_000, load);
    });

    let trigger_run = move || {
        let Some(flow) = selected.get_untracked() else {
            return;
        };
        let Ok(input) = serde_json::from_str::<Value>(&input_json.get_untracked()) else {
            input_error.set(Some("Invalid JSON".to_string()));
            return;
        };
        input_error.set(None);
        triggering.set(true);
        error.set(None);
        spawn_local(async move {
            let url = format!("/api/flows/{flow}/run");
            if let Ok(response) = api_request(Method::Post, &url, Some(&json!({ "input": input }))).await {
                hide_modal(RUN_MODAL_ID);
                if !response.ok {
                    error.set(response.error());
                } else if let Ok(started) = response.json::<RunStarted>() {
                    shopify::toast(&format!("Flow \"{flow}\" started"));
                    route(&format!("/runs/{}", started.run_id));
                }
            }
            triggering.set(false);
        });
    };

    let page_runs = Memo::new(move |_| runs.with(|runs| runs.iter().take(PAGE_SIZE).cloned().collect::<Vec<_>>()));
    let all_checked = move || {
        let checked = checked_ids.get();
        page_runs.with(|runs| !runs.is_empty() && runs.iter().all(|run| checked.contains(&run.id)))
    };

    let toggle_check = move |id: String| {
        checked_ids.update(|checked| match checked.iter().position(|entry| *entry == id) {
            Some(index) => {
                checked.remove(index);
            }
            None => checked.push(id),
        });
    };

    let toggle_all = move || {
        let next = if all_checked() {
            Vec::new()
        } else {
            page_runs.with_untracked(|runs| runs.iter().map(|run| run.id.clone()).collect())
        };
        checked_ids.set(next);
    };

    let confirm_bulk_delete = move || {
        if !checked_ids.with_untracked(Vec::is_empty) {
            show_modal(DELETE_MODAL_ID);
        }
    };

    let bulk_delete = move || {
        let ids = checked_ids.get_untracked();
        if ids.is_empty() {
            return;
        }
        deleting.set(true);
        hide_modal(DELETE_MODAL_ID);
        spawn_local(async move {
            let body = json!({ "ids": ids });
            if let Ok(response) = api_request(Method::Post, "/api/runs/bulk-delete", Some(&body)).await {
                if !response.ok {
                    error.set(response.error());
                } else if let Ok(result) = response.json::<BulkDeleted>() {
                    shopify::toast(&deleted_message(&result));
                    checked_ids.set(Vec::new());
                    refresh_key.update(|key| *key += 1);
                }
            }
            deleting.set(false);
        });
    };

    let on_run = move |flow: FlowSummary| {
        let example = schema_to_example(flow.input_schema.as_ref());
        input_json.set(example.map_or_else(|| "{}".to_string(), |example| json::pretty(&Value::Object(example))));
        input_error.set(None);
        selected.set(Some(flow.name));
        show_modal(RUN_MODAL_ID);
    };

    view! {
        <style>"@keyframes fade-in { from { opacity: 0 } to { opacity: 1 } }"</style>
        <RunFlowModal
            selected
            input_json
            input_error
            triggering
            trigger_run=Callback::new(move |()| trigger_run())
        />
        <DeleteRunsModal
            checked_count=Signal::derive(move || checked_ids.with(Vec::len))
            deleting
            bulk_delete=Callback::new(move |()| bulk_delete())
        />

        <Page heading="Flows">
            <FlowListTable flows on_run=Callback::new(on_run) />

            {move || {
                error
                    .get()
                    .map(|error| view! { <s-banner tone="critical" heading="Run failed">{error}</s-banner> })
            }}

            <RunsTable
                filter
                has_next_page=Signal::derive(move || runs.with(|runs| runs.len() > PAGE_SIZE))
                page_runs
                loading_runs
                page
                checked_ids
                deleting
                search_input
                all_checked=Signal::derive(all_checked)
                on_search_input=Callback::new(on_search_input)
                toggle_all=Callback::new(move |()| toggle_all())
                toggle_check=Callback::new(toggle_check)
                confirm_bulk_delete=Callback::new(move |()| confirm_bulk_delete())
            />
        </Page>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_run_list_urls() {
        assert_eq!(runs_url(None, "", 0), "/api/runs?limit=26&offset=0");
        assert_eq!(
            runs_url(Some("syncOrders"), "failed run", 2),
            "/api/flows/syncOrders/runs?limit=26&offset=50&search=failed+run"
        );
    }

    #[test]
    fn syncs_the_search_term_into_the_page_url() {
        assert_eq!(
            search_url("/flows", "?embedded=1&shop=x", "failed"),
            "/flows?embedded=1&shop=x&q=failed"
        );
        assert_eq!(search_url("/flows", "?q=old&shop=x", "new"), "/flows?q=new&shop=x");
        assert_eq!(search_url("/flows/sync", "?q=old", ""), "/flows/sync");
    }

    #[test]
    fn summarizes_bulk_deletions() {
        assert_eq!(
            deleted_message(&BulkDeleted { deleted: 3, skipped: 0 }),
            "Deleted 3 run(s)"
        );
        assert_eq!(
            deleted_message(&BulkDeleted { deleted: 1, skipped: 2 }),
            "Deleted 1 run(s), 2 skipped (active)"
        );
    }
}
