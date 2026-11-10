//! The operator dashboard, under `/ops`: what happens in every shop, read
//! from outside Shopify admin and behind a password.
//!
//! It is served when the application sets `ops_password`. Its pages are HTML
//! rendered by the server, without script: a menu on the side, charts drawn
//! as SVG, and dense tables over a period. They only read: nothing a visitor
//! does there changes a shop.

mod auth;
mod chart;
mod html;
mod pages;
mod queries;

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Extension, Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, LOCATION, REFERRER_POLICY, RETRY_AFTER, SET_COOKIE,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use serde_json::json;

use self::auth::{Login, OpsAuth, is_same_site};
use self::html::{login_page, message_page};
use crate::app::OpenShop;
use crate::runtime::logger;
use crate::server::client::client_ip;
use crate::server::linger::Peer;

pub use self::auth::check_password;

const HOME: &str = "/ops";
const LOGIN: &str = "/ops/login";
/// No script, no frame, nothing loaded from elsewhere, and forms that stay on the server.
const CONTENT_SECURITY_POLICY_VALUE: &str =
    "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";

fn html_response(status: StatusCode, html: String) -> Response {
    (status, [(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

fn see_other(location: &'static str) -> Response {
    (StatusCode::SEE_OTHER, [(LOCATION, location)]).into_response()
}

/// What a browser must not do with a page of the dashboard: keep it, frame
/// it, or tell another site where the visitor comes from.
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        (CACHE_CONTROL, "no-store"),
        (CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY_VALUE),
        (X_FRAME_OPTIONS, "DENY"),
        (REFERRER_POLICY, "no-referrer"),
        (X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

/// Sends a visitor without a session to the sign-in form, whatever was asked for.
async fn require_session(State(auth): State<Arc<OpsAuth>>, request: Request, next: Next) -> Response {
    if auth.has_session(request.headers()) {
        next.run(request).await
    } else {
        see_other(LOGIN)
    }
}

async fn login_form(Extension(auth): Extension<Arc<OpsAuth>>, headers: HeaderMap) -> Response {
    if auth.has_session(&headers) {
        return see_other(HOME);
    }
    html_response(StatusCode::OK, login_page(None))
}

fn refused(status: StatusCode, problem: &str) -> Response {
    html_response(status, login_page(Some(problem)))
}

async fn login(
    Extension(auth): Extension<Arc<OpsAuth>>,
    peer: Option<Extension<ConnectInfo<Peer>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !is_same_site(&headers) {
        return refused(StatusCode::FORBIDDEN, "Sign in from this page.");
    }
    let password = url::form_urlencoded::parse(&body)
        .find(|(name, _)| name == "password")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default();

    let client = client_ip(peer.as_ref().map(|Extension(info)| info), &headers);
    match auth.login(client, &password) {
        Login::Granted(cookie) => {
            logger::info("[openshop] Operator dashboard: signed in");
            (
                StatusCode::SEE_OTHER,
                [(LOCATION, HOME.to_string()), (SET_COOKIE, cookie)],
            )
                .into_response()
        }
        Login::Refused => {
            logger::warn_with(
                "[openshop] Operator dashboard: wrong password",
                json!({ "client": client.map(|address| address.to_string()) }),
            );
            refused(StatusCode::UNAUTHORIZED, "Wrong password.")
        }
        Login::Busy(delay) => {
            let seconds = delay.as_secs() + u64::from(delay.subsec_nanos() > 0);
            let mut response = refused(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many attempts. Try again in a few seconds.",
            );
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(seconds.max(1)));
            response
        }
    }
}

/// Removes the session from the browser. It stays valid until it expires:
/// changing the password is what closes every session.
async fn logout(Extension(auth): Extension<Arc<OpsAuth>>, headers: HeaderMap) -> Response {
    if !is_same_site(&headers) {
        return html_response(
            StatusCode::FORBIDDEN,
            message_page("Not signed out", "Sign out from the dashboard."),
        );
    }
    (
        StatusCode::SEE_OTHER,
        [(LOCATION, LOGIN.to_string()), (SET_COOKIE, auth.expired_cookie())],
    )
        .into_response()
}

/// The routes of the dashboard, when the application gave it a password.
/// `secure_cookie` is off in development, where the server is reached over
/// plain HTTP.
pub(crate) fn create_ops_routes(app: &OpenShop, secure_cookie: bool) -> Option<Router<OpenShop>> {
    let password = app.config().ops_password.as_deref()?;
    let auth = Arc::new(OpsAuth::new(
        password,
        app.env().encryption_key.as_deref(),
        secure_cookie,
    ));

    let behind_session = Router::new()
        .route(HOME, get(pages::overview))
        .route("/ops/", get(pages::overview))
        .route("/ops/runs", get(pages::runs))
        .route("/ops/runs/{id}", get(pages::run))
        .route("/ops/logs", get(pages::logs))
        .route("/ops/shops", get(pages::shops))
        .route("/ops/shops/{shop}", get(pages::shop))
        .route("/ops/flows", get(pages::flows))
        .route("/ops/crons", get(pages::crons))
        .route("/ops/logout", post(logout))
        .route("/ops/{*rest}", any(|| async { pages::not_found_page() }))
        .layer(middleware::from_fn_with_state(auth.clone(), require_session));

    Some(
        Router::new()
            .route(LOGIN, get(login_form).post(login))
            .merge(behind_session)
            .layer(Extension(auth))
            .layer(middleware::from_fn(security_headers)),
    )
}
