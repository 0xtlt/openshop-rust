use super::log_query::{LOG_LEVELS, LevelCounts, export_url, payload_text};
use crate::{
    dom::{self, localized},
    fetch::api_download,
    types::LogEntry,
};
use leptos::{prelude::*, task::spawn_local};
use std::collections::HashSet;

const LOG_STYLES: &str = "
            @keyframes logSlideIn {
              from { opacity: 0; max-height: 0; transform: translateY(-4px); }
              to { opacity: 1; max-height: 80px; transform: translateY(0); }
            }
            .log-line-new { animation: logSlideIn 0.4s ease-out both; }
            .log-line-highlight { background: #fff3cd; }
            .log-line-fade { transition: background 3s ease-out; background: transparent; }
          ";

fn log_level_color(level: &str) -> &'static str {
    match level {
        "info" => "#2e72d2",
        "warn" => "#b98900",
        "error" => "#d72c0d",
        _ => "inherit",
    }
}

fn download_logs(run_id: Option<String>, query: String, levels: Vec<String>, format: &'static str) {
    let Some(run_id) = run_id else { return };
    spawn_local(async move {
        if let Ok((bytes, content_type)) = api_download(&export_url(&run_id, &query, &levels, format)).await {
            dom::download_bytes(&bytes, &content_type, &format!("run-{run_id}-logs.{format}"));
        }
    });
}

#[component]
pub fn LogsSection(
    run_id: Memo<Option<String>>,
    query: RwSignal<String>,
    levels: RwSignal<Vec<String>>,
    level_counts: RwSignal<LevelCounts>,
    filtered_logs: RwSignal<Vec<LogEntry>>,
    total_logs: RwSignal<u64>,
    has_more_logs: RwSignal<bool>,
    new_log_ids: RwSignal<HashSet<String>>,
    show_help: RwSignal<bool>,
    on_query_input: Callback<web_sys::Event>,
    on_toggle_level: Callback<&'static str>,
) -> impl IntoView {
    let export = move |format: &'static str| {
        download_logs(
            run_id.get_untracked(),
            query.get_untracked(),
            levels.get_untracked(),
            format,
        );
    };
    let has_logs = Memo::new(move |_| !filtered_logs.with(Vec::is_empty));

    let log_line = move |log: LogEntry| {
        let id = log.id.clone();
        let matched = log.matched;
        let is_new = Memo::new(move |_| new_log_ids.with(|ids| ids.contains(&id)));
        let style = move || {
            let opacity = if matched == Some(false) { "opacity:0.5;" } else { "" };
            let is_search_match = matched == Some(true) && !query.with(String::is_empty);
            let background = if !is_new.get() && is_search_match {
                "background:#fff8e1;"
            } else {
                ""
            };
            format!("display:flex;gap:10px;padding:1px 4px;border-radius:3px;overflow:hidden;{opacity}{background}")
        };
        let level_style = format!(
            "color:{};font-weight:700;flex-shrink:0;min-width:40px",
            log_level_color(&log.level)
        );
        view! {
            <div
                class=move || if is_new.get() { "log-line-new log-line-highlight" } else { "log-line-fade" }
                style=style
            >
                <span style="color:#8c9196;flex-shrink:0;min-width:75px">
                    {localized(&dom::date(log.created_at.as_deref()), "toLocaleTimeString", &[])}
                </span>
                <span style=level_style>{log.level.to_uppercase()}</span>
                <span style="white-space:pre-wrap;word-break:break-word">
                    {log.message}
                    {payload_text(log.payload.as_ref())
                        .map(|payload| view! { <span style="color:#8c9196">" " {payload}</span> })}
                </span>
            </div>
        }
    };

    view! {
        <s-section heading="Logs">
            <div style="display:flex;gap:8px;align-items:center;margin-bottom:12px">
                <div style="flex:1">
                    <s-search-field
                        label="Search logs"
                        labelAccessibilityVisibility="exclusive"
                        placeholder="|= \"text\" != \"exclude\" |~ \"regex\" C:3"
                        prop:value=move || query.get()
                        on:input=move |event| on_query_input.run(event)
                    />
                </div>
                <s-button
                    variant="secondary"
                    accessibilityLabel="Show log query syntax"
                    aria-expanded=move || show_help.get().to_string()
                    on:click=move |_| show_help.update(|shown| *shown = !*shown)
                >
                    "?"
                </s-button>
            </div>

            <div style="display:flex;gap:6px;margin-bottom:12px">
                {LOG_LEVELS
                    .into_iter()
                    .map(|level| {
                        let enabled = move || levels.with(|levels| levels.iter().any(|known| known == level));
                        view! {
                            <s-button
                                variant=move || if enabled() { "primary" } else { "secondary" }
                                on:click=move |_| on_toggle_level.run(level)
                            >
                                {level.to_uppercase()}
                                " ("
                                {move || level_counts.get().get(level)}
                                ")"
                            </s-button>
                        }
                    })
                    .collect_view()}
                <span style="margin-left:auto;font-size:12px;color:#8c9196;align-self:center">
                    {move || filtered_logs.with(Vec::len)} "/" {move || total_logs.get()} " logs"
                </span>
                <s-button variant="secondary" on:click=move |_| export("json")>
                    "JSON"
                </s-button>
                <s-button variant="secondary" on:click=move |_| export("csv")>
                    "CSV"
                </s-button>
            </div>

            {move || {
                show_help
                    .get()
                    .then(|| {
                        view! {
                            <div style="margin-bottom:12px">
                                <s-banner tone="info" heading="Query syntax">
                                    <div style="font-family:monospace;font-size:12px;line-height:2">
                                        <div>
                                            <strong>"|= \"text\""</strong>
                                            " — Contains (case-insensitive)"
                                        </div>
                                        <div>
                                            <strong>"!= \"text\""</strong>
                                            " — Excludes"
                                        </div>
                                        <div>
                                            <strong>"|~ \"regex\""</strong>
                                            " — Regex match"
                                        </div>
                                        <div>
                                            <strong>"C:N"</strong>
                                            " — Show N context lines around matches"
                                        </div>
                                        <div>
                                            <strong>"B:N / A:N"</strong>
                                            " — Before / After context only"
                                        </div>
                                        <div>
                                            <strong>"last:5m"</strong>
                                            " — Last 5 minutes (s/m/h/d)"
                                        </div>
                                        <div>
                                            <strong>"from:ISO to:ISO"</strong>
                                            " — Absolute date range"
                                        </div>
                                        <div>
                                            <strong>"between:start,end"</strong>
                                            " — Date range shorthand"
                                        </div>
                                        <div>"Free text — Simple substring search"</div>
                                        <div style="margin-top:4px">
                                            "Example: " <code>"|= \"order\" != \"retry\" last:1h C:2"</code>
                                        </div>
                                    </div>
                                </s-banner>
                            </div>
                        }
                    })
            }}

            {move || {
                if has_logs.get() {
                    view! {
                        <div
                            tabindex="0"
                            role="log"
                            aria-label="Run logs"
                            style="font-family:ui-monospace, SFMono-Regular, Menlo, monospace;font-size:12px;line-height:1.7;max-height:500px;overflow-y:auto;border-radius:8px;background:#fafafa;padding:8px 12px"
                        >
                            <style>{LOG_STYLES}</style>
                            <For
                                each=move || filtered_logs.get()
                                key=|log| (log.id.clone(), log.matched)
                                children=log_line
                            />
                        </div>
                        <Show when=move || has_more_logs.get()>
                            <s-text color="subdued">
                                "Showing the newest lines only. Export the logs for the whole run."
                            </s-text>
                        </Show>
                    }
                        .into_any()
                } else {
                    view! {
                        <s-text color="subdued">
                            {move || if total_logs.get() == 0 { "No logs." } else { "No matching logs." }}
                        </s-text>
                    }
                        .into_any()
                }
            }}
        </s-section>
    }
}
