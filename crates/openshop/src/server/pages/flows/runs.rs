//! The runs of the page: the query of the URL that lists them (search,
//! filters, page, selection), their table and its pages, and the
//! confirmation of the deletion of those selected.

use chrono::{DateTime, NaiveDate, Utc};
use indexmap::IndexMap;
use maud::{Markup, html};

use super::super::{BuiltinPage, ui};
use super::format::run_duration;
use super::super::ui::{status_tone, time_ago};
use super::{DELETE, DELETE_FORM, DELETE_MODAL, FlowsView, IDS, PAGE_SIZE, REFRESH_MS};
use crate::db::schema::FlowRun;
use crate::runtime::time::to_iso;
use crate::types::{ACTIVE_RUN_STATUSES, ALL_RUN_STATUSES};

/// The runs the URL asks for: `?q=sync&status=failed&from=2026-11-01&to=2026-11-09&page=2&select=all`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct RunsQuery {
    /// In the flow, the status and the id of a run.
    pub(super) search: String,
    pub(super) status: String,
    /// Dates, `2026-11-09`; `to` includes its day.
    pub(super) from: String,
    pub(super) to: String,
    /// The page, from 1.
    pub(super) page: usize,
    /// Every run of the page is selected (`select=all`).
    pub(super) all_selected: bool,
}

impl RunsQuery {
    pub(super) fn of(page: &BuiltinPage<'_>) -> Self {
        let param = |name: &str| page.param(name).unwrap_or_default();
        Self {
            search: param("q"),
            status: param("status"),
            from: param("from"),
            to: param("to"),
            page: param("page").parse().ok().filter(|page| *page >= 1).unwrap_or(1),
            all_selected: param("select") == "all",
        }
    }

    fn is_filtered(&self) -> bool {
        [&self.search, &self.status, &self.from, &self.to]
            .iter()
            .any(|value| !value.is_empty())
    }

    /// What `query_runs` is given: one run more than a page holds, which
    /// says whether another page follows.
    pub(super) fn api(&self) -> IndexMap<String, String> {
        // A day, `to`, ends at its last millisecond.
        let to = match NaiveDate::parse_from_str(&self.to, "%Y-%m-%d") {
            Ok(_) => format!("{}T23:59:59.999Z", self.to),
            Err(_) => self.to.clone(),
        };
        let offset = (self.page - 1).saturating_mul(PAGE_SIZE);
        [
            ("limit", (PAGE_SIZE + 1).to_string()),
            ("offset", offset.to_string()),
            ("search", self.search.clone()),
            ("status", self.status.clone()),
            ("from", self.from.clone()),
            ("to", to),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect()
    }

    /// The URL of `pathname` with these filters, at `page`, with every run selected or none.
    pub(super) fn href(&self, pathname: &str, page: usize, all_selected: bool) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in [
            ("q", &self.search),
            ("status", &self.status),
            ("from", &self.from),
            ("to", &self.to),
        ] {
            if !value.is_empty() {
                query.append_pair(name, value);
            }
        }
        if page > 1 {
            query.append_pair("page", &page.to_string());
        }
        if all_selected {
            query.append_pair("select", "all");
        }
        let query = query.finish();
        if query.is_empty() { pathname.to_string() } else { format!("{pathname}?{query}") }
    }
}

/// The runs of the page: a link to every flow's when it lists one flow's,
/// the selection and its deletion, the table with its filters, and its pages.
/// Fetched again while one of them is active.
pub(super) fn runs_section(page: &FlowsView<'_>) -> Markup {
    let query = page.query;
    let every_flow = page.selected.is_none();
    let heading = page
        .selected
        .map_or_else(|| "Runs".to_string(), |flow| format!("Runs — {flow}"));
    let active = page
        .runs
        .iter()
        .any(|run| ACTIVE_RUN_STATUSES.contains(&run.status.as_str()));
    let empty = if query.page > 1 {
        "No runs on this page"
    } else if query.is_filtered() {
        "No runs match these filters"
    } else {
        "No runs yet"
    };
    html! {
        s-section heading=(heading) data-openshop-refresh=[active.then_some(REFRESH_MS)] {
            @if page.selected.is_some() || !page.runs.is_empty() {
                s-stack direction="inline" justifyContent="space-between" alignItems="center" gap="small-200" {
                    @if page.selected.is_some() {
                        s-link href="/flows" { "Runs of every flow" }
                    } @else {
                        span {}
                    }
                    @if !page.runs.is_empty() {
                        s-stack direction="inline" gap="small-200" {
                            s-button variant="tertiary" href=(query.href(page.pathname, query.page, !query.all_selected)) {
                                (if query.all_selected { "Clear selection" } else { "Select all" })
                            }
                            s-button variant="secondary" tone="critical" commandFor=(DELETE_MODAL) command="--show" {
                                "Delete selected"
                            }
                        }
                    }
                }
            }
            s-section padding="none" {
                s-table {
                    (filters(page))
                    s-table-header-row {
                        s-table-header {}
                        s-table-header listSlot="primary" { "Run" }
                        @if every_flow { s-table-header listSlot="secondary" { "Flow" } }
                        s-table-header listSlot="inline" { "Status" }
                        s-table-header listSlot="labeled" { "Started" }
                        s-table-header listSlot="labeled" { "Duration" }
                    }
                    s-table-body {
                        @if page.runs.is_empty() {
                            s-table-row {
                                s-table-cell {}
                                s-table-cell { s-text color="subdued" { (empty) } }
                                @if every_flow { s-table-cell {} }
                                s-table-cell {} s-table-cell {} s-table-cell {}
                            }
                        }
                        @for run in page.runs {
                            (run_row(run, every_flow, query.all_selected, page.now))
                        }
                    }
                }
                (pagination(page))
            }
        }
    }
}

/// The search and the filters of the runs: a form that is a navigation,
/// back to the first page.
fn filters(page: &FlowsView<'_>) -> Markup {
    let query = page.query;
    html! {
        form method="get" slot="filters" {
            s-stack direction="inline" gap="small-200" alignItems="center" {
                s-search-field
                    name="q"
                    label="Search runs"
                    labelAccessibilityVisibility="exclusive"
                    placeholder="Search by flow name, status, or ID..."
                    value=(query.search) {}
                s-select name="status" label="Status" labelAccessibilityVisibility="exclusive" value=(query.status) {
                    s-option value="" selected[query.status.is_empty()] { "All statuses" }
                    @for status in ALL_RUN_STATUSES {
                        s-option value=(status) selected[query.status == status] { (capitalized(status)) }
                    }
                }
                s-date-field name="from" label="From" labelAccessibilityVisibility="exclusive" placeholder="From" value=(query.from) {}
                s-date-field name="to" label="To" labelAccessibilityVisibility="exclusive" placeholder="To" value=(query.to) {}
                s-button type="submit" { "Filter" }
                @if query.is_filtered() {
                    s-button variant="tertiary" href=(page.pathname) { "Clear" }
                }
            }
        }
    }
}

fn capitalized(text: &str) -> String {
    let mut letters = text.chars();
    letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default()
}

/// A run: its checkbox, which belongs to the form of the confirmation, a
/// link to its page (the whole row follows it), its status, when it
/// started and how long it took.
fn run_row(run: &FlowRun, every_flow: bool, checked: bool, now: DateTime<Utc>) -> Markup {
    let id = run.id.to_string();
    let short: String = id.chars().take(8).collect();
    let link = format!("run-link-{id}");
    html! {
        s-table-row clickDelegate=(link) {
            s-table-cell {
                s-checkbox
                    form=(DELETE_FORM)
                    name=(IDS)
                    value=(id)
                    label=(format!("Select run {short}"))
                    labelAccessibilityVisibility="exclusive"
                    checked[checked] {}
            }
            s-table-cell { s-link id=(link) href=(format!("/runs/{id}")) { "#" (short) } }
            @if every_flow { s-table-cell { (run.flow_name) } }
            s-table-cell { s-badge tone=(status_tone(&run.status)) { (run.status) } }
            s-table-cell {
                @match run.started_at {
                    Some(started) => {
                        s-stack gap="none" {
                            s-text { (time_ago(now, started)) }
                            s-text color="subdued" { (ui::time(&to_iso(&started))) }
                        }
                    }
                    None => "—",
                }
            }
            s-table-cell { (run_duration(run)) }
        }
    }
}

/// The links to the page before and the page after, which keep the filters.
fn pagination(page: &FlowsView<'_>) -> Markup {
    let query = page.query;
    let previous = (query.page > 1).then(|| query.href(page.pathname, query.page - 1, false));
    let next = page
        .has_next
        .then(|| query.href(page.pathname, query.page + 1, false));
    html! {
        @if previous.is_some() || next.is_some() {
            s-stack direction="inline" justifyContent="center" alignItems="center" gap="small-200" padding="base" {
                s-button icon="chevron-left" accessibilityLabel="Previous page" href=[previous.as_deref()] disabled[previous.is_none()] {}
                s-text color="subdued" { "Page " (query.page) }
                s-button icon="chevron-right" accessibilityLabel="Next page" href=[next.as_deref()] disabled[next.is_none()] {}
            }
        }
    }
}

/// The confirmation of the deletion: the form the checkboxes of the runs belong to.
pub(super) fn delete_modal() -> Markup {
    html! {
        form id=(DELETE_FORM) method="post" {
            input type="hidden" name="_action" value=(DELETE);
            s-modal id=(DELETE_MODAL) heading="Delete runs" {
                s-paragraph { "Are you sure you want to delete the selected runs? This action cannot be undone." }
                s-button slot="primary-action" variant="primary" tone="critical" type="submit" { "Delete" }
                s-button slot="secondary-actions" commandFor=(DELETE_MODAL) command="--hide" { "Cancel" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_for_one_page_of_runs_and_one_more() {
        let query = RunsQuery {
            search: "sync".to_string(),
            to: "2026-11-09".to_string(),
            page: 3,
            ..RunsQuery::default()
        };
        let api = query.api();
        assert_eq!(api["limit"], "26");
        assert_eq!(api["offset"], "50");
        assert_eq!(api["search"], "sync");
        assert_eq!(api["status"], "");
        // The whole day.
        assert_eq!(api["to"], "2026-11-09T23:59:59.999Z");
        assert_eq!(query.href("/flows", 1, false), "/flows?q=sync&to=2026-11-09");
        assert_eq!(
            query.href("/flows/a%2Fb", 2, true),
            "/flows/a%2Fb?q=sync&to=2026-11-09&page=2&select=all"
        );
        assert_eq!(capitalized("canceled"), "Canceled");
    }
}
