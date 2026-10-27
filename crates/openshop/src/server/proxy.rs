//! Shopify app proxy routes (`/proxy/*`) and their extension-direct twins (`/ext/*`).
//!
//! `/proxy` accepts requests signed by Shopify's app proxy or a customer account
//! session token; `/ext` only accepts session tokens.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{RawPathParams, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter, on};
use indexmap::IndexMap;
use serde_json::Value;

use super::file_routes::mount_routes;
use super::hmac::{proxy_params_are_unambiguous, proxy_shop};
use super::http::{catch_panic, json_error, not_found, parse_query, report_failure};
use super::jwt::{customer_id, is_staff_subject, read_jwt_audience};
use crate::app::OpenShop;
use crate::declare::InvalidInput;
use crate::runtime::report;
use crate::server::connectors::Connectors;
use crate::shopify::client::ShopifyClient;
use crate::types::{ProxyAuthKind, ProxyContext, ProxyHandler, ProxyResponseType};

struct ResolvedProxyAuth {
    kind: ProxyAuthKind,
    /// The handle of the Shopify app.
    shopify_app: String,
    shop: String,
    customer_id: Option<String>,
}

fn signed_customer_id(value: Option<&String>) -> Option<String> {
    value
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .cloned()
}

fn resolve_proxy_auth(
    app: &OpenShop,
    raw_query: &str,
    query: &IndexMap<String, String>,
    headers: &IndexMap<String, String>,
    modes: &[ProxyAuthKind],
) -> Result<ResolvedProxyAuth, &'static str> {
    let authorization = headers.get("authorization").map(String::as_str).unwrap_or_default();
    if let Some(token) = authorization.strip_prefix("Bearer ") {
        if !modes.contains(&ProxyAuthKind::CustomerAccountJwt) {
            return Err("Unauthorized: session token not accepted on this route");
        }
        let Some(audience) = read_jwt_audience(token) else {
            return Err("Unauthorized: missing token audience");
        };
        let invalid = "Unauthorized: invalid or expired session token";
        let shopify_app = app.apps().by_api_key(&audience).map_err(|_| invalid)?;
        let verified = shopify_app
            .oauth
            .verify_extension_session_token(token)
            .map_err(|_| invalid)?;
        // The session token of a staff user opens the admin, not the storefront:
        // only the tokens of customer account and checkout extensions do here.
        let sub = verified.claims().get("sub").and_then(Value::as_str).unwrap_or_default();
        if is_staff_subject(sub) {
            return Err("Unauthorized: session token was issued to a staff user");
        }
        return Ok(ResolvedProxyAuth {
            kind: ProxyAuthKind::CustomerAccountJwt,
            shopify_app: shopify_app.handle.clone(),
            shop: verified.shop().to_string(),
            customer_id: customer_id(sub).map(str::to_string),
        });
    }

    if modes.contains(&ProxyAuthKind::AppProxyHmac) {
        let Ok((shopify_app, signed)) = app.apps().by_proxy_signature(raw_query) else {
            return Err("Invalid proxy signature");
        };
        // Who the request is for must read one way only for its signature to count.
        if !proxy_params_are_unambiguous(signed.params()) {
            return Err("Invalid proxy signature");
        }
        let Some(shop) = proxy_shop(signed.params()) else {
            return Err("Invalid shop parameter");
        };
        return Ok(ResolvedProxyAuth {
            kind: ProxyAuthKind::AppProxyHmac,
            shopify_app: shopify_app.handle.clone(),
            shop,
            customer_id: signed_customer_id(query.get("logged_in_customer_id")),
        });
    }

    Err("Unauthorized: missing session token")
}

/// The query handlers see: identity fields always reflect the verified identity.
fn sanitized_query(query: &IndexMap<String, String>, auth: &ResolvedProxyAuth) -> IndexMap<String, String> {
    let mut safe = query.clone();
    safe.insert("shop".to_string(), auth.shop.clone());
    match &auth.customer_id {
        Some(customer_id) => {
            safe.insert("logged_in_customer_id".to_string(), customer_id.clone());
        }
        None => {
            safe.shift_remove("logged_in_customer_id");
        }
    }
    safe
}

fn send_response(result: Value, response_type: ProxyResponseType) -> Response {
    match (response_type, result) {
        (ProxyResponseType::Liquid, Value::String(body)) => {
            ([(CONTENT_TYPE, "application/liquid")], body).into_response()
        }
        (ProxyResponseType::Html, Value::String(body)) => ([(CONTENT_TYPE, "text/html")], body).into_response(),
        (_, result) => ([(CONTENT_TYPE, "application/json")], result.to_string()).into_response(),
    }
}

#[derive(Clone)]
struct ProxyRoute {
    route_path: String,
    method: &'static str,
    response_type: ProxyResponseType,
    handler: ProxyHandler,
    auth_modes: &'static [ProxyAuthKind],
}

async fn handle(
    route: ProxyRoute,
    app: OpenShop,
    method: Method,
    uri: Uri,
    params: RawPathParams,
    header_map: HeaderMap,
    body: Bytes,
) -> Response {
    if !app.apps().has_configured_secret() {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SHOPIFY_API_SECRET is not configured",
        );
    }

    let headers: IndexMap<String, String> = header_map
        .iter()
        .filter_map(|(name, value)| Some((name.as_str().to_lowercase(), value.to_str().ok()?.to_string())))
        .collect();
    let query = parse_query(uri.query());
    let auth = match resolve_proxy_auth(&app, uri.query().unwrap_or_default(), &query, &headers, route.auth_modes) {
        Ok(auth) => auth,
        Err(error) => return json_error(StatusCode::UNAUTHORIZED, error),
    };
    report::describe(|request| {
        request
            .unit("proxy", format_args!("{} {}", route.method, route.route_path))
            .shop(&auth.shop, &auth.shopify_app);
    });

    // Parse body for non-GET methods
    let parsed_body = if route.method == "GET" || body.is_empty() {
        None
    } else {
        serde_json::from_slice::<Value>(&body).ok()
    };

    let context = ProxyContext {
        query: sanitized_query(&query, &auth),
        db: app.db().clone(),
        shopify: ShopifyClient::new(&app, &auth.shop, &auth.shopify_app),
        connectors: Connectors::for_shop(&app, &auth.shop, &auth.shopify_app),
        shop: auth.shop,
        shopify_app: auth.shopify_app,
        customer_id: auth.customer_id,
        auth: auth.kind,
        params: params
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        headers,
        path: uri.path().to_string(),
        method: method.as_str().to_string(),
        body: parsed_body,
        app,
    };

    let failure = match catch_panic("handler", (route.handler)(context)).await {
        Ok(result) => return send_response(result, route.response_type),
        // The request does not fit the typed input of the handler.
        Err(error) if error.is::<InvalidInput>() => {
            return json_error(StatusCode::UNPROCESSABLE_ENTITY, error.to_string());
        }
        Err(error) => error,
    };
    report_failure(
        format!("[openshop] Proxy {} {} error", route.method, route.route_path),
        &failure,
    );
    json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal proxy error")
}

fn method_filter(method: &str) -> MethodFilter {
    match method {
        "POST" => MethodFilter::POST,
        "PUT" => MethodFilter::PUT,
        "DELETE" => MethodFilter::DELETE,
        "PATCH" => MethodFilter::PATCH,
        _ => MethodFilter::GET,
    }
}

pub const PROXY_AUTH_MODES: &[ProxyAuthKind] = &[ProxyAuthKind::AppProxyHmac, ProxyAuthKind::CustomerAccountJwt];
pub const EXTENSION_AUTH_MODES: &[ProxyAuthKind] = &[ProxyAuthKind::CustomerAccountJwt];

/// Builds the proxy routes under `prefix` (`/proxy` or `/ext`).
pub fn create_proxy_routes(
    app: &OpenShop,
    prefix: &str,
    auth_modes: &'static [ProxyAuthKind],
    log: bool,
) -> anyhow::Result<Router<OpenShop>> {
    let proxies = &app.config().proxies;
    mount_routes("Proxy", prefix, proxies.keys().map(String::as_str), log, |route_path, key| {
        let definition = &proxies[key];
        let response_type = definition.response_type.unwrap_or_default();
        let mut method_router: MethodRouter<OpenShop> = MethodRouter::new().fallback(|| async { not_found() });

        for (method, handler) in definition.handlers() {
            let Some(handler) = handler else {
                continue;
            };
            let route = ProxyRoute {
                route_path: route_path.to_string(),
                method,
                response_type,
                handler: handler.clone(),
                auth_modes,
            };
            method_router = method_router.merge(on(
                method_filter(method),
                move |State(app): State<OpenShop>,
                      method: Method,
                      uri: Uri,
                      params: RawPathParams,
                      headers: HeaderMap,
                      body: Bytes| { handle(route.clone(), app, method, uri, params, headers, body) },
            ));
        }

        (method_router, response_type.as_str().to_string())
    })
}
