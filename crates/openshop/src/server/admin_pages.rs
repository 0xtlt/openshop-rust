//! Admin API for custom admin pages: discovery, access checks and the RPC
//! endpoint behind loaders and actions.

use std::panic::AssertUnwindSafe;
use std::time::Instant;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, RawQuery, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures::FutureExt;
use indexmap::IndexMap;
use serde_json::{Value, json};
use uuid::Uuid;

use super::http::{header, json_response, parse_query};
use super::shop::ShopSession;
use crate::admin::{
    AdminAuthorize, AdminContext, AdminFunctionKind, AdminPublicError, CustomAdminPage, validate_admin_menu,
};
use crate::app::OpenShop;
use crate::engine::step::error_message;
use crate::runtime::logger;
use crate::types::{CustomAdminPageAccess, CustomAdminPagesResponse, JsonMap};
use openshop_shared::match_custom_admin_path;

/// JavaScript's `decodeURIComponent`; `None` for malformed input.
fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = value.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn extract_params(pattern: &str, pathname: &str) -> Option<IndexMap<String, String>> {
    if !match_custom_admin_path(pattern, pathname) {
        return None;
    }
    let pattern_segments = pattern.split('/').filter(|segment| !segment.is_empty());
    let path_segments = pathname.split('/').filter(|segment| !segment.is_empty());
    pattern_segments
        .zip(path_segments)
        .filter_map(|(pattern, path)| Some((pattern.strip_prefix(':')?, path)))
        .map(|(name, value)| Some((name.to_string(), decode_uri_component(value)?)))
        .collect()
}

/// The most specific page matching a path. Pages are sorted with static
/// segments first, so `/reviews/new` wins over `/reviews/:id`.
fn find_matching_page<'a>(pages: &'a [CustomAdminPage], pathname: &str) -> Option<&'a CustomAdminPage> {
    pages
        .iter()
        .find(|page| match_custom_admin_path(&page.route_pattern, pathname))
}

/// Access rules of the page and of every page it is nested under.
fn ancestor_access<'a>(pages: &'a [CustomAdminPage], page: &CustomAdminPage) -> Vec<&'a AdminAuthorize> {
    pages
        .iter()
        .filter(|candidate| candidate.id == page.id || page.id.starts_with(&format!("{}/", candidate.id)))
        .filter_map(|candidate| candidate.page.page_access.as_ref())
        .collect()
}

async fn create_context(
    app: &OpenShop,
    session: &ShopSession,
    params: IndexMap<String, String>,
    request_id: String,
    idempotency_key: Option<String>,
) -> anyhow::Result<AdminContext> {
    Ok(AdminContext {
        shop: session.shop.clone(),
        shopify_app: session.shopify_app.clone(),
        actor: session.actor.clone(),
        params,
        db: app.db().clone(),
        shopify: app.shopify_client(&session.shop, &session.shopify_app),
        connectors: app.connectors(&session.shop, &session.shopify_app),
        request_id,
        idempotency_key,
        app: app.clone(),
    })
}

async fn is_allowed(
    access: &[&AdminAuthorize],
    context: &AdminContext,
    authorize: Option<&AdminAuthorize>,
) -> anyhow::Result<bool> {
    for rule in access {
        if !rule(context.clone()).await? {
            return Ok(false);
        }
    }
    match authorize {
        Some(authorize) => authorize(context.clone()).await,
        None => Ok(true),
    }
}

async fn page_is_allowed(app: &OpenShop, session: &ShopSession, page: &CustomAdminPage, pathname: &str) -> bool {
    let check = async {
        let Some(params) = extract_params(&page.route_pattern, pathname) else {
            return Ok(false);
        };
        let context = create_context(app, session, params, Uuid::new_v4().to_string(), None).await?;
        is_allowed(&ancestor_access(app.custom_pages(), page), &context, None).await
    };
    let outcome: anyhow::Result<bool> = AssertUnwindSafe(check)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("access rule panicked")));

    outcome.unwrap_or_else(|error| {
        logger::warn_with(
            "[openshop] Custom admin page access check failed",
            json!({ "pageId": page.id, "error": error_message(&error) }),
        );
        false
    })
}

async fn list_pages(State(app): State<OpenShop>, Extension(session): Extension<ShopSession>) -> Response {
    let config = app.config();
    let empty = || json_response(StatusCode::OK, json!(CustomAdminPagesResponse::default()));
    if !config.custom_pages_enabled() {
        return empty();
    }

    let pages = app.custom_pages();
    let navigation = match validate_admin_menu(&config.resolved_admin_menu(), pages) {
        Ok(navigation) => navigation,
        Err(error) => {
            logger::warn_with(
                "[openshop] Custom admin page bootstrap failed; using built-in pages only",
                json!({ "error": error_message(&error) }),
            );
            return empty();
        }
    };

    let mut access = Vec::with_capacity(pages.len());
    for page in pages {
        let allowed =
            !page.route_pattern.contains(':') && page_is_allowed(&app, &session, page, &page.route_pattern).await;
        access.push(CustomAdminPageAccess {
            id: page.id.clone(),
            path: page.route_pattern.clone(),
            allowed,
        });
    }

    let mut authorized_navigation = Vec::new();
    for item in navigation {
        if let Some(page) = find_matching_page(pages, &item.path)
            && page_is_allowed(&app, &session, page, &item.path).await
        {
            authorized_navigation.push(item);
        }
    }

    json_response(
        StatusCode::OK,
        json!(CustomAdminPagesResponse {
            navigation: authorized_navigation,
            pages: access,
        }),
    )
}

async fn page_access(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let denied = || json_response(StatusCode::NOT_FOUND, json!({ "allowed": false }));
    let query = parse_query(raw_query.as_deref());
    let Some(pathname) = query.get("path").filter(|path| !path.is_empty()) else {
        return denied();
    };
    if !app.config().custom_pages_enabled() {
        return denied();
    }
    let Some(page) = find_matching_page(app.custom_pages(), pathname) else {
        return denied();
    };
    json_response(
        StatusCode::OK,
        json!({ "allowed": page_is_allowed(&app, &session, page, pathname).await }),
    )
}

fn public_error(code: &str, message: &str, status: u16) -> anyhow::Error {
    AdminPublicError::new(code, message).status(status).into()
}

fn rpc_not_found() -> anyhow::Error {
    public_error("NOT_FOUND", "Not found", 404)
}

async fn execute_rpc(
    app: &OpenShop,
    session: &ShopSession,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
    request_id: &str,
    metadata: &mut JsonMap,
) -> anyhow::Result<String> {
    if !app.config().custom_pages_enabled() {
        return Err(rpc_not_found());
    }

    let suffix = path
        .split_once("/pages/custom")
        .map(|(_, suffix)| suffix)
        .unwrap_or_default();
    const MARKER: &str = "/_rpc/";
    let marker_index = suffix.rfind(MARKER).ok_or_else(rpc_not_found)?;
    // Empty for the page at `/`, the home of the admin.
    let page_path = &suffix[..marker_index];
    let mut parts = suffix[marker_index + MARKER.len()..].split('/');
    let kind = match parts.next() {
        Some("loader") => AdminFunctionKind::Loader,
        Some("action") => AdminFunctionKind::Action,
        _ => return Err(rpc_not_found()),
    };
    let encoded_name = parts.next().filter(|name| !name.is_empty()).ok_or_else(rpc_not_found)?;
    if parts.next().is_some() {
        return Err(rpc_not_found());
    }
    let name = decode_uri_component(encoded_name).ok_or_else(rpc_not_found)?;

    let pages = app.custom_pages();
    let page = find_matching_page(pages, page_path).ok_or_else(rpc_not_found)?;
    let params = extract_params(&page.route_pattern, page_path).ok_or_else(rpc_not_found)?;
    let definition = page
        .page
        .functions
        .get(&name)
        .filter(|definition| definition.kind == kind)
        .ok_or_else(rpc_not_found)?;

    let idempotency_key = header(headers, "idempotency-key").map(str::to_string);
    let context = create_context(app, session, params, request_id.to_string(), idempotency_key).await?;
    if !is_allowed(&ancestor_access(pages, page), &context, definition.authorize.as_ref()).await? {
        return Err(public_error("FORBIDDEN", "Forbidden", 403));
    }

    let mut input = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|body| body.get("input").cloned())
        .unwrap_or(Value::Null);
    match &definition.input {
        Some(schema) => {
            input = schema
                .parse(input)
                .map_err(|summary| public_error("INVALID_INPUT", &summary, 422))?;
        }
        None if kind == AdminFunctionKind::Action => {
            return Err(public_error("INVALID_ACTION", "Action input schema is required", 500));
        }
        None => {}
    }

    let output = (definition.handler)(context, input).await?;
    if let Some(validate) = &definition.output {
        validate(&output).map_err(|summary| anyhow::anyhow!("Invalid output: {summary}"))?;
    }

    metadata.insert("pageId".into(), Value::from(page.id.clone()));
    metadata.insert("functionName".into(), Value::from(name));
    metadata.insert("kind".into(), Value::from(kind.as_str()));
    metadata.insert("status".into(), Value::from(200));
    Ok(output.to_string())
}

async fn rpc(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started_at = Instant::now();
    let request_id = Uuid::new_v4().to_string();
    let mut metadata = JsonMap::new();
    metadata.insert("requestId".into(), Value::from(request_id.clone()));
    metadata.insert("shop".into(), Value::from(session.shop.clone()));
    metadata.insert("shopifyApp".into(), Value::from(session.shopify_app.clone()));
    metadata.insert("actorId".into(), Value::from(session.actor.id.clone()));

    // A panicking access rule or function is an internal error, not a dropped connection.
    let execution = execute_rpc(&app, &session, uri.path(), &headers, &body, &request_id, &mut metadata);
    let outcome = AssertUnwindSafe(execution)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("admin function panicked")));
    metadata.insert(
        "durationMs".into(),
        Value::from(started_at.elapsed().as_millis() as u64),
    );

    match outcome {
        Ok(output) => {
            logger::info_with("[openshop] Custom admin function completed", Value::Object(metadata));
            ([(CONTENT_TYPE, "application/json; charset=UTF-8")], output).into_response()
        }
        Err(error) => match error.downcast_ref::<AdminPublicError>() {
            Some(public) => {
                metadata.insert("status".into(), Value::from(public.status));
                metadata.insert("code".into(), Value::from(public.code.clone()));
                logger::warn_with("[openshop] Custom admin function rejected", Value::Object(metadata));

                let mut payload = JsonMap::new();
                payload.insert("error".into(), Value::from(public.message.clone()));
                payload.insert("code".into(), Value::from(public.code.clone()));
                payload.insert("requestId".into(), Value::from(request_id));
                if let Some(field_errors) = &public.field_errors {
                    payload.insert("fieldErrors".into(), json!(field_errors));
                }
                let status = StatusCode::from_u16(public.status).unwrap_or(StatusCode::BAD_REQUEST);
                json_response(status, Value::Object(payload))
            }
            None => {
                metadata.insert("error".into(), Value::from(error_message(&error)));
                logger::error_with("[openshop] Custom admin function failed", Value::Object(metadata));
                json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "Internal server error", "code": "INTERNAL_ERROR", "requestId": request_id }),
                )
            }
        },
    }
}

pub fn create_custom_admin_page_routes() -> Router<OpenShop> {
    Router::new()
        .route("/api/pages/custom", get(list_pages).post(rpc))
        .route("/api/pages/custom/access", get(page_access).post(rpc))
        .route("/api/pages/custom/{*rest}", post(rpc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{AdminPage, resolve_custom_admin_pages};

    #[test]
    fn decodes_uri_components() {
        assert_eq!(decode_uri_component("save%20review").as_deref(), Some("save review"));
        assert_eq!(decode_uri_component("caf%C3%A9").as_deref(), Some("café"));
        assert_eq!(decode_uri_component("plain").as_deref(), Some("plain"));
        assert_eq!(decode_uri_component("%"), None);
        assert_eq!(decode_uri_component("%zz"), None);
        assert_eq!(decode_uri_component("%ff"), None);
    }

    #[test]
    fn extracts_route_params() {
        let params = extract_params("/reviews/:id/items/:item", "/reviews/42/items/a%20b").unwrap();
        assert_eq!(params["id"], "42");
        assert_eq!(params["item"], "a b");
        assert!(extract_params("/reviews", "/reviews").unwrap().is_empty());
        assert!(extract_params("/reviews/:id", "/reviews").is_none());
        assert!(extract_params("/reviews/:id", "/reviews/%zz").is_none());
    }

    #[test]
    fn finds_pages_and_their_ancestors() {
        let pages = resolve_custom_admin_pages(&[
            AdminPage::new("/reviews"),
            AdminPage::new("/reviews/:id"),
            AdminPage::new("/reviewers"),
        ])
        .unwrap();
        assert_eq!(find_matching_page(&pages, "/reviews/42").unwrap().id, "reviews/[id]");
        assert_eq!(find_matching_page(&pages, "/reviews").unwrap().id, "reviews");
        assert!(find_matching_page(&pages, "/orders").is_none());

        let detail = find_matching_page(&pages, "/reviews/42").unwrap();
        let related: Vec<&str> = pages
            .iter()
            .filter(|candidate| candidate.id == detail.id || detail.id.starts_with(&format!("{}/", candidate.id)))
            .map(|candidate| candidate.id.as_str())
            .collect();
        assert_eq!(related, vec!["reviews", "reviews/[id]"]);
    }
}
