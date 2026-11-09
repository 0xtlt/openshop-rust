//! Custom admin pages on the server: their access rules, the context of their
//! functions, the call of a function through its rules and its schema, and the
//! Admin API that lists them and says whether a path may be opened.

use axum::Router;
use axum::extract::{Extension, RawQuery, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use indexmap::IndexMap;
use serde_json::{Value, json};
use uuid::Uuid;

use super::http::{catch_panic, json_response, parse_query};
use super::mcp::schema::property_errors;
use super::shop::ShopSession;
use crate::admin::{
    AdminAuthorize, AdminContext, AdminFunction, AdminFunctionKind, AdminPublicError, CustomAdminPage, field_message,
};
use crate::app::OpenShop;
use crate::engine::step::error_message;
use crate::runtime::context_logger::ContextLogger;
use crate::runtime::logger;
use crate::types::{CustomAdminPageAccess, CustomAdminPagesResponse};
use openshop_shared::{custom_admin_route_params, match_custom_admin_path};

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

pub(crate) fn create_context(
    app: &OpenShop,
    session: &ShopSession,
    params: IndexMap<String, String>,
    request_id: String,
    idempotency_key: Option<String>,
) -> AdminContext {
    AdminContext {
        shop: session.shop.clone(),
        shopify_app: session.shopify_app.clone(),
        actor: session.actor.clone(),
        params,
        db: app.db().clone(),
        shopify: session.shopify_client(app),
        connectors: app.connectors(&session.shop, &session.shopify_app),
        logger: ContextLogger::with_category(
            json!({ "shop": session.shop, "shopifyApp": session.shopify_app, "requestId": request_id }),
            "admin",
        ),
        request_id,
        idempotency_key,
        app: app.clone(),
    }
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

pub(crate) async fn page_is_allowed(
    app: &OpenShop,
    session: &ShopSession,
    page: &CustomAdminPage,
    pathname: &str,
) -> bool {
    let check = async {
        let Some(params) = custom_admin_route_params(&page.route_pattern, pathname) else {
            return Ok(false);
        };
        let context = create_context(app, session, params, Uuid::new_v4().to_string(), None);
        is_allowed(&ancestor_access(app.custom_pages(), page), &context, None).await
    };
    let outcome: anyhow::Result<bool> = catch_panic("access rule", check).await;

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
    // The menu was checked against the pages with the rest of the config.
    for item in config.resolved_admin_menu() {
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

/// The loader or the action `name` of the page at `page_path`, with the
/// values of the dynamic segments of the path: what a test calls there.
/// `None` when no page matches the path, or the page has no function of that
/// kind by that name.
pub(crate) fn find_function<'a>(
    pages: &'a [CustomAdminPage],
    page_path: &str,
    kind: AdminFunctionKind,
    name: &str,
) -> Option<(&'a CustomAdminPage, IndexMap<String, String>, &'a AdminFunction)> {
    let page = find_matching_page(pages, page_path)?;
    let params = custom_admin_route_params(&page.route_pattern, page_path)?;
    let function = page.page.functions.get(name).filter(|function| function.kind == kind)?;
    Some((page, params, function))
}

/// Runs `function` of `page`, as a form of the page and a test call it: the access rules of
/// the page and of the pages it is nested under and the rule of the function
/// first (`403 FORBIDDEN` when one says no), then the input schema
/// (`422 INVALID_INPUT`, with the error of each field it can tell), then the
/// function.
pub(crate) async fn call_function(
    pages: &[CustomAdminPage],
    page: &CustomAdminPage,
    function: &AdminFunction,
    context: AdminContext,
    input: Value,
) -> anyhow::Result<Value> {
    if !is_allowed(&ancestor_access(pages, page), &context, function.authorize.as_ref()).await? {
        return Err(public_error("FORBIDDEN", "Forbidden", 403));
    }
    let input = match &function.input {
        Some(schema) => schema.parse(input.clone()).map_err(|summary| {
            let fields = property_errors(&input, &schema.json_schema);
            let refusal = AdminPublicError::new("INVALID_INPUT", summary).status(422);
            if fields.is_empty() {
                refusal
            } else {
                refusal.field_errors(
                    fields
                        .iter()
                        .map(|(field, error)| (field.as_str(), field_message(error))),
                )
            }
        })?,
        None if function.kind == AdminFunctionKind::Action => {
            return Err(public_error("INVALID_ACTION", "Action input schema is required", 500));
        }
        None => input,
    };
    (function.handler)(context, input).await
}

pub fn create_custom_admin_page_routes() -> Router<OpenShop> {
    Router::new()
        .route("/api/pages/custom", get(list_pages))
        .route("/api/pages/custom/access", get(page_access))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{AdminPage, resolve_custom_admin_pages};

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
