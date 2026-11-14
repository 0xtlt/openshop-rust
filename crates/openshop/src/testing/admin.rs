//! Calling custom admin page loaders and actions without HTTP.

use anyhow::Result;
use indexmap::IndexMap;
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use super::TestContext;
use crate::admin::{AdminActor, AdminContext, AdminFunction, AdminFunctionKind, AdminPublicError, CustomAdminPage};
use crate::admin_paths::custom_admin_route_params;
use crate::app::OpenShop;
use crate::runtime::context_logger::ContextLogger;
use crate::server::admin_pages::{call_function, create_context, find_matching_page};
use crate::server::shop::ShopSession;
use crate::server::shopify_apps::DEFAULT_SHOPIFY_APP_HANDLE;
use crate::shopify::client_credentials::Grant;

#[derive(Debug, Clone)]
pub struct AdminTestOptions {
    pub shop: String,
    pub shopify_app: String,
    pub actor_id: String,
    pub params: IndexMap<String, String>,
    pub request_id: String,
    /// The locale of the staff member's admin (`de-CH`), matched to the
    /// languages of the application for `ctx.locale`. `None`: the default
    /// language.
    pub locale: Option<String>,
}

impl Default for AdminTestOptions {
    fn default() -> Self {
        Self {
            shop: "test.myshopify.com".to_string(),
            shopify_app: "default".to_string(),
            actor_id: "test-actor".to_string(),
            params: IndexMap::new(),
            request_id: "test-request".to_string(),
            locale: None,
        }
    }
}

pub async fn admin_context(app: &OpenShop, options: AdminTestOptions) -> Result<AdminContext> {
    Ok(AdminContext {
        actor: AdminActor {
            id: options.actor_id,
            session_id: "test-session".to_string(),
        },
        params: options.params,
        locale: app.config().admin_language(options.locale.as_deref()).to_string(),
        db: app.db().clone(),
        // As in production: the client of the session's shop, which renews a
        // token the Admin API refuses.
        shopify: app.shopify_client(&options.shop, &options.shopify_app).granting(Some(
            Grant::vouched(&options.shopify_app, &options.shop).unwrap_or(Grant::Background),
        )),
        connectors: app.connectors(&options.shop, &options.shopify_app),
        logger: ContextLogger::with_category(
            serde_json::json!({ "shop": options.shop, "shopifyApp": options.shopify_app, "requestId": options.request_id }),
            "admin",
        ),
        request_id: options.request_id,
        idempotency_key: None,
        shop: options.shop,
        shopify_app: options.shopify_app,
        app: app.clone(),
    })
}

/// The loaders and the actions of the admin pages, called as the view of a
/// page calls them.
// The error is the one a test asserts on, as the page receives it: it is not
// moved around, its size does not matter.
#[allow(clippy::result_large_err)]
impl TestContext {
    /// Calls the loader `name` of the admin page at `path` as staff user `1`
    /// of the shop of the test: [`call_loader_as`](Self::call_loader_as).
    pub async fn call_loader<T: DeserializeOwned>(
        &self,
        path: &str,
        name: &str,
        input: impl Serialize,
    ) -> Result<T, AdminPublicError> {
        self.call_loader_as(path, name, input, &self.shop, "1").await
    }

    /// Calls the loader `name` of the admin page at `path` (`/reviews/42` for
    /// the page `/reviews/:id`) as staff user `sub` of `shop` calls it from
    /// the view of the page: the access rules of the page and of the pages it
    /// is nested under, the `authorize` rule of the loader, its input schema,
    /// then the loader, which returns a `T`. Its `ctx.locale` is the language
    /// of the staff user's last page, as the server kept it
    /// (`ctx.page(…).locale("de")`), else the default one.
    ///
    /// The error is the one the page receives: an [`AdminPublicError`] the
    /// loader or a rule returned (`402 PLAN_REQUIRED` of
    /// [`billing::require_plan`](crate::billing::require_plan)), `403
    /// FORBIDDEN` when a rule says no, `422 INVALID_INPUT`, and `500
    /// INTERNAL_ERROR` for any other error, with its message, which the page
    /// does not see.
    ///
    /// Panics when no page matches `path`, when the page has no loader
    /// `name`, or when what the loader returns is not a `T`.
    pub async fn call_loader_as<T: DeserializeOwned>(
        &self,
        path: &str,
        name: &str,
        input: impl Serialize,
        shop: &str,
        sub: &str,
    ) -> Result<T, AdminPublicError> {
        self.call_admin_function(AdminFunctionKind::Loader, path, name, input, shop, sub)
            .await
    }

    /// Calls the action `name` of the admin page at `path` as staff user `1`
    /// of the shop of the test: [`call_action_as`](Self::call_action_as).
    pub async fn call_action<T: DeserializeOwned>(
        &self,
        path: &str,
        name: &str,
        input: impl Serialize,
    ) -> Result<T, AdminPublicError> {
        self.call_action_as(path, name, input, &self.shop, "1").await
    }

    /// Calls the action `name` of the admin page at `path` as staff user
    /// `sub` of `shop` calls it from the view of the page, with the rules,
    /// the input schema and the errors of
    /// [`call_loader_as`](Self::call_loader_as).
    pub async fn call_action_as<T: DeserializeOwned>(
        &self,
        path: &str,
        name: &str,
        input: impl Serialize,
        shop: &str,
        sub: &str,
    ) -> Result<T, AdminPublicError> {
        self.call_admin_function(AdminFunctionKind::Action, path, name, input, shop, sub)
            .await
    }

    async fn call_admin_function<T: DeserializeOwned>(
        &self,
        kind: AdminFunctionKind,
        path: &str,
        name: &str,
        input: impl Serialize,
        shop: &str,
        sub: &str,
    ) -> Result<T, AdminPublicError> {
        let kind_name = kind.as_str();
        let pages = self.app.custom_pages();
        let Some((page, params, function)) = find_function(pages, path, kind, name) else {
            panic!("{}", no_such_function(pages, path, kind, name));
        };
        // What the session token of the staff user gives the RPC of the page,
        // in the locale the server kept for them.
        let mut session = ShopSession {
            shop: shop.to_string(),
            shopify_app: DEFAULT_SHOPIFY_APP_HANDLE.to_string(),
            actor: AdminActor {
                id: sub.to_string(),
                session_id: "test-sid".to_string(),
            },
            locale: None,
        };
        session.locale = self.app.staff_locales().recall(&session);
        let context = create_context(&self.app, &session, params, Uuid::new_v4().to_string(), None);
        let input = serde_json::to_value(input)
            .unwrap_or_else(|error| panic!("[openshop] The input of the {kind_name} `{name}` is not JSON: {error}"));

        let output = call_function(pages, page, function, context, input)
            .await
            .map_err(|error| match error.downcast::<AdminPublicError>() {
                Ok(public) => public,
                Err(error) => AdminPublicError::new("INTERNAL_ERROR", format!("{error:#}")).status(500),
            })?;
        match serde_json::from_value(output.clone()) {
            Ok(output) => Ok(output),
            Err(error) => panic!(
                "[openshop] The {kind_name} `{name}` of the admin page `{}` returned {output}, which is not a `{}`: {error}",
                page.route_pattern,
                std::any::type_name::<T>()
            ),
        }
    }
}

/// The loader or the action `name` of the page at `page_path`, with the
/// values of the dynamic segments of the path: what a test calls there.
/// `None` when no page matches the path, or the page has no function of that
/// kind by that name.
fn find_function<'a>(
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

/// Why `path` and `name` name no function of `kind`: the pages there are,
/// or the functions of the page.
fn no_such_function(pages: &[CustomAdminPage], path: &str, kind: AdminFunctionKind, name: &str) -> String {
    let list = |names: Vec<&str>| {
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    };
    let Some(page) = pages
        .iter()
        .find(|page| crate::admin_paths::match_custom_admin_path(&page.route_pattern, path))
    else {
        return format!(
            "[openshop] No admin page of the application is at `{path}`. Its pages: {}",
            list(pages.iter().map(|page| page.route_pattern.as_str()).collect())
        );
    };
    let of_kind = |wanted: AdminFunctionKind| {
        list(
            page.page
                .functions
                .iter()
                .filter(|(_, function)| function.kind == wanted)
                .map(|(name, _)| name.as_str())
                .collect(),
        )
    };
    format!(
        "[openshop] The admin page `{}` has no {} `{name}`. Its loaders: {}; its actions: {}",
        page.route_pattern,
        kind.as_str(),
        of_kind(AdminFunctionKind::Loader),
        of_kind(AdminFunctionKind::Action),
    )
}
