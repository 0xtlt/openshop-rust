use leptos::prelude::*;

/// A Polaris `s-table-header`.
///
/// Going through a component keeps the header out of Leptos's static HTML templates. Elements
/// cloned from a template are only upgraded once connected, after their `s-table`, which then
/// warns that it has no primary column.
#[component]
pub fn TableHeader(
    #[prop(optional)] list_slot: Option<&'static str>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! { <s-table-header listSlot=list_slot>{children.map(|children| children())}</s-table-header> }
}
