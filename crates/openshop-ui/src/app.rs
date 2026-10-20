use crate::{
    admin_pages::{AdminPageGate, AdminPagesContext, use_admin_pages},
    custom_admin_pages::{
        CustomAdminPage, CustomAdminPageGate, CustomAdminPageOutlet, CustomAdminPages, CustomAdminPagesContext,
        CustomPageView, use_custom_admin_pages, use_custom_home,
    },
    dom::{self, every, hold_until_cleanup, listen},
    fetch::{ApiError, api_json},
    navigation::{self, NavigateEvent, NavigateTarget},
    pages::{
        crons::Crons, flow_run::FlowRun, flows::Flows, functions::Functions, home::Home, mcp::Mcp, providers::Providers,
    },
    report::{log_error, report_error},
    router, shopify,
};
use futures_util::future::join;
use gloo_timers::future::TimeoutFuture;
use leptos::{mount::mount_to, prelude::*, task::spawn_local};
use leptos_router::{
    NavigateOptions,
    components::{FlatRoutes, Route, Router},
    hooks::{use_location, use_navigate},
    location::{BrowserUrl, LocationProvider},
    path,
};
use openshop_shared::{
    AdminPageMode, CustomAdminPagesResponse, ResolvedAdminPages, resolve_admin_pages, same_admin_pages,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Element, Event, HtmlElement, VisibilityState};

const PAGES_REFRESH_MS: u32 = 10_000;

/// The embedded admin UI, optionally extended with custom admin pages.
///
/// ```no_run
/// use leptos::prelude::*;
/// use openshop_ui::{App, RouteParams};
///
/// #[component]
/// fn Reviews() -> impl IntoView {
///     view! { <s-page heading="Reviews"></s-page> }
/// }
///
/// fn review(params: RouteParams) -> impl IntoView {
///     let heading = format!("Review {}", params.get("id").unwrap_or_default());
///     view! { <s-page heading=heading></s-page> }
/// }
///
/// App::new().page("/reviews", Reviews).page("/reviews/:id", review).mount();
/// ```
#[derive(Clone, Default)]
pub struct App {
    pages: Vec<CustomAdminPage>,
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a custom admin page for a route pattern such as `/reviews` or `/reviews/:id`.
    ///
    /// `view` is a function without arguments (for instance a `#[component]` without props), or
    /// one taking the [`RouteParams`](crate::RouteParams) of the dynamic segments. The server
    /// decides who may open the page and which navigation entries point to it.
    pub fn page<Marker>(self, route_pattern: &str, view: impl CustomPageView<Marker>) -> Self {
        self.custom_page(CustomAdminPage::new(route_pattern, view))
    }

    /// Registers a custom admin page built with [`CustomAdminPage`], e.g. to set its id.
    pub fn custom_page(mut self, page: CustomAdminPage) -> Self {
        self.pages.push(page);
        self
    }

    /// Mounts the UI into `<div id="root">`.
    ///
    /// # Panics
    ///
    /// Panics when a custom page route is reserved or malformed, when two custom pages have
    /// the same route shape, or when the document has no `#root` element.
    pub fn mount(self) {
        console_error_panic_hook::set_once();
        let custom_pages = CustomAdminPages::new(self.pages);
        let root: HtmlElement = dom::element_by_id("root")
            .expect("index.html must contain <div id=\"root\">")
            .unchecked_into();
        mount_to(root, move || view! { <Root custom_pages /> }).forget();
    }
}

async fn load_custom_admin_pages() -> Result<CustomAdminPagesResponse, ApiError> {
    api_json("/api/pages/custom").await
}

fn current_url() -> Signal<String> {
    let location = use_location();
    Signal::derive(move || {
        let search = location.search.get();
        let pathname = location.pathname.get();
        if search.is_empty() {
            pathname
        } else {
            format!("{pathname}?{search}")
        }
    })
}

#[component]
fn NavLink(href: String, #[prop(into)] current: Signal<bool>, children: Children) -> impl IntoView {
    view! {
        <a href=href aria-current=move || current.get().then_some("page")>
            {children()}
        </a>
    }
}

#[component]
fn NavMenu() -> impl IntoView {
    let url = current_url();
    let pages = use_admin_pages();
    let custom_pages = use_custom_admin_pages();
    let builtin =
        move |mode: fn(&ResolvedAdminPages) -> AdminPageMode, href: &'static str, label: &'static str, prefix: bool| {
            move || {
                (mode(&pages.get()) == AdminPageMode::Visible).then(|| {
                    let current = Signal::derive(move || {
                        url.with(|url| if prefix { url.starts_with(href) } else { url == href })
                    });
                    view! { <NavLink href=href.to_string() current>{label}</NavLink> }
                })
            }
        };

    view! {
        <ui-nav-menu>
            <a href="/" rel="home" aria-current=move || (url.get() == "/").then_some("page")>
                "Home"
            </a>
            {builtin(|pages| pages.flows, "/flows", "Flows", true)}
            {builtin(|pages| pages.providers, "/providers", "Providers", false)}
            {builtin(|pages| pages.mcp, "/mcp", "MCP", false)}
            {builtin(|pages| pages.crons, "/crons", "Crons", false)}
            {builtin(|pages| pages.functions, "/functions", "Functions", true)}
            <For
                each=move || custom_pages.get().navigation
                key=|item| (item.path.clone(), item.label.clone())
                children=move |item| {
                    let path = item.path.clone();
                    let current = Signal::derive(move || url.with(|url| *url == path));
                    view! { <NavLink href=item.path current>{item.label}</NavLink> }
                }
            />
        </ui-nav-menu>
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthStatus {
    Checking,
    Ready,
    /// There is no App Bridge session: the app is not open in Shopify admin.
    Blocked,
    /// There is a session, and what the app needs to start could not be loaded.
    Failed,
}

/// Waits for an App Bridge session, then keeps the page visibility config fresh.
#[component]
fn AuthGate(children: ChildrenFn) -> impl IntoView {
    let location = use_location();
    let status = RwSignal::new(AuthStatus::Checking);
    // Why the app could not start, when `status` is `Failed`.
    let failure = RwSignal::new(String::new());
    let pages = RwSignal::new(resolve_admin_pages(None, None));
    let custom_pages = RwSignal::new(CustomAdminPagesResponse::default());
    provide_context(AdminPagesContext(pages.read_only()));
    provide_context(CustomAdminPagesContext(custom_pages.read_only()));

    spawn_local(async move {
        let mut attempt = 0;
        while !shopify::has_id_token() && attempt < 20 {
            TimeoutFuture::new(100).await;
            attempt += 1;
        }
        match shopify::id_token().await {
            Ok(Some(token)) if !token.is_empty() => {}
            Ok(_) => {
                status.set(AuthStatus::Blocked);
                return;
            }
            Err(error) => {
                log_error("Could not get a session token from App Bridge", &error);
                failure.set(format!("Could not get a session from Shopify: {error}"));
                status.set(AuthStatus::Failed);
                return;
            }
        }
        let (data, custom_data) = join(api_json::<ResolvedAdminPages>("/api/pages"), load_custom_admin_pages()).await;
        let data = match data {
            Ok(data) => data,
            Err(error) => {
                log_error("Could not load the pages of the app", &error);
                failure.set(format!("Could not load the app: {error}"));
                status.set(AuthStatus::Failed);
                return;
            }
        };
        // Without its own pages, the app still opens on the built-in ones.
        match custom_data {
            Ok(custom_data) => custom_pages.set(custom_data),
            Err(error) => report_error(
                "Could not load the custom pages of the app",
                &error,
                &format!("Could not load the pages of the app: {error}"),
            ),
        }
        pages.set(data);
        status.set(AuthStatus::Ready);
    });

    Effect::new(move |_| {
        location.pathname.track();
        location.search.track();
        if status.get() != AuthStatus::Ready {
            return;
        }
        let active = Arc::new(AtomicBool::new(true));
        on_cleanup({
            let active = Arc::clone(&active);
            move || active.store(false, Ordering::Relaxed)
        });

        let refresh = move || {
            let active = Arc::clone(&active);
            spawn_local(async move {
                let (data, custom_data) =
                    join(api_json::<ResolvedAdminPages>("/api/pages"), load_custom_admin_pages()).await;
                if !active.load(Ordering::Relaxed) {
                    return;
                }
                // A refresh that fails keeps what is shown: the next one tries again.
                match data {
                    Ok(data) => {
                        if pages.try_with_untracked(|current| !same_admin_pages(current, &data)) == Some(true) {
                            pages.set(data);
                        }
                    }
                    Err(error) => log_error("Could not refresh the pages of the app", &error),
                }
                match custom_data {
                    Ok(custom_data) => {
                        if custom_pages.try_with_untracked(|current| *current != custom_data) == Some(true) {
                            custom_pages.set(custom_data);
                        }
                    }
                    Err(error) => log_error("Could not refresh the custom pages of the app", &error),
                }
            });
        };

        refresh();
        every(PAGES_REFRESH_MS, refresh.clone());
        let document = document();
        hold_until_cleanup(listen(&document, "visibilitychange", {
            let (refresh, document) = (refresh.clone(), document.clone());
            move |_| {
                if document.visibility_state() == VisibilityState::Visible {
                    refresh();
                }
            }
        }));
        hold_until_cleanup(listen(&window(), "focus", move |_| refresh()));
    });

    move || {
        match status.get() {
        AuthStatus::Ready => children().into_any(),
        // The page the server gives a visitor without a shop, for one without a session.
        status => view! {
            <s-page inlineSize="small">
                <s-section>
                    {match status {
                        AuthStatus::Checking => view! { <s-paragraph>"Loading..."</s-paragraph> }.into_any(),
                        AuthStatus::Failed => {
                            view! {
                                <s-banner tone="critical" heading="The app could not start">
                                    {failure.get()}
                                </s-banner>
                            }
                                .into_any()
                        }
                        _ => {
                            view! {
                                <s-stack gap="base">
                                    <s-heading>"Open this app from Shopify admin"</s-heading>
                                    <s-paragraph>
                                        "This app works inside Shopify admin. Open it from the Apps section of your store."
                                    </s-paragraph>
                                </s-stack>
                            }
                                .into_any()
                        }
                    }}
                </s-section>
            </s-page>
        }
        .into_any(),
    }
    }
}

struct ComposedPathEntry(JsValue);

impl NavigateTarget for ComposedPathEntry {
    fn matches(&self, selector: &str) -> bool {
        let is_function = |name: &str| dom::member(&self.0, name).is_some_and(|member| member.is_function());
        self.0.is_object()
            && is_function("matches")
            && is_function("getAttribute")
            && self.0.unchecked_ref::<Element>().matches(selector).unwrap_or(false)
    }

    fn attribute(&self, name: &str) -> Option<String> {
        self.0.unchecked_ref::<Element>().get_attribute(name)
    }
}

struct ShopifyNavigateEvent(Event);

impl NavigateEvent for ShopifyNavigateEvent {
    type Target = ComposedPathEntry;

    fn composed_path(&self) -> Vec<ComposedPathEntry> {
        self.0.composed_path().iter().map(ComposedPathEntry).collect()
    }

    fn prevent_default(&self) {
        self.0.prevent_default();
    }
}

/// Routes App Bridge navigations (sidebar links, `s-link`, `s-button href`) client-side.
#[component]
fn ShopifyNavigateBridge() -> impl IntoView {
    let navigate = use_navigate();
    router::install(move |path| {
        navigate(
            path,
            NavigateOptions {
                resolve: false,
                ..NavigateOptions::default()
            },
        )
    });

    // Custom pages are gated outside the router, so the address bar must not wait for a route view.
    let pathname = use_location().pathname;
    let browser_url = use_context::<BrowserUrl>();
    Effect::new(move |_| {
        pathname.track();
        if let Some(browser_url) = &browser_url {
            browser_url.ready_to_complete();
        }
    });

    let custom_patterns = expect_context::<CustomAdminPages>().route_patterns();
    hold_until_cleanup(listen(&document(), "shopify:navigate", move |event| {
        navigation::handle_shopify_navigate(
            &ShopifyNavigateEvent(event),
            router::route,
            &dom::location_origin(),
            &custom_patterns,
        );
    }));
}

/// `/`: the page the application declares there, else the built-in dashboard.
#[component]
fn HomeRoute() -> impl IntoView {
    let custom = use_custom_home();
    move || {
        if custom.get() {
            view! { <CustomAdminPageOutlet /> }.into_any()
        } else {
            view! { <Home /> }.into_any()
        }
    }
}

#[component]
fn Root(custom_pages: CustomAdminPages) -> impl IntoView {
    provide_context(custom_pages);
    view! {
        <Router>
            <ShopifyNavigateBridge />
            <AuthGate>
                <NavMenu />
                <AdminPageGate>
                    <CustomAdminPageGate>
                        <FlatRoutes fallback=|| view! { <CustomAdminPageOutlet /> }>
                            <Route path=path!("/") view=HomeRoute />
                            <Route path=path!("/flows/:name?") view=Flows />
                            <Route path=path!("/runs/:id") view=FlowRun />
                            <Route path=path!("/crons") view=Crons />
                            <Route path=path!("/providers") view=Providers />
                            <Route path=path!("/mcp") view=Mcp />
                            <Route path=path!("/functions/:handle?/:action?") view=Functions />
                        </FlatRoutes>
                    </CustomAdminPageGate>
                </AdminPageGate>
            </AuthGate>
        </Router>
    }
}
