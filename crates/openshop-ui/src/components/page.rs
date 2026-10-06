use leptos::prelude::*;

/// Renders its children one tick after being created.
///
/// Polaris expects a single `s-page` in the document, while Leptos mounts a view before
/// removing the one it replaces. Deferring lets the outgoing page leave first.
#[component]
pub fn Deferred(children: Children) -> impl IntoView {
    let ready = RwSignal::new(false);
    Effect::new(move |_| ready.set(true));
    let mut children = Some(children);
    move || ready.get().then(|| children.take().map(|children| children()))
}

/// The Polaris `s-page` of a screen.
#[component]
pub fn Page(#[prop(optional, into)] heading: MaybeProp<String>, children: Children) -> impl IntoView {
    view! {
        <Deferred>
            <s-page heading=move || heading.get()>{children()}</s-page>
        </Deferred>
    }
}
