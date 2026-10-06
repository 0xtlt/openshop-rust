use crate::pages::disabled::Disabled;
use leptos::prelude::*;
use leptos_router::hooks::use_location;
use openshop_shared::{AdminPageMode, ResolvedAdminPages, admin_page_from_ui_path};

#[derive(Clone, Copy)]
pub struct AdminPagesContext(pub ReadSignal<ResolvedAdminPages>);

pub fn use_admin_pages() -> ReadSignal<ResolvedAdminPages> {
    expect_context::<AdminPagesContext>().0
}

#[component]
pub fn AdminPageGate(children: ChildrenFn) -> impl IntoView {
    let pathname = use_location().pathname;
    let pages = use_admin_pages();
    let disabled = Memo::new(move |_| {
        admin_page_from_ui_path(&pathname.get()).is_some_and(|page| pages.get().get(page) == AdminPageMode::Disabled)
    });

    move || {
        if disabled.get() {
            view! { <Disabled /> }.into_any()
        } else {
            children().into_any()
        }
    }
}
