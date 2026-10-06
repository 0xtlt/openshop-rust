//! Application-defined admin pages: registration, route matching and the access gate.

use crate::{
    components::page::{Deferred, Page},
    fetch::api_json,
    pages::disabled::Disabled,
    url::decode_uri_component,
};
use leptos::{prelude::*, task::spawn_local};
use leptos_router::hooks::use_location;
use openshop_shared::{
    CustomAdminPagesResponse, custom_admin_route_shape_key, is_safe_custom_admin_path, match_custom_admin_path,
    rpc::encode_uri_component,
};
use serde::Deserialize;
use std::{
    cmp::Ordering,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
};

/// Values of the dynamic segments of a custom page route, e.g. `id` for `/reviews/:id`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteParams(Vec<(String, String)>);

impl RouteParams {
    /// The percent-decoded value of a dynamic segment.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

/// A function rendering a custom admin page: `Fn() -> impl IntoView` (for instance a
/// `#[component]` without props) or, to read dynamic route segments,
/// `Fn(RouteParams) -> impl IntoView`.
pub trait CustomPageView<Marker>: Send + Sync + 'static {
    #[doc(hidden)]
    fn render(&self, params: RouteParams) -> AnyView;
}

#[doc(hidden)]
pub struct WithoutParams;

#[doc(hidden)]
pub struct WithParams;

impl<F, V> CustomPageView<WithoutParams> for F
where
    F: Fn() -> V + Send + Sync + 'static,
    V: IntoView + 'static,
{
    fn render(&self, _params: RouteParams) -> AnyView {
        self().into_any()
    }
}

impl<F, V> CustomPageView<WithParams> for F
where
    F: Fn(RouteParams) -> V + Send + Sync + 'static,
    V: IntoView + 'static,
{
    fn render(&self, params: RouteParams) -> AnyView {
        self(params).into_any()
    }
}

type PageView = Arc<dyn Fn(RouteParams) -> AnyView + Send + Sync>;

/// A custom admin page: a route pattern and the view rendered for it.
#[derive(Clone)]
pub struct CustomAdminPage {
    id: String,
    route_pattern: String,
    view: PageView,
}

impl CustomAdminPage {
    /// Declares a page for a pattern such as `/reviews` or `/reviews/:id`.
    ///
    /// The id defaults to the pattern written as a folder path (`reviews/[id]`).
    pub fn new<Marker>(route_pattern: &str, view: impl CustomPageView<Marker>) -> Self {
        Self {
            id: default_page_id(route_pattern),
            route_pattern: route_pattern.to_string(),
            view: Arc::new(move |params| view.render(params)),
        }
    }

    /// Overrides the stable id of the page.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn route_pattern(&self) -> &str {
        &self.route_pattern
    }
}

fn pattern_segments(pattern: &str) -> impl Iterator<Item = &str> {
    pattern.split('/').filter(|segment| !segment.is_empty())
}

fn default_page_id(route_pattern: &str) -> String {
    pattern_segments(route_pattern)
        .map(|segment| match segment.strip_prefix(':') {
            Some(name) => format!("[{name}]"),
            None => segment.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Static segments sort before dynamic ones so `/reviews/new` wins over `/reviews/:id`.
fn compare_patterns(left: &str, right: &str) -> Ordering {
    let mut left = left.split('/');
    let mut right = right.split('/');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) if left == right => {}
            (Some(left), Some(right)) => {
                return match (left.starts_with(':'), right.starts_with(':')) {
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    _ => left.cmp(right),
                };
            }
        }
    }
}

fn match_route(pattern: &str, pathname: &str) -> Option<RouteParams> {
    let pattern: Vec<&str> = pattern_segments(pattern).collect();
    let path: Vec<&str> = pattern_segments(pathname).collect();
    if pattern.len() != path.len() {
        return None;
    }
    let mut params = Vec::new();
    for (pattern, segment) in pattern.into_iter().zip(path) {
        match pattern.strip_prefix(':') {
            Some(name) => params.push((name.to_string(), decode_uri_component(segment))),
            None if pattern == segment => {}
            None => return None,
        }
    }
    Some(RouteParams(params))
}

/// The custom pages compiled into this bundle, most specific route first.
#[derive(Clone, Default)]
pub struct CustomAdminPages(Arc<[CustomAdminPage]>);

impl CustomAdminPages {
    /// # Panics
    ///
    /// Panics when a route is reserved or malformed, or when two routes have the same shape.
    pub fn new(mut pages: Vec<CustomAdminPage>) -> Self {
        pages.sort_by(|left, right| compare_patterns(&left.route_pattern, &right.route_pattern));
        for (index, page) in pages.iter().enumerate() {
            assert!(
                is_safe_custom_admin_path(&page.route_pattern, true),
                "[openshop] Custom admin page path \"{}\" is reserved or invalid",
                page.route_pattern
            );
            let shape = custom_admin_route_shape_key(&page.route_pattern);
            if let Some(existing) = pages[..index]
                .iter()
                .find(|other| custom_admin_route_shape_key(&other.route_pattern) == shape)
            {
                panic!(
                    "[openshop] Custom admin page routes \"{}\" and \"{}\" have the same route shape",
                    existing.route_pattern, page.route_pattern
                );
            }
        }
        Self(pages.into())
    }

    pub fn route_patterns(&self) -> Vec<String> {
        self.0.iter().map(|page| page.route_pattern.clone()).collect()
    }

    fn find(&self, pathname: &str) -> Option<(usize, RouteParams)> {
        self.0
            .iter()
            .enumerate()
            .find_map(|(index, page)| Some((index, match_route(&page.route_pattern, pathname)?)))
    }
}

#[derive(Clone, Copy)]
pub struct CustomAdminPagesContext(pub ReadSignal<CustomAdminPagesResponse>);

pub fn use_custom_admin_pages() -> ReadSignal<CustomAdminPagesResponse> {
    expect_context::<CustomAdminPagesContext>().0
}

fn page_unavailable(reason: &'static str) -> impl IntoView {
    view! {
        <Page heading="Page unavailable">
            <s-banner tone="critical">{reason}</s-banner>
        </Page>
    }
}

#[component]
pub fn CustomAdminErrorBoundary(children: Children) -> impl IntoView {
    view! {
        <ErrorBoundary fallback=|_| {
            page_unavailable("This custom page could not be displayed.")
        }>{children()}</ErrorBoundary>
    }
}

/// Renders the custom page matching the current path; the router falls back to it when no
/// built-in route matches.
#[component]
pub fn CustomAdminPageOutlet() -> impl IntoView {
    let pages = expect_context::<CustomAdminPages>();
    let declared = use_custom_admin_pages();
    let pathname = use_location().pathname;
    let matched = Memo::new({
        let pages = pages.clone();
        move |_| pages.find(&pathname.get())
    });
    // The server knows the page, but this bundle was built without it.
    let missing_from_bundle = Memo::new(move |_| {
        matched.with(Option::is_none)
            && declared.with(|declared| {
                let pathname = pathname.get();
                declared
                    .pages
                    .iter()
                    .any(|page| match_custom_admin_path(&page.path, &pathname))
            })
    });

    move || match matched.get() {
        Some((index, params)) => {
            let view = Arc::clone(&pages.0[index].view);
            view! {
                <CustomAdminErrorBoundary>
                    <Deferred>{view(params)}</Deferred>
                </CustomAdminErrorBoundary>
            }
            .into_any()
        }
        None if missing_from_bundle.get() => {
            page_unavailable("This custom page is not included in the admin UI bundle.").into_any()
        }
        None => ().into_any(),
    }
}

#[derive(Deserialize)]
struct RouteAccess {
    allowed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Gate {
    Open,
    Checking,
    Denied,
}

/// Asks the server whether the current custom page may be shown before rendering it.
#[component]
pub fn CustomAdminPageGate(children: ChildrenFn) -> impl IntoView {
    let pages = expect_context::<CustomAdminPages>();
    let pathname = use_location().pathname;
    let is_custom = Memo::new(move |_| pages.find(&pathname.get()).is_some());
    let route_access = RwSignal::new(None::<(String, bool)>);

    Effect::new(move |_| {
        let path = pathname.get();
        if !is_custom.get() {
            return;
        }
        let active = Arc::new(AtomicBool::new(true));
        on_cleanup({
            let active = Arc::clone(&active);
            move || active.store(false, AtomicOrdering::Relaxed)
        });
        spawn_local(async move {
            let url = format!("/api/pages/custom/access?path={}", encode_uri_component(&path));
            let allowed = api_json::<RouteAccess>(&url).await.is_ok_and(|access| access.allowed);
            if active.load(AtomicOrdering::Relaxed) {
                route_access.set(Some((path, allowed)));
            }
        });
    });

    let gate = Memo::new(move |_| {
        if !is_custom.get() {
            return Gate::Open;
        }
        match route_access.get() {
            Some((path, allowed)) if path == pathname.get() => {
                if allowed {
                    Gate::Open
                } else {
                    Gate::Denied
                }
            }
            _ => Gate::Checking,
        }
    });

    move || match gate.get() {
        Gate::Open => children().into_any(),
        Gate::Checking => view! { <Page>"Loading..."</Page> }.into_any(),
        Gate::Denied => view! { <Disabled /> }.into_any(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(route_pattern: &str) -> CustomAdminPage {
        CustomAdminPage::new(route_pattern, || ())
    }

    #[test]
    fn accepts_views_with_and_without_route_params() {
        fn static_page() -> impl IntoView {}
        fn dynamic_page(params: RouteParams) -> impl IntoView {
            params.get("id").map(str::to_string)
        }
        let pages = CustomAdminPages::new(vec![
            CustomAdminPage::new("/reviews", static_page),
            CustomAdminPage::new("/reviews/:id", dynamic_page),
            CustomAdminPage::new("/orders/:id", |params: RouteParams| {
                params.get("id").map(str::to_string)
            }),
        ]);
        assert_eq!(pages.route_patterns(), ["/orders/:id", "/reviews", "/reviews/:id"]);
    }

    #[test]
    fn derives_ids_from_route_patterns() {
        assert_eq!(page("/reviews").id(), "reviews");
        assert_eq!(page("/reviews/:id").id(), "reviews/[id]");
        assert_eq!(page("/reviews/:id").with_id("review-details").id(), "review-details");
    }

    #[test]
    fn extracts_decoded_route_params() {
        let params = match_route("/reviews/:id/:tab", "/reviews/gid%3A%2F%2Fx%2F1/notes/").unwrap();
        assert_eq!(params.get("id"), Some("gid://x/1"));
        assert_eq!(params.get("tab"), Some("notes"));
        assert_eq!(params.get("other"), None);
        assert_eq!(params.iter().count(), 2);
        assert_eq!(match_route("/reviews", "/reviews"), Some(RouteParams::default()));
        assert_eq!(match_route("/reviews/:id", "/reviews"), None);
        assert_eq!(match_route("/reviews/:id", "/orders/1"), None);
    }

    #[test]
    fn prefers_static_routes_over_dynamic_ones() {
        let pages = CustomAdminPages::new(vec![page("/reviews/:id"), page("/reviews/new"), page("/reviews")]);
        assert_eq!(pages.route_patterns(), ["/reviews", "/reviews/new", "/reviews/:id"]);
        assert_eq!(pages.find("/reviews/new").map(|(index, _)| index), Some(1));
        let (index, params) = pages.find("/reviews/42").unwrap();
        assert_eq!((index, params.get("id")), (2, Some("42")));
        assert!(pages.find("/orders").is_none());
    }

    #[test]
    #[should_panic(expected = "is reserved or invalid")]
    fn rejects_reserved_routes() {
        CustomAdminPages::new(vec![page("/flows/custom")]);
    }

    #[test]
    #[should_panic(expected = "have the same route shape")]
    fn rejects_routes_with_the_same_shape() {
        CustomAdminPages::new(vec![page("/reviews/:id"), page("/reviews/:slug")]);
    }
}
