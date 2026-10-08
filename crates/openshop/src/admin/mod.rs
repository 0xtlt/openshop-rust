//! Server side of application-defined admin pages: page access rules, loaders
//! and actions called by the WebAssembly admin UI over RPC.

use std::cmp::Ordering;
use std::future::Future;
use std::sync::Arc;

use anyhow::{Result, bail};
use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::PgPool;

use crate::app::OpenShop;
use crate::server::connectors::Connectors;
use crate::shopify::client::ShopifyClient;
use crate::types::{AdminMenuItem, InputSchema, ShopScope};
use openshop_shared::{custom_admin_route_shape_key, is_safe_custom_admin_path, match_custom_admin_path};

pub use crate::types::{CustomAdminPageAccess, CustomAdminPagesResponse, ResolvedAdminPages};

/// The Shopify staff member behind the session token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminActor {
    pub id: String,
    pub session_id: String,
}

#[derive(Clone)]
pub struct AdminContext {
    pub shop: String,
    pub shopify_app: String,
    pub actor: AdminActor,
    /// Dynamic route segments of the page, e.g. `id` for `/reviews/:id`.
    pub params: IndexMap<String, String>,
    pub db: PgPool,
    pub shopify: ShopifyClient,
    pub connectors: Connectors,
    pub request_id: String,
    pub idempotency_key: Option<String>,
    pub app: OpenShop,
}

impl ShopScope for AdminContext {
    fn shop(&self) -> &str {
        &self.shop
    }

    fn shopify_app(&self) -> &str {
        &self.shopify_app
    }

    fn app(&self) -> &OpenShop {
        &self.app
    }
}

pub type AdminAuthorize = Arc<dyn Fn(AdminContext) -> BoxFuture<'static, Result<bool>> + Send + Sync>;
pub type AdminHandler = Arc<dyn Fn(AdminContext, Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;
pub type AdminOutputValidator = Arc<dyn Fn(&Value) -> Result<(), String> + Send + Sync>;

fn boxed_authorize<F, Fut>(authorize: F) -> AdminAuthorize
where
    F: Fn(AdminContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<bool>> + Send + 'static,
{
    Arc::new(move |context| Box::pin(authorize(context)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminFunctionKind {
    Loader,
    Action,
}

impl AdminFunctionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Loader => "loader",
            Self::Action => "action",
        }
    }
}

/// A loader (read) or action (write) callable from a custom admin page.
#[derive(Clone)]
pub struct AdminFunction {
    pub kind: AdminFunctionKind,
    /// Actions must validate their input.
    pub input: Option<InputSchema>,
    pub output: Option<AdminOutputValidator>,
    pub authorize: Option<AdminAuthorize>,
    pub handler: AdminHandler,
}

fn boxed_handler<F, Fut>(handler: F) -> AdminHandler
where
    F: Fn(AdminContext, Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value>> + Send + 'static,
{
    Arc::new(move |context, input| Box::pin(handler(context, input)))
}

impl AdminFunction {
    /// A loader: a read. Its input is passed through unless
    /// [`AdminFunction::input`] gives it a schema.
    pub fn loader<F, Fut>(handler: F) -> Self
    where
        F: Fn(AdminContext, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        Self {
            kind: AdminFunctionKind::Loader,
            input: None,
            output: None,
            authorize: None,
            handler: boxed_handler(handler),
        }
    }

    /// An action: a write, whose input is validated by `input`.
    pub fn action<F, Fut>(input: InputSchema, handler: F) -> Self
    where
        F: Fn(AdminContext, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        Self {
            kind: AdminFunctionKind::Action,
            input: Some(input),
            output: None,
            authorize: None,
            handler: boxed_handler(handler),
        }
    }

    /// A loader whose input is validated against the schema of `T` and
    /// deserialized into it.
    #[cfg(feature = "schema")]
    pub fn typed_loader<T, R, F, Fut>(handler: F) -> Self
    where
        T: DeserializeOwned + schemars::JsonSchema + Send + 'static,
        R: Serialize + 'static,
        F: Fn(AdminContext, T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        Self::loader(move |context, input| {
            let handler = handler.clone();
            async move {
                let input: T = serde_json::from_value(input)?;
                Ok(serde_json::to_value(handler(context, input).await?)?)
            }
        })
        .input(InputSchema::of::<T>())
    }

    /// An action whose input is validated against the schema of `T` and
    /// deserialized into it.
    #[cfg(feature = "schema")]
    pub fn typed_action<T, R, F, Fut>(handler: F) -> Self
    where
        T: DeserializeOwned + schemars::JsonSchema + Send + 'static,
        R: Serialize + 'static,
        F: Fn(AdminContext, T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        Self::action(InputSchema::of::<T>(), move |context, input| {
            let handler = handler.clone();
            async move {
                let input: T = serde_json::from_value(input)?;
                Ok(serde_json::to_value(handler(context, input).await?)?)
            }
        })
    }

    pub fn input(mut self, input: InputSchema) -> Self {
        self.input = Some(input);
        self
    }

    pub fn output(mut self, validate: impl Fn(&Value) -> Result<(), String> + Send + Sync + 'static) -> Self {
        self.output = Some(Arc::new(validate));
        self
    }

    /// Adds a rule of its own: the function runs only when `authorize`
    /// answers `true`.
    pub fn authorize<F, Fut>(mut self, authorize: F) -> Self
    where
        F: Fn(AdminContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<bool>> + Send + 'static,
    {
        self.authorize = Some(boxed_authorize(authorize));
        self
    }
}

/// The server half of a custom admin page at `path`, e.g. `/reviews` or `/reviews/:id`.
#[derive(Clone)]
pub struct AdminPage {
    pub path: String,
    /// The label of the page in the menu of the admin, when it has an entry there.
    pub nav: Option<String>,
    /// Gate for the page and every page nested under it.
    pub page_access: Option<AdminAuthorize>,
    pub functions: IndexMap<String, AdminFunction>,
}

impl AdminPage {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            nav: None,
            page_access: None,
            functions: IndexMap::new(),
        }
    }

    /// Gives the page an entry in the menu of the admin. A page with a
    /// dynamic segment (`/reviews/:id`) cannot have one.
    pub fn nav(mut self, label: impl Into<String>) -> Self {
        self.nav = Some(label.into());
        self
    }

    /// Gates the page and the pages nested under it: they open only when
    /// `authorize` answers `true`.
    pub fn access<F, Fut>(mut self, authorize: F) -> Self
    where
        F: Fn(AdminContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<bool>> + Send + 'static,
    {
        self.page_access = Some(boxed_authorize(authorize));
        self
    }

    pub fn function(mut self, name: impl Into<String>, function: AdminFunction) -> Self {
        self.functions.insert(name.into(), function);
        self
    }

    /// Shorthand for `.function(name, AdminFunction::loader(handler))`.
    pub fn loader<F, Fut>(self, name: impl Into<String>, handler: F) -> Self
    where
        F: Fn(AdminContext, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        self.function(name, AdminFunction::loader(handler))
    }

    /// Shorthand for `.function(name, AdminFunction::action(input, handler))`.
    pub fn action<F, Fut>(self, name: impl Into<String>, input: InputSchema, handler: F) -> Self
    where
        F: Fn(AdminContext, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        self.function(name, AdminFunction::action(input, handler))
    }
}

/// An error that is safe to show to the merchant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminPublicError {
    pub code: String,
    pub message: String,
    pub status: u16,
    pub field_errors: Option<IndexMap<String, String>>,
}

impl AdminPublicError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            status: 400,
            field_errors: None,
        }
    }

    pub fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    pub fn field_errors<I, K, V>(mut self, errors: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.field_errors = Some(
            errors
                .into_iter()
                .map(|(field, message)| (field.into(), message.into()))
                .collect(),
        );
        self
    }
}

impl std::fmt::Display for AdminPublicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AdminPublicError {}

/// A registered page with its derived identifiers.
#[derive(Clone)]
pub struct CustomAdminPage {
    /// Path segments joined by `/`, with dynamic ones written `[name]`.
    pub id: String,
    /// `/` + id.
    pub path: String,
    /// Router pattern with `:name` segments.
    pub route_pattern: String,
    pub page: AdminPage,
}

fn is_param_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters.next().is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn route_segment(segment: &str) -> Result<(String, String)> {
    let dynamic = segment
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .or_else(|| segment.strip_prefix(':'));
    if let Some(name) = dynamic.filter(|name| is_param_name(name)) {
        return Ok((format!("[{name}]"), format!(":{name}")));
    }
    let plain = !segment.is_empty()
        && segment
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-');
    if !plain {
        bail!("[openshop] Invalid custom admin page segment \"{segment}\"");
    }
    Ok((segment.to_string(), segment.to_string()))
}

fn compare_patterns(left: &str, right: &str) -> Ordering {
    let left_segments: Vec<&str> = left.split('/').collect();
    let right_segments: Vec<&str> = right.split('/').collect();
    for index in 0..left_segments.len().max(right_segments.len()) {
        match (left_segments.get(index), right_segments.get(index)) {
            (Some(left), Some(right)) if left == right => continue,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(left), Some(right)) => {
                let left_dynamic = left.starts_with(':');
                if left_dynamic != right.starts_with(':') {
                    return if left_dynamic {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    };
                }
                return left.cmp(right);
            }
        }
    }
    Ordering::Equal
}

/// Derives ids and route patterns, sorted by specificity. Fails on reserved
/// paths and on routes that could match the same URL.
pub fn resolve_custom_admin_pages(pages: &[AdminPage]) -> Result<Vec<CustomAdminPage>> {
    let mut resolved = Vec::with_capacity(pages.len());

    for page in pages {
        let segments: Vec<&str> = page.path.split('/').filter(|segment| !segment.is_empty()).collect();
        if segments.is_empty() {
            bail!("[openshop] A custom admin page cannot replace the built-in Home page");
        }
        let mapped = segments
            .iter()
            .map(|segment| route_segment(segment))
            .collect::<Result<Vec<_>>>()?;
        let id = mapped
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>()
            .join("/");
        let path = format!("/{id}");
        let route_pattern = format!(
            "/{}",
            mapped
                .iter()
                .map(|(_, pattern)| pattern.as_str())
                .collect::<Vec<_>>()
                .join("/")
        );
        if !is_safe_custom_admin_path(&route_pattern, true) {
            bail!("[openshop] Custom admin page path \"{path}\" is reserved or invalid");
        }
        resolved.push(CustomAdminPage {
            id,
            path,
            route_pattern,
            page: page.clone(),
        });
    }

    resolved.sort_by(|left, right| compare_patterns(&left.route_pattern, &right.route_pattern));

    let mut routes_by_shape: IndexMap<String, &str> = IndexMap::new();
    for page in &resolved {
        let shape = custom_admin_route_shape_key(&page.route_pattern);
        if let Some(existing) = routes_by_shape.get(&shape) {
            bail!(
                "[openshop] Custom admin page routes \"{existing}\" and \"{}\" have the same route shape",
                page.route_pattern
            );
        }
        routes_by_shape.insert(shape, &page.route_pattern);
    }
    Ok(resolved)
}

/// Checks that every entry of the admin menu is the static path of a listed page.
pub fn validate_admin_menu(menu: &[AdminMenuItem], pages: &[CustomAdminPage]) -> Result<Vec<AdminMenuItem>> {
    let mut seen: Vec<&str> = Vec::new();

    for (index, item) in menu.iter().enumerate() {
        if item.label.trim().is_empty() {
            bail!("[openshop] Invalid config: adminMenu[{index}].label must be a non-empty string");
        }
        if !is_safe_custom_admin_path(&item.path, false) || item.path.contains('[') || item.path.contains(':') {
            bail!("[openshop] Invalid config: adminMenu[{index}].path must be a static custom page path");
        }
        if !pages
            .iter()
            .any(|page| match_custom_admin_path(&page.route_pattern, &item.path))
        {
            bail!(
                "[openshop] Invalid config: adminMenu[{index}].path \"{}\" does not match a page of the application",
                item.path
            );
        }
        if seen.contains(&item.path.as_str()) {
            bail!(
                "[openshop] Invalid config: adminMenu[{index}].path \"{}\" is listed twice",
                item.path
            );
        }
        seen.push(&item.path);
    }
    Ok(menu.to_vec())
}

/// Calls a loader or action directly, applying its authorization and input validation.
pub async fn invoke_admin_function<T: DeserializeOwned>(
    function: &AdminFunction,
    context: AdminContext,
    input: impl Serialize,
) -> Result<T> {
    if let Some(authorize) = &function.authorize
        && !authorize(context.clone()).await?
    {
        bail!("Admin function authorization rejected");
    }
    let mut input = serde_json::to_value(input)?;
    if let Some(schema) = &function.input {
        input = schema.parse(input).map_err(anyhow::Error::msg)?;
    }
    Ok(serde_json::from_value((function.handler)(context, input).await?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(paths: &[&str]) -> Result<Vec<String>> {
        let pages: Vec<AdminPage> = paths.iter().map(|path| AdminPage::new(*path)).collect();
        Ok(resolve_custom_admin_pages(&pages)?
            .into_iter()
            .map(|page| page.route_pattern)
            .collect())
    }

    #[test]
    fn derives_ids_paths_and_patterns() {
        let pages =
            resolve_custom_admin_pages(&[AdminPage::new("/reviews/:id"), AdminPage::new("reviews/[slug]/edit")])
                .unwrap();
        let summary: Vec<(&str, &str, &str)> = pages
            .iter()
            .map(|page| (page.id.as_str(), page.path.as_str(), page.route_pattern.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("reviews/[id]", "/reviews/[id]", "/reviews/:id"),
                ("reviews/[slug]/edit", "/reviews/[slug]/edit", "/reviews/:slug/edit"),
            ]
        );
    }

    #[test]
    fn sorts_static_routes_before_dynamic_ones() {
        assert_eq!(
            patterns(&["/reviews/:id", "/reviews/new", "/reviews", "/analytics"]).unwrap(),
            vec!["/analytics", "/reviews", "/reviews/new", "/reviews/:id"]
        );
    }

    #[test]
    fn rejects_home_reserved_and_invalid_paths() {
        assert!(patterns(&["/"]).unwrap_err().to_string().contains("built-in Home page"));
        assert!(
            patterns(&["/flows/custom"])
                .unwrap_err()
                .to_string()
                .contains("is reserved or invalid")
        );
        assert!(
            patterns(&["/reviews/a b"])
                .unwrap_err()
                .to_string()
                .contains("Invalid custom admin page segment \"a b\"")
        );
        assert!(
            patterns(&["/reviews/:1"])
                .unwrap_err()
                .to_string()
                .contains("Invalid custom admin page segment")
        );
    }

    #[test]
    fn rejects_ambiguous_routes() {
        let error = patterns(&["/reviews/:id", "/reviews/:slug"]).unwrap_err().to_string();
        assert!(error.contains("have the same route shape"), "{error}");
    }

    #[test]
    fn validates_the_menu_against_pages() {
        let pages = resolve_custom_admin_pages(&[AdminPage::new("/reviews"), AdminPage::new("/reviews/:id")]).unwrap();
        let item = |label: &str, path: &str| AdminMenuItem {
            label: label.into(),
            path: path.into(),
        };

        assert_eq!(
            validate_admin_menu(&[item("Reviews", "/reviews")], &pages)
                .unwrap()
                .len(),
            1
        );
        let error = |menu: &[AdminMenuItem]| validate_admin_menu(menu, &pages).unwrap_err().to_string();
        assert!(error(&[item(" ", "/reviews")]).contains("label must be a non-empty string"));
        assert!(error(&[item("One", "/reviews/:id")]).contains("must be a static custom page path"));
        assert!(error(&[item("Flows", "/flows")]).contains("must be a static custom page path"));
        assert!(error(&[item("Orders", "/orders")]).contains("does not match a page of the application"));
        assert!(error(&[item("Reviews", "/reviews"), item("Again", "/reviews")]).contains("is listed twice"));
    }

    #[test]
    fn public_errors_carry_status_and_field_errors() {
        let error = AdminPublicError::new("INVALID", "Title is required")
            .status(422)
            .field_errors([("title", "Required")]);
        assert_eq!(error.to_string(), "Title is required");
        assert_eq!(error.status, 422);
        assert_eq!(error.field_errors.unwrap()["title"], "Required");
        assert_eq!(AdminPublicError::new("X", "y").status, 400);
    }
}
