//! The admin rendered by the server, page by page (`UiSource::Server`):
//! HTML made of Shopify's Polaris web components, one hand-written script
//! (`openshop.js`), no bundle and no WebAssembly. Experimental, next to the
//! Leptos admin UI; see `docs/src/reference/server-rendered-ui.md`.
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
//! plain post with a redirect to the page.

mod crons;
mod shell;

use std::sync::Arc;
use std::time::Instant;

use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde_json::Value;
use uuid::Uuid;

use super::ServerOptions;
use super::admin_pages::{call_function, create_context, page_is_allowed};
use super::api::pages::resolved_admin_pages;
use super::http::{catch_panic, header, not_found, parse_query, report_failure};
use super::jwt::read_jwt_audience;
use super::reload::Served;
use super::shop::{ShopSession, session_of};
use super::shop_domain::normalize_shop_domain;
use super::static_ui::{forget_plan_on_welcome, is_installed_for_launch, is_ui_shell_path, open_from_shopify_admin};
use crate::admin::{
    ACTION_FIELD, AdminFunctionKind, AdminPublicError, CustomAdminPage, Outcome, PageRequest, fields_to_json,
    parse_fields, with_outcome,
};
use crate::app::OpenShop;
use crate::engine::step::error_message;
use crate::runtime::logger;
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

pub(crate) async fn serve(app: OpenShop, options: ServerOptions, reload: Option<Arc<Served>>, request: Request) -> Response {
    let pathname = request.uri().path().to_string();
    if !is_ui_shell_path(&pathname) || !matches!(*request.method(), Method::GET | Method::HEAD | Method::POST) {
        return not_found();
    }
    let raw_query = request.uri().query().unwrap_or_default().to_string();
    let query = parse_query(Some(&raw_query));
    let headers = request.headers().clone();
    let fetch = header(&headers, FETCH_HEADER).is_some();
    let endpoints = app.endpoints();

    // A launch Shopify signed: the shop is installed first, as for the Leptos admin.
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
        let embedded = options.dev
            || query.contains_key("shop")
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
        render(&call, StatusCode::OK, None).await
    };
    finish(response, &app, options.dev)
}

/// The response with the headers of every page: framed by the admin only,
/// never stored, never sniffed.
fn finish(mut response: Response, app: &OpenShop, dev: bool) -> Response {
    let mut ancestors = "https://*.myshopify.com https://admin.shopify.com".to_string();
    if dev {
        ancestors.push_str(" http://localhost:*");
    }
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

fn redirect(location: &str) -> Response {
    (StatusCode::FOUND, [(LOCATION, location)]).into_response()
}

fn see_other(location: &str) -> Response {
    (StatusCode::SEE_OTHER, [(LOCATION, location)]).into_response()
}

fn page_url(pathname: &str, query: &str) -> String {
    if query.is_empty() { pathname.to_string() } else { format!("{pathname}?{query}") }
}

/// The query of the URL without its `id_token`: App Bridge adds a fresh one.
fn without_token(query: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(parse_fields(query.as_bytes()).iter().filter(|(name, _)| name != "id_token"))
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
    if !reload.starts_with('/') || reload.starts_with("//") {
        return not_found();
    }
    let shopify_app = app.apps().by_handle(query.get("app").map(String::as_str)).ok().or_else(|| app.apps().all().first());
    let api_key = shopify_app.map_or("", |shopify_app| shopify_app.api_key.as_str());
    let markup = shell::bounce(api_key, app.endpoints());
    (
        [(CONTENT_TYPE, "text/html; charset=utf-8"), (axum::http::header::CACHE_CONTROL, "no-store")],
        markup.into_string(),
    )
        .into_response()
}

/// The page, rendered for the session. `notice` is a failure to show above
/// it, for a plain post the action refused.
async fn render(call: &PageCall<'_>, status: StatusCode, notice: Option<&str>) -> Response {
    let app = call.app;
    let content = match target(app, &call.pathname) {
        None => Err(StatusCode::NOT_FOUND),
        Some(Target::Custom(page)) => render_custom(call, page).await,
        Some(Target::Home) => Ok(not_ported("Home")),
        Some(Target::Builtin(page)) => {
            if resolved_admin_pages(app.config()).get(page) == AdminPageMode::Disabled {
                Err(StatusCode::NOT_FOUND)
            } else {
                match page {
                    AdminPageId::Crons => crons::render(app, &call.session).await.map_err(|error| {
                        report_failure("[openshop] The crons page could not be rendered", &error);
                        StatusCode::INTERNAL_SERVER_ERROR
                    }),
                    other => Ok(not_ported(builtin_label(other))),
                }
            }
        }
    };
    let (status, page) = match content {
        Ok(page) => (status, page),
        Err(StatusCode::FORBIDDEN | StatusCode::NOT_FOUND) => (StatusCode::NOT_FOUND, disabled()),
        Err(failed) => (failed, unavailable()),
    };
    let menu = shell::Menu::of(app, &call.session).await;
    let document = shell::document(app, &call.session, &menu, notice, page, call.reload);
    (status, [(CONTENT_TYPE, "text/html; charset=utf-8")], document.into_string()).into_response()
}

async fn render_custom(call: &PageCall<'_>, page: &CustomAdminPage) -> Result<Markup, StatusCode> {
    let app = call.app;
    if !page_is_allowed(app, &call.session, page, &call.pathname).await {
        return Err(StatusCode::FORBIDDEN);
    }
    let Some(view) = &page.page.view else {
        return Ok(not_ported(&page.route_pattern));
    };
    let params = custom_admin_route_params(&page.route_pattern, &call.pathname).ok_or(StatusCode::NOT_FOUND)?;
    let ctx = create_context(app, &call.session, params, Uuid::new_v4().to_string(), None);
    let request = PageRequest {
        ctx,
        query: call.query.clone(),
    };
    match catch_panic("admin view", view(request)).await {
        Ok(markup) => Ok(markup),
        Err(error) => match error.downcast_ref::<AdminPublicError>() {
            Some(public) => Ok(html! {
                s-page {
                    s-banner tone="critical" heading=(public.message) {}
                }
            }),
            None => {
                report_failure("[openshop] Custom admin page could not be rendered", &error);
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        },
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
    let outcome = match target(call.app, &call.pathname) {
        Some(Target::Custom(page)) => run_action(call, page, &action, &fields).await,
        Some(Target::Builtin(AdminPageId::Crons))
            if resolved_admin_pages(call.app.config()).get(AdminPageId::Crons) != AdminPageMode::Disabled =>
        {
            crons::submit(call.app, &call.session, &action, &fields).await
        }
        _ => Err(AdminPublicError::new("NOT_FOUND", "Not found").status(404).into()),
    };
    match outcome {
        Ok(outcome) => answer(call, outcome).await,
        Err(error) => {
            let (status, message) = match error.downcast_ref::<AdminPublicError>() {
                Some(public) => (
                    StatusCode::from_u16(public.status).unwrap_or(StatusCode::BAD_REQUEST),
                    public.message.clone(),
                ),
                None => {
                    report_failure("[openshop] Admin page action failed", &error);
                    (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string())
                }
            };
            if call.fetch {
                // The page stays as it is, with an error toast.
                (status, [(TOAST_ERROR_HEADER, encode(&message))]).into_response()
            } else {
                render(call, status, Some(&message)).await
            }
        }
    }
}

/// What a form gets after its action: the page again, or the page the
/// action sent it to, with its toast.
async fn answer(call: &PageCall<'_>, outcome: Outcome) -> Response {
    let next = outcome
        .redirect
        .as_deref()
        .filter(|path| path.starts_with('/') && !path.starts_with("//"));
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
        None => render(call, StatusCode::OK, None).await,
    };
    if let Some(toast) = &outcome.toast {
        let name = if outcome.error { TOAST_ERROR_HEADER } else { TOAST_HEADER };
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
    let schema = function.input.as_ref().map_or(Value::Null, |input| input.json_schema.clone());
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

fn builtin_label(page: AdminPageId) -> &'static str {
    match page {
        AdminPageId::Flows => "Flows",
        AdminPageId::Providers => "Providers",
        AdminPageId::Crons => "Crons",
        AdminPageId::Mcp => "MCP",
        AdminPageId::Functions => "Functions",
    }
}

/// A page of the framework that the server does not render yet.
fn not_ported(name: &str) -> Markup {
    html! {
        s-page heading=(name) {
            s-banner tone="info" heading="Not rendered by the server yet" {
                s-paragraph { "This page of the admin is still part of the WebAssembly admin UI." }
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
