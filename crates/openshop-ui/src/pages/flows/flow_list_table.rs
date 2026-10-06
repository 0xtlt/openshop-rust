use crate::components::table_header::TableHeader;
use crate::types::FlowSummary;
use leptos::prelude::*;

#[component]
pub fn FlowListTable(flows: RwSignal<Vec<FlowSummary>>, on_run: Callback<FlowSummary>) -> impl IntoView {
    let row = move |flow: FlowSummary| {
        let scheduled = !flow.crons.is_empty();
        let last = flow.crons.len().saturating_sub(1);
        let schedules = flow
            .crons
            .iter()
            .enumerate()
            .map(|(index, cron)| {
                let margin = if index < last { "4px" } else { "0" };
                view! {
                    <code style=format!(
                        "display:block;font-size:13px;background:#f1f1f1;padding:2px 6px;border-radius:4px;margin-bottom:{margin}",
                    )>{cron.schedule.clone()}</code>
                }
            })
            .collect_view();
        let name = flow.name.clone();
        view! {
            <s-table-row>
                <s-table-cell>{name}</s-table-cell>
                <s-table-cell>{if scheduled { schedules.into_any() } else { "—".into_any() }}</s-table-cell>
                <s-table-cell>
                    <s-badge tone=if scheduled { "info" } else { "neutral" }>
                        {if scheduled { "Scheduled" } else { "Manual" }}
                    </s-badge>
                </s-table-cell>
                <s-table-cell>
                    <s-button variant="primary" on:click=move |_| on_run.run(flow.clone())>
                        "Run"
                    </s-button>
                </s-table-cell>
            </s-table-row>
        }
    };

    view! {
        <s-section padding="none">
            <s-table>
                <s-table-header-row>
                    <TableHeader list_slot="primary">"Flow"</TableHeader>
                    <TableHeader list_slot="secondary">"Schedule"</TableHeader>
                    <TableHeader list_slot="inline">"Type"</TableHeader>
                    <TableHeader />
                </s-table-header-row>
                <s-table-body>{move || flows.get().into_iter().map(row).collect_view()}</s-table-body>
            </s-table>
        </s-section>
    }
}
