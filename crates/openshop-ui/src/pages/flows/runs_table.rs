use crate::{
    components::table_header::TableHeader,
    dom::{self, localized, now_ms},
    format::{days_between, elapsed_ms, run_duration},
    types::{FlowRun, status_tone},
};
use leptos::prelude::*;

const SELECTION_BAR_STYLE: &str = "position:absolute;top:50%;left:0;transform:translateY(-50%);display:flex;align-items:center;gap:8px;white-space:nowrap;background:#f7f7f7;padding-right:999px;margin-right:-999px;animation:fade-in 150ms ease";

fn format_date(iso: &str) -> String {
    let date = dom::date(Some(iso));
    let time = localized(
        &date,
        "toLocaleTimeString",
        &[("hour", "2-digit"), ("minute", "2-digit")],
    );
    match days_between(now_ms(), date.get_time() as i64) {
        0 => format!("Today at {time}"),
        1 => format!("Yesterday at {time}"),
        days if days < 7 => {
            format!(
                "{} at {time}",
                localized(&date, "toLocaleDateString", &[("weekday", "long")])
            )
        }
        _ => format!(
            "{} at {time}",
            localized(&date, "toLocaleDateString", &[("day", "numeric"), ("month", "short")])
        ),
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[component]
pub fn RunsTable(
    filter: Memo<Option<String>>,
    #[prop(into)] has_next_page: Signal<bool>,
    page_runs: Memo<Vec<FlowRun>>,
    loading_runs: RwSignal<bool>,
    page: RwSignal<usize>,
    checked_ids: RwSignal<Vec<String>>,
    deleting: RwSignal<bool>,
    search_input: RwSignal<String>,
    #[prop(into)] all_checked: Signal<bool>,
    on_search_input: Callback<web_sys::Event>,
    toggle_all: Callback<()>,
    toggle_check: Callback<String>,
    confirm_bulk_delete: Callback<()>,
) -> impl IntoView {
    let select_all = move || {
        view! {
            <s-checkbox
                label="Select all runs"
                labelAccessibilityVisibility="exclusive"
                prop:checked=move || all_checked.get()
                on:change=move |_| toggle_all.run(())
            />
        }
    };
    let has_selection = Memo::new(move |_| !checked_ids.with(Vec::is_empty));
    let unfiltered = Memo::new(move |_| filter.with(Option::is_none));

    let row = move |run: FlowRun| {
        let link_id = format!("run-link-{}", run.id);
        let click_delegate = link_id.clone();
        let duration = run_duration(
            elapsed_ms(run.started_at.as_deref(), run.completed_at.as_deref()),
            &run.status,
        );
        let (checked_id, toggled_id) = (run.id.clone(), run.id.clone());
        view! {
            <s-table-row clickDelegate=click_delegate>
                <s-table-cell>
                    <span on:click=|event| event.stop_propagation()>
                        <s-checkbox
                            label=format!("Select run {}", short_id(&run.id))
                            labelAccessibilityVisibility="exclusive"
                            prop:checked=move || checked_ids.with(|checked| checked.contains(&checked_id))
                            on:change=move |_| toggle_check.run(toggled_id.clone())
                        />
                    </span>
                </s-table-cell>
                <s-table-cell>
                    <s-link id=link_id href=format!("/runs/{}", run.id)>
                        "#"
                        {short_id(&run.id)}
                    </s-link>
                </s-table-cell>
                {unfiltered.get().then(|| view! { <s-table-cell>{run.flow_name.clone()}</s-table-cell> })}
                <s-table-cell>
                    <s-badge tone=status_tone(&run.status)>{run.status.clone()}</s-badge>
                </s-table-cell>
                <s-table-cell>
                    {run.started_at.as_deref().map_or_else(|| "—".to_string(), format_date)}
                </s-table-cell>
                <s-table-cell>{duration}</s-table-cell>
            </s-table-row>
        }
    };

    view! {
        <s-section heading=move || match filter.get() {
            Some(flow) => format!("Runs — {flow}"),
            None => "Runs".to_string(),
        }>
            <s-section padding="none">
                <s-table
                    loading=move || loading_runs.get()
                    paginate
                    hasPreviousPage={move || page.get() > 0}
                    hasNextPage=move || has_next_page.get()
                    on:nextpage=move |_: web_sys::Event| page.update(|page| *page += 1)
                    on:previouspage=move |_: web_sys::Event| page.update(|page| *page = page.saturating_sub(1))
                >
                    <s-search-field
                        slot="filters"
                        label="Search runs"
                        labelAccessibilityVisibility="exclusive"
                        placeholder="Search by flow name, status, or ID..."
                        prop:value=move || search_input.get()
                        on:input=move |event| on_search_input.run(event)
                    />
                    <s-table-header-row>
                        <s-table-header>
                            <div style="position:relative">
                                {select_all()}
                                {move || {
                                    has_selection
                                        .get()
                                        .then(|| {
                                            view! {
                                                <div style=SELECTION_BAR_STYLE>
                                                    {select_all()} <span style="padding-left:8px">
                                                        <s-text color="subdued">
                                                            {move || checked_ids.with(Vec::len)} " selected"
                                                        </s-text>
                                                    </span>
                                                    <s-button
                                                        variant="tertiary"
                                                        tone="critical"
                                                        on:click=move |_| confirm_bulk_delete.run(())
                                                        disabled=move || deleting.get()
                                                    >
                                                        {move || if deleting.get() { "Deleting…" } else { "Delete" }}
                                                    </s-button>
                                                </div>
                                            }
                                        })
                                }}
                            </div>
                        </s-table-header>
                        <TableHeader list_slot="primary">"Run"</TableHeader>
                        {move || {
                            unfiltered.get().then(|| view! { <TableHeader list_slot="secondary">"Flow"</TableHeader> })
                        }}
                        <TableHeader list_slot="inline">"Status"</TableHeader>
                        <TableHeader list_slot="labeled">"Started"</TableHeader>
                        <TableHeader list_slot="labeled">"Duration"</TableHeader>
                    </s-table-header-row>
                    <s-table-body>
                        {move || {
                            (page_runs.with(Vec::is_empty) && !loading_runs.get())
                                .then(|| {
                                    view! {
                                        <s-table-row>
                                            <s-table-cell />
                                            <s-table-cell>
                                                <s-text color="subdued">"No runs yet"</s-text>
                                            </s-table-cell>
                                            {unfiltered.get().then(|| view! { <s-table-cell /> })}
                                            <s-table-cell />
                                            <s-table-cell />
                                            <s-table-cell />
                                        </s-table-row>
                                    }
                                })
                        }}
                        {move || page_runs.get().into_iter().map(row).collect_view()}
                    </s-table-body>
                </s-table>
            </s-section>
        </s-section>
    }
}
