//! The admin rendered by the server, page by page (`UiSource::Server`):
//! HTML made of Shopify's Polaris web components and one hand-written script
//! (`openshop.js`). See `docs/src/reference/custom-admin-pages.md`.
//!
//! A page is a URL. A request for it is authenticated with a session token:
//! the `Authorization` header that App Bridge's `fetch` adds to the requests
//! of the script, or the `id_token` of the URL Shopify opens the app with. A
//! document request without a valid one is sent to the bounce page, where
//! App Bridge gets a fresh token and loads the page again with it.
//!
//! A form posts to the URL of its page, the name of its action in `_action`.
//! The script's request is answered with the page rendered again (or
//! `X-OpenShop-Location`) and the toast the action asked for, in a header; a
//! plain post with a redirect to the page. A form the page refuses, field by
//! field, is rendered again in place (`422`), with what was sent and the
//! error of each field, when the view reads them.

mod crons;
mod flow_run;
mod flows;
mod functions;
mod home;
mod mcp;
mod providers;
mod shell;
mod ui;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use indexmap::IndexMap;
use maud::{Markup, html};
use serde_json::Value;
use uuid::Uuid;

use super::ServerOptions;
use super::admin_pages::{call_function, create_context, page_is_allowed};
use super::api::pages::resolved_admin_pages;
use super::http::{catch_panic, header, not_found, parse_query, report_failure};
use super::jwt::read_jwt_audience;
use super::launch::{forget_plan_on_welcome, is_installed_for_launch, is_ui_shell_path, open_from_shopify_admin};
use super::reload::Served;
use super::shop::{ShopSession, session_of};
use super::shop_domain::normalize_shop_domain;
use crate::admin::{
    ACTION_FIELD, AdminFunctionKind, AdminPublicError, CustomAdminPage, FormState, Outcome, PageRequest,
    fields_to_json, parse_fields, with_outcome,
};
use crate::app::OpenShop;
use crate::engine::step::error_message;
use crate::runtime::{logger, report};
use crate::types::{AdminPageMode, JsonMap};
use openshop_shared::rpc::encode_uri_component as encode;
use openshop_shared::{ADMIN_HOME_PATH, AdminPageId, custom_admin_route_params, match_custom_admin_path};

pub(crate) use shell::{SCRIPT_PATH, script};

/// Where App Bridge gets a fresh session token for a page loaded without one.
pub(crate) const BOUNCE_PATH: &str = "/auth/session-token";
/// The request header of the script: its requests get headers, not redirects.
const FETCH_HEADER: &str = "x-openshop-fetch";
const TOAST_HEADER: &str = "x-openshop-toast";
const TOAST_ERROR_HEADER: &str = "x-openshop-toast-error";
const LOCATION_HEADER: &str = "x-openshop-location";
/// What App Bridge's `fetch` answers by asking again with a fresh token.
const RETRY_HEADER: &str = "x-shopify-retry-invalid-session-request";
/// What a form may send: its fields, not files.
const MAX_FORM_BYTES: usize = 256 * 1024;

/// A request of the page, as the script or the browser sent it.
struct PageCall<'a> {
    app: &'a OpenShop,
    /// Development: the page reloads when the server starts again.
    reload: Option<&'a Served>,
    session: ShopSession,
    pathname: String,
    query: String,
    /// Sent by `openshop.js`.
    fetch: bool,
}

/// The page at `pathname`: a page of the application, or one of the framework.
enum Target<'a> {
    Custom(&'a CustomAdminPage),
    Builtin(AdminPageId),
    Home,
}

fn target<'a>(app: &'a OpenShop, pathname: &str) -> Option<Target<'a>> {
    let custom = app.config().custom_pages_enabled().then(|| {
        app.custom_pages()
            .iter()
            .find(|page| match_custom_admin_path(&page.route_pattern, pathname))
    });
    if let Some(Some(page)) = custom {
        return Some(Target::Custom(page));
    }
    if pathname == ADMIN_HOME_PATH {
        return Some(Target::Home);
    }
    openshop_shared::admin_page_from_ui_path(pathname).map(Target::Builtin)
}

pub(crate) async fn serve(
    app: OpenShop,
    options: ServerOptions,
    reload: Option<Arc<Served>>,
    request: Request,
) -> Response {
    let pathname = request.uri().path().to_string();
    if !is_ui_shell_path(&pathname) || !matches!(*request.method(), Method::GET | Method::HEAD | Method::POST) {
        return not_found();
    }
    let raw_query = request.uri().query().unwrap_or_default().to_string();
    let query = parse_query(Some(&raw_query));
    let headers = request.headers().clone();
    let fetch = header(&headers, FETCH_HEADER).is_some();
    let endpoints = app.endpoints();

    // A launch Shopify signed: the shop is installed first.
    if !fetch
        && query.contains_key("hmac")
        && let Ok((shopify_app, signed)) = app.apps().by_signed_query(&raw_query)
        && let Some(shop) = query.get("shop").and_then(|shop| normalize_shop_domain(shop))
    {
        match is_installed_for_launch(&app, shopify_app, &shop, &signed).await {
            Ok(true) => forget_plan_on_welcome(&app, &shopify_app.handle, &signed).await,
            Ok(false) => {
                return redirect(&format!(
                    "/auth?shop={}&app={}",
                    encode(&shop),
                    encode(&shopify_app.handle)
                ));
            }
            Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response(),
        }
    }

    let token = header(&headers, AUTHORIZATION.as_str())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| query.get("id_token").map(String::as_str))
        .filter(|token| !token.is_empty());
    let session = match token {
        Some(token) => session_of(&app, token, &pathname).await.ok(),
        None => None,
    };
    let Some(session) = session else {
        if fetch {
            // App Bridge asks again with a fresh token.
            return (StatusCode::UNAUTHORIZED, [(RETRY_HEADER, "1")]).into_response();
        }
        let embedded = query.contains_key("shop")
            || query.contains_key("embedded")
            || header(&headers, "sec-fetch-dest") == Some("iframe");
        return if embedded && request.method() != Method::POST {
            bounce(&app, &pathname, &raw_query, token)
        } else if embedded {
            // A plain post without a token: its page, which gets one.
            see_other(&page_url(&pathname, &raw_query))
        } else {
            open_from_shopify_admin(StatusCode::UNAUTHORIZED, endpoints)
        };
    };

    let shop = session.shop.clone();
    let call = PageCall {
        app: &app,
        reload: reload.as_deref(),
        session,
        pathname: pathname.clone(),
        query: raw_query.clone(),
        fetch,
    };
    let is_post = request.method() == Method::POST;
    let response = if is_post {
        let body = to_bytes(request.into_body(), MAX_FORM_BYTES).await.unwrap_or_default();
        submit(&call, &body).await
    } else {
        render(&call, StatusCode::OK, None, &FormState::default(), None).await
    };
    finish(response, &app, options.dev, &shop)
}

/// The response with the headers of every page: framed by the admin of its
/// shop only (any shop in development), never stored, never sniffed.
fn finish(mut response: Response, app: &OpenShop, dev: bool, shop: &str) -> Response {
    let mut ancestors = if dev {
        "https://*.myshopify.com https://admin.shopify.com http://localhost:*".to_string()
    } else {
        format!("https://{shop} https://admin.shopify.com")
    };
    if let Some(origin) = &app.endpoints().admin_origin {
        ancestors.push_str(&format!(" {origin}"));
    }
    let headers = response.headers_mut();
    if let Ok(policy) = HeaderValue::from_str(&format!("frame-ancestors {ancestors};")) {
        headers.insert("content-security-policy", policy);
    }
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    response
}

/// A path of this server: `/reviews/42`, not `//host` nor `/\\host`, which
/// browsers read as another host.
fn is_local_path(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//") && !path.contains('\\')
}

fn redirect(location: &str) -> Response {
    (StatusCode::FOUND, [(LOCATION, location)]).into_response()
}

fn see_other(location: &str) -> Response {
    (StatusCode::SEE_OTHER, [(LOCATION, location)]).into_response()
}

fn page_url(pathname: &str, query: &str) -> String {
    if query.is_empty() {
        pathname.to_string()
    } else {
        format!("{pathname}?{query}")
    }
}

/// The query of the URL without its `id_token`: App Bridge adds a fresh one.
fn without_token(query: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(
            parse_fields(query.as_bytes())
                .iter()
                .filter(|(name, _)| name != "id_token"),
        )
        .finish()
}

/// Sends a document request without a valid session token to the bounce
/// page, which loads it again with a fresh one: the parameters of the
/// request, and `shopify-reload`, the URL to load. `app` names the app whose
/// App Bridge gets the token, from the expired token when there is one.
fn bounce(app: &OpenShop, pathname: &str, query: &str, token: Option<&str>) -> Response {
    let query = without_token(query);
    let handle = token
        .and_then(read_jwt_audience)
        .and_then(|audience| app.apps().by_api_key(&audience).ok())
        .map(|shopify_app| shopify_app.handle.clone());
    let mut target = url::form_urlencoded::Serializer::new(String::new());
    target.extend_pairs(parse_fields(query.as_bytes()));
    if let Some(handle) = &handle {
        target.append_pair("app", handle);
    }
    target.append_pair("shopify-reload", &page_url(pathname, &query));
    redirect(&format!("{BOUNCE_PATH}?{}", target.finish()))
}

/// `GET /auth/session-token`: the bounce page, App Bridge alone. It sees
/// `shopify-reload` and loads that URL with a fresh `id_token`.
pub(crate) async fn bounce_page(State(app): State<OpenShop>, request: Request) -> Response {
    let query = parse_query(request.uri().query());
    let reload = query.get("shopify-reload").map(String::as_str).unwrap_or_default();
    if !is_local_path(reload) {
        return not_found();
    }
    let shopify_app = app
        .apps()
        .by_handle(query.get("app").map(String::as_str))
        .ok()
        .or_else(|| app.apps().all().first());
    let api_key = shopify_app.map_or("", |shopify_app| shopify_app.api_key.as_str());
    let markup = shell::bounce(api_key, app.endpoints());
    (
        [
            (CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        markup.into_string(),
    )
        .into_response()
}

/// The page, rendered for the session. `notice` is a failure to show above
/// it, for a plain post the action refused; `form`, the form the page
/// refused, which its view shows again; `revealed`, what an action of a page
/// of the framework gave it to show once.
async fn render(
    call: &PageCall<'_>,
    status: StatusCode,
    notice: Option<&str>,
    form: &FormState,
    revealed: Option<Value>,
) -> Response {
    let app = call.app;
    let content = match target(app, &call.pathname) {
        None => Err(StatusCode::NOT_FOUND),
        Some(Target::Custom(page)) => render_custom(call, page, form).await,
        Some(Target::Home) => render_builtin(call, None, form, revealed).await,
        Some(Target::Builtin(page)) => {
            if resolved_admin_pages(app.config()).get(page) == AdminPageMode::Disabled {
                Err(StatusCode::NOT_FOUND)
            } else {
                render_builtin(call, Some(page), form, revealed).await
            }
        }
    };
    let (status, page) = match content {
        Ok(Shown::Page(page)) => (status, page),
        Ok(Shown::Refused(refused, page)) => (refused, page),
        Err(StatusCode::FORBIDDEN | StatusCode::NOT_FOUND) => (StatusCode::NOT_FOUND, disabled()),
        Err(failed) => (failed, unavailable()),
    };
    let menu = shell::Menu::of(app, &call.session).await;
    let document = shell::document(app, &call.session, &menu, notice, page, call.reload);
    (
        status,
        [(CONTENT_TYPE, "text/html; charset=utf-8")],
        document.into_string(),
    )
        .into_response()
}

/// What a page shows: itself, or what refused it, with its status.
enum Shown {
    Page(Markup),
    Refused(StatusCode, Markup),
}

/// A failure of a page the merchant may read, `AdminPublicError`, as the page
/// shows it; any other one is reported and the page is unavailable. A plan
/// the page needs (`billing::require_plan`) links to the plans of the app.
fn shown_failure(error: &anyhow::Error, failure: &str) -> Result<Shown, StatusCode> {
    match error.downcast_ref::<AdminPublicError>() {
        Some(public) => Ok(Shown::Refused(
            StatusCode::from_u16(public.status).unwrap_or(StatusCode::BAD_REQUEST),
            html! {
                s-page {
                    @match public.details.as_ref().and_then(|details| details.get("pricingLink")) {
                        Some(plans) => {
                            s-banner tone="warning" heading=(public.message) {
                                s-button variant="primary" href=(plans) target="_top" { "See the plans" }
                            }
                        }
                        None => {
                            s-banner tone="critical" heading=(public.message) {}
                        }
                    }
                }
            },
        )),
        None => {
            report_failure(failure, error);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// A request for a page of the framework: what its `render` and its
/// `submit` are given.
pub(super) struct BuiltinPage<'a> {
    pub(super) app: &'a OpenShop,
    pub(super) session: &'a ShopSession,
    /// The path of the page: `/flows/syncOrders`, `/runs/42`.
    pub(super) pathname: &'a str,
    /// The query string of its URL, without `?`.
    pub(super) query: &'a str,
    /// The form the page refused, which it shows again; empty otherwise.
    pub(super) form: &'a FormState,
    /// What an action gives the page to show once (a secret made for the
    /// merchant): set by `submit` with [`BuiltinPage::reveal`], read by
    /// `render` with [`BuiltinPage::revealed`].
    revealed: Mutex<Option<Value>>,
}

impl BuiltinPage<'_> {
    /// Called by `submit`: the page is rendered again with `value`, which
    /// `render` reads with [`BuiltinPage::revealed`], in place of a redirect
    /// (a plain post too, which would lose it). A secret shown once.
    pub(super) fn reveal(&self, value: Value) {
        *crate::locked(&self.revealed) = Some(value);
    }

    /// What the action that ran gave the page to show once, if one did.
    pub(super) fn revealed(&self) -> Option<Value> {
        crate::locked(&self.revealed).clone()
    }

    /// A parameter of the query string, `?flow=syncOrders`: the first one of that name.
    pub(super) fn param(&self, name: &str) -> Option<String> {
        parse_fields(self.query.as_bytes())
            .into_iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value)
    }
}

/// The value of the field `name` of a form, the first one of that name.
pub(super) fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value.as_str())
}

/// Whether `pathname` is the page of a run, `/runs/42`, part of the flows.
fn is_run_path(pathname: &str) -> bool {
    pathname.starts_with("/runs/")
}

/// A page of the framework: the home of the admin (`None`) or one of the pages it lists.
async fn render_builtin(
    call: &PageCall<'_>,
    id: Option<AdminPageId>,
    form: &FormState,
    revealed: Option<Value>,
) -> Result<Shown, StatusCode> {
    let page = BuiltinPage {
        app: call.app,
        session: &call.session,
        pathname: &call.pathname,
        query: &call.query,
        form,
        revealed: Mutex::new(revealed),
    };
    report::describe(|description| {
        description.unit("admin", page.pathname);
    });
    let rendered = catch_panic("admin page", async {
        match id {
            None => home::render(&page).await,
            Some(AdminPageId::Crons) => crons::render(&page).await,
            Some(AdminPageId::Providers) => providers::render(&page).await,
            Some(AdminPageId::Flows) if is_run_path(page.pathname) => flow_run::render(&page).await,
            Some(AdminPageId::Flows) => flows::render(&page).await,
            Some(AdminPageId::Functions) => functions::render(&page).await,
            Some(AdminPageId::Mcp) => mcp::render(&page).await,
        }
    })
    .await;
    match rendered {
        Ok(markup) => Ok(Shown::Page(markup)),
        Err(error) => shown_failure(&error, "[openshop] A page of the admin could not be rendered"),
    }
}

/// A form of a page of the framework: the action it names, and what it gave
/// the page to show once.
async fn submit_builtin(
    call: &PageCall<'_>,
    id: Option<AdminPageId>,
    action: &str,
    fields: &[(String, String)],
) -> (anyhow::Result<Outcome>, Option<Value>) {
    let form = FormState::default();
    let page = BuiltinPage {
        app: call.app,
        session: &call.session,
        pathname: &call.pathname,
        query: &call.query,
        form: &form,
        revealed: Mutex::new(None),
    };
    report::describe(|description| {
        description.unit("admin", format_args!("{} {action}", page.pathname));
    });
    let outcome = catch_panic("admin page action", async {
        match id {
            None => home::submit(&page, action, fields).await,
            Some(AdminPageId::Crons) => crons::submit(&page, action, fields).await,
            Some(AdminPageId::Providers) => providers::submit(&page, action, fields).await,
            Some(AdminPageId::Flows) if is_run_path(page.pathname) => flow_run::submit(&page, action, fields).await,
            Some(AdminPageId::Flows) => flows::submit(&page, action, fields).await,
            Some(AdminPageId::Functions) => functions::submit(&page, action, fields).await,
            Some(AdminPageId::Mcp) => mcp::submit(&page, action, fields).await,
        }
    })
    .await;
    (
        outcome,
        page.revealed
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

/// The answer of a page to an action it does not have.
pub(super) fn no_action() -> anyhow::Error {
    AdminPublicError::new("NOT_FOUND", "Not found").status(404).into()
}

async fn render_custom(call: &PageCall<'_>, page: &CustomAdminPage, form: &FormState) -> Result<Shown, StatusCode> {
    let app = call.app;
    if !page_is_allowed(app, &call.session, page, &call.pathname).await {
        return Err(StatusCode::FORBIDDEN);
    }
    let Some(view) = &page.page.view else {
        return Ok(Shown::Page(without_view(&page.route_pattern)));
    };
    let params = custom_admin_route_params(&page.route_pattern, &call.pathname).ok_or(StatusCode::NOT_FOUND)?;
    let request_id = Uuid::new_v4().to_string();
    // What a failure of the view is reported as: the page and the request.
    report::describe(|description| {
        description
            .unit("admin", format_args!("{} view", page.route_pattern))
            .tag("request_id", &request_id);
    });
    let ctx = create_context(app, &call.session, params, request_id, None);
    let request = PageRequest {
        ctx,
        query: call.query.clone(),
        form: form.clone(),
    };
    match catch_panic("admin view", view(request)).await {
        Ok(markup) => Ok(Shown::Page(markup)),
        Err(error) => shown_failure(&error, "[openshop] Custom admin page could not be rendered"),
    }
}

/// A form of the page: the action it names, then what the page shows after it.
async fn submit(call: &PageCall<'_>, body: &[u8]) -> Response {
    let fields = parse_fields(body);
    let action = fields
        .iter()
        .find(|(name, _)| name == ACTION_FIELD)
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let target = target(call.app, &call.pathname);
    let (outcome, revealed) = match &target {
        Some(Target::Custom(page)) => (run_action(call, page, &action, &fields).await, None),
        Some(Target::Home) => submit_builtin(call, None, &action, &fields).await,
        Some(Target::Builtin(page))
            if resolved_admin_pages(call.app.config()).get(*page) != AdminPageMode::Disabled =>
        {
            submit_builtin(call, Some(*page), &action, &fields).await
        }
        _ => (Err(no_action()), None),
    };
    // The view shows a refused form itself: every page of the framework, and a
    // view of the application that reads its `form`.
    let reads_form = match &target {
        Some(Target::Custom(page)) => page.page.view_reads_form,
        Some(_) => true,
        None => false,
    };
    let refused = |errors: IndexMap<String, String>, message: Option<String>| FormState {
        action: Some(action.clone()),
        values: fields.clone(),
        errors,
        message,
    };
    match outcome {
        Ok(outcome) if !outcome.field_errors.is_empty() => {
            let form = refused(outcome.field_errors.clone(), outcome.toast.clone());
            let message = outcome.toast.clone().unwrap_or_else(|| first_error(&form.errors));
            refuse(
                call,
                reads_form,
                &form,
                StatusCode::UNPROCESSABLE_ENTITY,
                &message,
                outcome.toast.as_deref(),
            )
            .await
        }
        Ok(outcome) => answer(call, outcome, revealed).await,
        Err(error) => {
            let (status, message, errors) = match error.downcast_ref::<AdminPublicError>() {
                Some(public) => (
                    StatusCode::from_u16(public.status).unwrap_or(StatusCode::BAD_REQUEST),
                    public.message.clone(),
                    public.field_errors.clone().unwrap_or_default(),
                ),
                None => {
                    report_failure("[openshop] Admin page action failed", &error);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal server error".to_string(),
                        IndexMap::new(),
                    )
                }
            };
            if errors.is_empty() {
                return fail(call, status, &message).await;
            }
            // The schema's summary is for the fields; the action's message is the action's to say.
            let said = (error
                .downcast_ref::<AdminPublicError>()
                .map(|public| public.code.as_str())
                != Some("INVALID_INPUT"))
            .then(|| message.clone());
            let form = refused(errors, said);
            refuse(call, reads_form, &form, status, &message, None).await
        }
    }
}

/// The first error of a refused form, `title: Required`: what a toast says
/// when the view does not show the fields.
fn first_error(errors: &IndexMap<String, String>) -> String {
    errors
        .first()
        .map(|(field, error)| format!("{field}: {error}"))
        .unwrap_or_default()
}

/// A failure of the form: the script keeps the page as it is, with an error
/// toast; a plain post gets the page with the message above it.
async fn fail(call: &PageCall<'_>, status: StatusCode, message: &str) -> Response {
    if call.fetch {
        (status, [(TOAST_ERROR_HEADER, encode(message))]).into_response()
    } else {
        render(call, status, Some(message), &FormState::default(), None).await
    }
}

/// A form refused field by field: rendered again in place, with what was
/// sent and the errors, when the view shows them (`toast`, an error toast the
/// action asked for); a failure of the form otherwise.
async fn refuse(
    call: &PageCall<'_>,
    reads_form: bool,
    form: &FormState,
    status: StatusCode,
    message: &str,
    toast: Option<&str>,
) -> Response {
    if !reads_form {
        return fail(call, status, message).await;
    }
    let mut response = render(call, status, None, form, None).await;
    if let Some(toast) = toast.filter(|_| call.fetch)
        && let Ok(value) = HeaderValue::from_str(&encode(toast))
    {
        response.headers_mut().insert(TOAST_ERROR_HEADER, value);
    }
    response
}

/// What a form gets after its action: the page again, or the page the
/// action sent it to, with its toast. A page given something to show once is
/// rendered again with it, never redirected.
async fn answer(call: &PageCall<'_>, outcome: Outcome, revealed: Option<Value>) -> Response {
    if revealed.is_some() {
        let mut response = render(call, StatusCode::OK, None, &FormState::default(), revealed).await;
        if call.fetch
            && let Some(toast) = &outcome.toast
            && let Ok(value) = HeaderValue::from_str(&encode(toast))
        {
            let name = if outcome.error {
                TOAST_ERROR_HEADER
            } else {
                TOAST_HEADER
            };
            response.headers_mut().insert(name, value);
        }
        return response;
    }
    let next = outcome.redirect.as_deref().filter(|path| is_local_path(path));
    if !call.fetch {
        return see_other(next.unwrap_or(&page_url(&call.pathname, &call.query)));
    }
    let mut response = match next {
        Some(next) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            if let Ok(value) = HeaderValue::from_str(next) {
                response.headers_mut().insert(LOCATION_HEADER, value);
            }
            response
        }
        None => render(call, StatusCode::OK, None, &FormState::default(), None).await,
    };
    if let Some(toast) = &outcome.toast {
        let name = if outcome.error {
            TOAST_ERROR_HEADER
        } else {
            TOAST_HEADER
        };
        if let Ok(value) = HeaderValue::from_str(&encode(toast)) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

/// Runs the action `name` of a page with the fields of its form, read as its
/// input schema describes them, through the access rules of the page and of
/// the function, as an RPC call is. One log line, as for an RPC call.
async fn run_action(
    call: &PageCall<'_>,
    page: &CustomAdminPage,
    name: &str,
    fields: &[(String, String)],
) -> anyhow::Result<Outcome> {
    let not_found = || AdminPublicError::new("NOT_FOUND", "Not found").status(404);
    let function = page
        .page
        .functions
        .get(name)
        .filter(|function| function.kind == AdminFunctionKind::Action)
        .ok_or_else(not_found)?;
    let params = custom_admin_route_params(&page.route_pattern, &call.pathname).ok_or_else(not_found)?;
    let request_id = Uuid::new_v4().to_string();
    // What a failure of the action is reported as: the page, the action and the request.
    report::describe(|description| {
        description
            .unit("admin", format_args!("{} {name}", page.route_pattern))
            .tag("request_id", &request_id);
    });
    let schema = function
        .input
        .as_ref()
        .map_or(Value::Null, |input| input.json_schema.clone());
    let input = fields_to_json(&schema, fields);
    let context = create_context(call.app, &call.session, params, request_id.clone(), None);
    let started = Instant::now();
    let (result, outcome) = with_outcome(catch_panic(
        "admin function",
        call_function(call.app.custom_pages(), page, function, context, input),
    ))
    .await;
    let mut metadata = JsonMap::new();
    for (key, value) in [
        ("requestId", request_id.as_str()),
        ("shop", call.session.shop.as_str()),
        ("shopifyApp", call.session.shopify_app.as_str()),
        ("actorId", call.session.actor.id.as_str()),
        ("pageId", page.id.as_str()),
        ("functionName", name),
        ("kind", "action"),
    ] {
        metadata.insert(key.into(), Value::from(value));
    }
    metadata.insert("form".into(), Value::Bool(true));
    metadata.insert("durationMs".into(), Value::from(started.elapsed().as_millis() as u64));
    match &result {
        Ok(_) => logger::info_with("[openshop] Custom admin function completed", Value::Object(metadata)),
        Err(error) => {
            metadata.insert("error".into(), Value::from(error_message(error)));
            logger::warn_with("[openshop] Custom admin function rejected", Value::Object(metadata));
        }
    }
    result.map(|_| outcome.unwrap_or_default())
}

/// A page of the application without a view: its functions, and nothing to show.
fn without_view(name: &str) -> Markup {
    html! {
        s-page heading=(name) {
            s-banner tone="info" heading="This page has nothing to show" {
                s-paragraph { "It declares no view." }
            }
        }
    }
}

fn disabled() -> Markup {
    html! {
        s-page {
            s-section { s-paragraph { "This page is disabled." } }
        }
    }
}

fn unavailable() -> Markup {
    html! {
        s-page heading="Page unavailable" {
            s-banner tone="critical" { "This page could not be displayed." }
        }
    }
}
