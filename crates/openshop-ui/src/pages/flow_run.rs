mod log_query;
mod logs_section;
mod output;
mod run_action_modals;

use crate::{
    components::page::Page,
    components::table_header::TableHeader,
    dom::{self, every, locale_date_time},
    events::event_value,
    fetch::{api_fetch, api_request},
    format::{elapsed_ms, summary_duration},
    http::Method,
    query::QueryParams,
    reactive::set_if_changed,
    router::route,
    shopify,
    types::{LogEntry, LogsResponse, RunDetail, status_tone},
};
use gloo_timers::callback::Timeout;
use leptos::{prelude::*, task::spawn_local};
use leptos_router::hooks::use_params_map;
use log_query::{LevelCounts, all_logs_url, count_levels, logs_url, note_logs, page_url, parse_levels, toggle_level};
use logs_section::LogsSection;
use output::{OutputModal, build_step_rows, step_row};
use run_action_modals::{ActionFuture, RunActionModals};
use std::collections::HashSet;

async fn load_run(id: &str, run: RwSignal<Option<RunDetail>>) {
    if let Ok(response) = api_fetch(&format!("/api/runs/{id}")).await
        && response.ok
        && let Ok(detail) = response.json::<RunDetail>()
    {
        set_if_changed(run, Some(detail));
    }
}

/// Whether an answer is for the request sent last. Requests overlap: a slow
/// answer to a replaced query arrives after the answer to its successor.
fn is_current(latest: &str, answered: &str) -> bool {
    latest == answered
}

/// The server's reason, or a plain one when it gave none.
fn logs_error_message(reason: Option<String>) -> String {
    reason
        .filter(|reason| !reason.is_empty())
        .unwrap_or_else(|| "The logs could not be loaded.".to_string())
}

fn is_finished(status: &str) -> bool {
    matches!(status, "failed" | "completed" | "canceled")
}

#[component]
pub fn FlowRun() -> impl IntoView {
    let params = use_params_map();
    let id = Memo::new(move |_| params.with(|params| params.get("id")).filter(|id| !id.is_empty()));
    // Filters start from the URL search params.
    let initial_params = QueryParams::parse(&dom::location_search());
    let run = RwSignal::new(None::<RunDetail>);
    let filtered_logs = RwSignal::new(Vec::<LogEntry>::new());
    let total_logs = RwSignal::new(0_u64);
    let query = RwSignal::new(initial_params.get("q").unwrap_or_default().to_string());
    let levels = RwSignal::new(parse_levels(initial_params.get("levels")));
    let show_help = RwSignal::new(false);
    let level_counts = RwSignal::new(LevelCounts::default());
    let new_log_ids = RwSignal::new(HashSet::<String>::new());
    let known_log_ids = StoredValue::new(HashSet::<String>::new());
    let initial_load = StoredValue::new(true);
    // The address of the last log request sent: an answer to another one is stale.
    let latest_logs_url = StoredValue::new(String::new());
    let debounce = StoredValue::new_local(None::<Timeout>);
    let retrying = RwSignal::new(false);
    // Why the logs cannot be shown, such as an invalid regular expression.
    let logs_error = RwSignal::new(None::<String>);

    // App Bridge mirrors `replaceState` to the admin address bar.
    let sync_url = |query: &str, levels: &[String]| {
        dom::replace_url(&page_url(&dom::location_pathname(), query, levels));
    };

    // Run details, without logs.
    Effect::new(move |_| {
        let Some(id) = id.get() else { return };
        let load = move || {
            let id = id.clone();
            spawn_local(async move { load_run(&id, run).await });
        };
        load();
        every(3_000, load);
    });

    // Logs are filtered server-side.
    let fetch_logs = move || {
        let Some(id) = id.get_untracked() else { return };
        let filtered_url = logs_url(&id, &query.get_untracked(), &levels.get_untracked());
        latest_logs_url.set_value(filtered_url.clone());
        spawn_local(async move {
            let response = api_fetch(&filtered_url).await;
            // A slow answer to a query that was replaced since must change
            // nothing: neither the lines, nor the error of the newer one.
            let current = latest_logs_url
                .try_get_value()
                .is_some_and(|latest| is_current(&latest, &filtered_url));
            // A query the server refuses must not leave the lines of the
            // previous one on screen as if they were its result.
            if current
                && let Ok(refused) = &response
                && !refused.ok
            {
                logs_error.set(Some(logs_error_message(refused.error())));
                set_if_changed(filtered_logs, Vec::new());
            }
            if current
                && let Ok(response) = response
                && response.ok
                && let Ok(data) = response.json::<LogsResponse>()
            {
                logs_error.set(None);
                let initial = initial_load.try_get_value().unwrap_or(false);
                let fresh = known_log_ids
                    .try_update_value(|known| note_logs(known, &data.logs, initial))
                    .unwrap_or_default();
                initial_load.try_set_value(false);
                if !fresh.is_empty() {
                    new_log_ids.set(fresh);
                    Timeout::new(5_000, move || new_log_ids.set(HashSet::new())).forget();
                }
                set_if_changed(filtered_logs, data.logs);
                set_if_changed(total_logs, data.total);
            }
            // All logs once more for the level counters, in the same cycle.
            if let Ok(response) = api_fetch(&all_logs_url(&id)).await
                && response.ok
                && let Ok(data) = response.json::<LogsResponse>()
            {
                set_if_changed(level_counts, count_levels(&data.logs));
            }
        });
    };

    Effect::new(move |_| {
        id.track();
        query.track();
        levels.track();
        fetch_logs();
        every(3_000, fetch_logs);
    });

    let handle_query_input = move |event: web_sys::Event| {
        let value = event_value(&event);
        levels.with_untracked(|levels| sync_url(&value, levels));
        query.set(value);
        debounce.set_value(Some(Timeout::new(300, fetch_logs)));
    };

    let toggle = move |level: &'static str| {
        levels.update(|levels| toggle_level(levels, level));
        levels.with_untracked(|levels| sync_url(&query.get_untracked(), levels));
    };

    let retry_run = move |mode: &'static str| -> ActionFuture {
        let id = id.get_untracked();
        Box::pin(async move {
            let Some(id) = id else { return };
            if retrying.try_get_untracked() != Some(false) {
                return;
            }
            retrying.set(true);
            let url = format!("/api/runs/{id}/retry?mode={mode}");
            if let Ok(response) = api_request(Method::Post, &url, None).await {
                if response.ok {
                    load_run(&id, run).await;
                    shopify::toast(if mode == "reset" {
                        "Run restarted"
                    } else {
                        "Run resumed"
                    });
                } else {
                    shopify::toast(&response.error().unwrap_or_else(|| "Failed to retry run".to_string()));
                }
            }
            retrying.set(false);
        })
    };

    let delete_run = move |()| -> ActionFuture {
        let id = id.get_untracked();
        let flow_name = run.with_untracked(|run| run.as_ref().map(|run| run.flow_name.clone()));
        Box::pin(async move {
            let Some(id) = id else { return };
            let Ok(response) = api_request(Method::Delete, &format!("/api/runs/{id}"), None).await else {
                return;
            };
            if response.ok {
                shopify::toast("Run deleted");
                route(&format!("/flows/{}", flow_name.unwrap_or_default()));
            } else {
                shopify::toast(&response.error().unwrap_or_else(|| "Failed to delete run".to_string()));
            }
        })
    };

    let loaded = Memo::new(move |_| run.with(Option::is_some));
    let field = move |read: fn(&RunDetail) -> String| {
        Memo::new(move |_| run.with(|run| run.as_ref().map(read).unwrap_or_default()))
    };
    let run_id = field(|run| run.id.clone());
    let flow_name = field(|run| run.flow_name.clone());
    let status = field(|run| run.status.clone());
    let flow_href = move || format!("/flows/{}", flow_name.get());
    let steps = Memo::new(move |_| run.with(|run| run.as_ref().map(|run| run.steps.clone()).unwrap_or_default()));

    let run_page = move || {
        view! {
            <OutputModal />
            <RunActionModals retrying retry_run=Callback::new(retry_run) delete_run=Callback::new(delete_run) />

            <Page heading=Signal::derive(move || format!("Run #{}", run_id.get()))>
                <s-link
                    slot="breadcrumb-actions"
                    href=flow_href
                    on:click=move |event| {
                        event.prevent_default();
                        route(&format!("/flows/{}", flow_name.get_untracked()));
                    }
                >
                    {move || flow_name.get()}
                </s-link>
                {move || {
                    is_finished(&status.get())
                        .then(|| {
                            view! {
                                <s-button
                                    slot="secondary-actions"
                                    variant="secondary"
                                    commandFor="delete-modal"
                                    command="--show"
                                    tone="critical"
                                >
                                    "Delete"
                                </s-button>
                                <s-button
                                    slot="primary-action"
                                    variant="primary"
                                    disabled=move || retrying.get()
                                    commandFor="retry-modal"
                                    command="--show"
                                >
                                    "Retry"
                                </s-button>
                            }
                        })
                }}

                {move || {
                    run.with(|run| run.as_ref().and_then(|run| run.error.clone()))
                        .filter(|error| !error.is_empty())
                        .map(|error| view! { <s-banner tone="critical" heading="Error">{error}</s-banner> })
                }}

                <s-section heading="Summary">
                    <s-stack direction="inline" gap="large-200" alignItems="center">
                        <s-stack direction="inline" gap="small" alignItems="center">
                            <s-text color="subdued">"Status"</s-text>
                            <s-badge tone=move || status_tone(&status.get())>{move || status.get()}</s-badge>
                        </s-stack>
                        <s-stack direction="inline" gap="small" alignItems="center">
                            <s-text color="subdued">"Duration"</s-text>
                            <s-text type="strong">
                                {move || {
                                    run.with(|run| {
                                        run.as_ref()
                                            .map(|run| {
                                                summary_duration(
                                                    elapsed_ms(run.started_at.as_deref(), run.completed_at.as_deref()),
                                                    &run.status,
                                                )
                                            })
                                    })
                                }}
                            </s-text>
                        </s-stack>
                        <s-stack direction="inline" gap="small" alignItems="center">
                            <s-text color="subdued">"Started"</s-text>
                            <s-text>
                                {move || {
                                    run.with(|run| run.as_ref().and_then(|run| run.started_at.clone()))
                                        .map_or_else(|| "—".to_string(), |started| locale_date_time(&started))
                                }}
                            </s-text>
                        </s-stack>
                    </s-stack>
                </s-section>

                <s-section padding="none">
                    <s-table>
                        <s-table-header-row>
                            <TableHeader list_slot="primary">"Step"</TableHeader>
                            <TableHeader list_slot="inline">"Status"</TableHeader>
                            <TableHeader list_slot="labeled">"Duration"</TableHeader>
                            <TableHeader />
                        </s-table-header-row>
                        <s-table-body>
                            {move || steps.with(|steps| build_step_rows(steps)).into_iter().map(step_row).collect_view()}
                        </s-table-body>
                    </s-table>
                </s-section>

                <LogsSection
                    run_id=id
                    query
                    levels
                    level_counts
                    filtered_logs
                    logs_error
                    total_logs
                    new_log_ids
                    show_help
                    on_query_input=Callback::new(handle_query_input)
                    on_toggle_level=Callback::new(toggle)
                />
            </Page>
        }
    };

    move || {
        if loaded.get() {
            run_page().into_any()
        } else {
            view! {
                <Page heading="Run">
                    <s-section>
                        <s-spinner />
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
    fn only_the_answer_to_the_last_request_is_current() {
        assert!(is_current("/api/runs/1/logs?q=a", "/api/runs/1/logs?q=a"));
        assert!(!is_current("/api/runs/1/logs?q=%7C~%22(%22", "/api/runs/1/logs?q=a"));
    }

    #[test]
    fn a_refused_search_shows_the_reason_of_the_server() {
        assert_eq!(
            logs_error_message(Some("Invalid regex \"(\": unclosed group".to_string())),
            "Invalid regex \"(\": unclosed group"
        );
        assert_eq!(logs_error_message(None), "The logs could not be loaded.");
        assert_eq!(logs_error_message(Some(String::new())), "The logs could not be loaded.");
    }
}
