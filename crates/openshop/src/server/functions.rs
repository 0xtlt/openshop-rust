//! Admin API for Shopify Function instances. Instances live in Shopify: every
//! call goes through the Admin GraphQL API of the authenticated shop.

use std::collections::HashMap;
use std::sync::LazyLock;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, put};
use regex::Regex;
use serde_json::{Value, json};

use super::function_mutations::{
    METAFIELD_NAMESPACE, build_create_mutation, build_delete_mutation, build_update_mutation, extract_errors,
    extract_payload, get_mutations, has_user_error_code, resolve_title, user_error_messages,
};
use super::http::{ApiResult, json_error, json_object_body, json_response, parse_query};
use super::provider_config::{ConfigError, parse_provider_config};
use super::shop::ShopSession;
use crate::app::{OpenShop, OpenShopConfig};
use crate::runtime::logger;
use crate::shopify::client::ShopifyClient;
use crate::types::{DiscountMode, Function, FunctionType, JsonMap};

fn discount_mode_from_body(body: &JsonMap, fallback: DiscountMode) -> DiscountMode {
    body.get("mode")
        .and_then(Value::as_str)
        .and_then(DiscountMode::parse)
        .unwrap_or(fallback)
}

static CAMEL_BOUNDARY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([a-z0-9])([A-Z])").expect("valid regex"));
static SEPARATORS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[-_]+").expect("valid regex"));

/// `volumeDiscount` → `Volume Discount`, `cart-transform` → `Cart Transform`.
pub fn humanize(value: &str) -> String {
    let spaced = CAMEL_BOUNDARY.replace_all(value, "$1 $2");
    let spaced = SEPARATORS.replace_all(&spaced, " ");

    let mut result = String::with_capacity(spaced.len());
    let mut at_word_start = true;
    for character in spaced.trim().chars() {
        let is_word = character.is_ascii_alphanumeric() || character == '_';
        if is_word && at_word_start {
            result.extend(character.to_uppercase());
        } else {
            result.push(character);
        }
        at_word_start = !is_word;
    }
    result
}

fn function_label(key: &str, definition: &Function) -> String {
    let non_empty = |value: String| (!value.is_empty()).then_some(value);
    definition
        .label
        .as_deref()
        .map(str::trim)
        .map(str::to_string)
        .and_then(non_empty)
        .or_else(|| non_empty(humanize(key)))
        .unwrap_or_else(|| humanize(&definition.handle))
}

/// A function of the app, as the admin shows it: its definition, the key the
/// app registered it under, and its label.
pub(crate) struct AppFunction<'a> {
    pub(crate) key: &'a str,
    pub(crate) definition: &'a Function,
    pub(crate) label: String,
}

impl<'a> AppFunction<'a> {
    pub(crate) fn of(key: &'a str, definition: &'a Function) -> Self {
        Self {
            key,
            definition,
            label: function_label(key, definition),
        }
    }

    /// Its handle, in Shopify and in the URLs of the admin.
    pub(crate) fn handle(&self) -> &'a str {
        &self.definition.handle
    }

    /// The ways a discount can be given (automatically, with a code), the
    /// first one the default: automatic unless the discount says otherwise.
    /// `None` for the other types of function.
    pub(crate) fn modes(&self) -> Option<Vec<DiscountMode>> {
        (self.definition.function_type == FunctionType::Discount).then(|| {
            self.definition
                .modes
                .clone()
                .unwrap_or_else(|| vec![DiscountMode::Automatic])
        })
    }

    /// Whether an instance can be changed, or must be deleted and created again.
    pub(crate) fn supports_update(&self) -> bool {
        get_mutations(self.definition, None).update.is_some()
    }

    /// Whether a shop has one instance at most: a Cart Transform.
    pub(crate) fn is_singleton(&self) -> bool {
        self.definition.function_type == FunctionType::CartTransform
    }
}

/// The functions of the app, in the order it declares them.
pub(crate) fn app_functions(config: &OpenShopConfig) -> Vec<AppFunction<'_>> {
    config
        .functions
        .iter()
        .map(|(key, definition)| AppFunction::of(key, definition))
        .collect()
}

/// The function of the app whose handle is `handle`.
pub(crate) fn find_function<'a>(config: &'a OpenShopConfig, handle: &str) -> Option<AppFunction<'a>> {
    config
        .functions
        .iter()
        .find(|(_, definition)| definition.handle == handle)
        .map(|(key, definition)| AppFunction::of(key, definition))
}

/// Pages read for one listing, at 50 nodes each: past that, the rest is not
/// listed (and a warning says so) rather than a request that never ends.
const MAX_PAGES: usize = 20;

/// Every node of a connection, page after page. `query` takes an optional
/// `$after: String` that the connection reads, and asks for
/// `pageInfo { hasNextPage endCursor }`; `variables` are the others it takes.
async fn connection_pages(
    shopify: &ShopifyClient,
    query: &str,
    variables: JsonMap,
    connection: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut nodes = Vec::new();
    let mut after: Option<String> = None;
    for page in 1..=MAX_PAGES {
        let mut page_variables = variables.clone();
        if let Some(cursor) = &after {
            page_variables.insert("after".into(), json!(cursor));
        }
        let page_variables = (!page_variables.is_empty()).then_some(Value::Object(page_variables));
        let data = shopify.graphql(query, page_variables).await?;
        nodes.extend(connection_nodes(&data, connection).into_iter().cloned());

        let page_info = data.get(connection).and_then(|connection| connection.get("pageInfo"));
        let has_next = page_info.and_then(|info| info.get("hasNextPage")) == Some(&Value::Bool(true));
        let cursor = page_info
            .and_then(|info| info.get("endCursor"))
            .and_then(Value::as_str)
            .map(str::to_string);
        // A cursor that does not move would ask for the same page forever.
        let Some(cursor) = cursor.filter(|cursor| has_next && after.as_ref() != Some(cursor)) else {
            return Ok(nodes);
        };
        if page == MAX_PAGES {
            logger::warn_with(
                "[openshop] A listing of Shopify nodes was cut",
                json!({ "connection": connection, "pages": MAX_PAGES, "nodes": nodes.len() }),
            );
        }
        after = Some(cursor);
    }
    Ok(nodes)
}

fn connection_nodes<'a>(data: &'a Value, key: &str) -> Vec<&'a Value> {
    data.get(key)
        .and_then(|connection| connection.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| nodes.iter().filter(|node| node.is_object()).collect())
        .unwrap_or_default()
}

/// The JSON configuration stored in the instance's metafield, or `{}`.
fn metafield_config(node: &Value) -> Value {
    node.get("metafield")
        .and_then(|metafield| metafield.get("value"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_else(|| json!({}))
}

async fn list_cart_transform_instances(
    shopify: &ShopifyClient,
    handle: &str,
    label: &str,
) -> anyhow::Result<Vec<Value>> {
    let functions_query = "#graphql
    query ListShopifyFunctions($after: String) {
      shopifyFunctions(first: 50, after: $after) {
        nodes { id handle }
        pageInfo { hasNextPage endCursor }
      }
    }";
    let functions = connection_pages(shopify, functions_query, JsonMap::new(), "shopifyFunctions").await?;
    let handles: HashMap<&str, &str> = functions
        .iter()
        .filter_map(|function| Some((function.get("id")?.as_str()?, function.get("handle")?.as_str()?)))
        .collect();

    let query = format!(
        "#graphql
    query ListCartTransformInstances($metafieldKey: String!, $after: String) {{
      cartTransforms(first: 50, after: $after) {{
        nodes {{
          id functionId blockOnFailure
          metafield(namespace: \"{METAFIELD_NAMESPACE}\", key: $metafieldKey) {{ value }}
        }}
        pageInfo {{ hasNextPage endCursor }}
      }}
    }}"
    );
    let variables = JsonMap::from_iter([("metafieldKey".to_string(), json!(handle))]);
    let transforms = connection_pages(shopify, &query, variables, "cartTransforms").await?;

    Ok(transforms
        .iter()
        .filter(|node| {
            node.get("functionId")
                .and_then(Value::as_str)
                .and_then(|id| handles.get(id))
                .is_some_and(|function_handle| *function_handle == handle)
        })
        .map(|node| {
            json!({
                "id": node.get("id"),
                "label": label,
                "state": "active",
                "blockOnFailure": node.get("blockOnFailure") == Some(&Value::Bool(true)),
                "config": metafield_config(node),
            })
        })
        .collect())
}

/// List all function definitions
async fn list_functions(State(app): State<OpenShop>) -> Response {
    let functions: Vec<Value> = app_functions(app.config())
        .into_iter()
        .map(|function| {
            let definition = function.definition;
            let fields: JsonMap = definition
                .config
                .iter()
                .map(|(name, field)| (name.clone(), Value::Object(field.metadata())))
                .collect();

            let mut item = JsonMap::new();
            item.insert("key".into(), Value::from(function.key));
            item.insert("label".into(), Value::from(function.label.clone()));
            item.insert("type".into(), json!(definition.function_type));
            item.insert("handle".into(), Value::from(definition.handle.clone()));
            if let Some(modes) = function.modes() {
                item.insert("modes".into(), json!(modes));
            }
            item.insert("supportsUpdate".into(), Value::Bool(function.supports_update()));
            item.insert("singleton".into(), Value::Bool(function.is_singleton()));
            item.insert("fields".into(), Value::Object(fields));
            Value::Object(item)
        })
        .collect();
    json_response(StatusCode::OK, Value::Array(functions))
}

fn user_errors_response(errors: &[Value], status: StatusCode) -> Response {
    json_response(
        status,
        json!({ "error": user_error_messages(errors), "userErrors": errors }),
    )
}

/// List instances of a function (queries Shopify live)
async fn list_instances(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(handle): Path<String>,
) -> ApiResult {
    let Some(function) = find_function(app.config(), &handle) else {
        return Ok(json_error(StatusCode::NOT_FOUND, "Function not found"));
    };
    let instances = function_instances(&app, &session, &function).await?;
    Ok(json_response(StatusCode::OK, Value::Array(instances)))
}

/// The instances of a function in the shop of the session, read from Shopify
/// page after page: `id`, its `config`, and what the type of the function
/// says of it (`title`, `status` and dates of a discount, `title` and
/// `enabled` of a customization, `label`, `state` and `blockOnFailure` of a
/// Cart Transform).
pub(crate) async fn function_instances(
    app: &OpenShop,
    session: &ShopSession,
    function: &AppFunction<'_>,
) -> anyhow::Result<Vec<Value>> {
    let definition = function.definition;
    let handle = function.handle();
    let mutations = get_mutations(definition, None);
    let shopify = session.shopify_client(app);

    if definition.function_type == FunctionType::CartTransform {
        return list_cart_transform_instances(&shopify, handle, &function.label).await;
    }

    if definition.function_type == FunctionType::Discount {
        let query = format!(
            "#graphql
        query ListDiscountInstances($query: String!, $after: String) {{
          discountNodes(first: 50, after: $after, query: $query) {{
            nodes {{
              id
              discount {{
                ... on DiscountAutomaticApp {{
                  title status startsAt endsAt
                  combinesWith {{ productDiscounts orderDiscounts shippingDiscounts }}
                }}
                ... on DiscountCodeApp {{
                  title status startsAt endsAt usageLimit
                  combinesWith {{ productDiscounts orderDiscounts shippingDiscounts }}
                  codes(first: 1) {{ nodes {{ code }} }}
                }}
              }}
              metafield(namespace: \"{METAFIELD_NAMESPACE}\", key: \"{}\") {{ value }}
            }}
            pageInfo {{ hasNextPage endCursor }}
          }}
        }}",
            definition.handle
        );
        let variables = JsonMap::from_iter([("query".to_string(), json!(format!("function_handle:{handle}")))]);
        let nodes = connection_pages(&shopify, &query, variables, "discountNodes").await?;
        let instances: Vec<Value> = nodes
            .iter()
            .map(|node| {
                let discount = node.get("discount");
                let field = |name: &str| discount.and_then(|discount| discount.get(name));
                let mut instance = JsonMap::new();
                instance.insert("id".into(), json!(node.get("id")));
                for name in ["title", "status", "startsAt", "endsAt"] {
                    if let Some(value) = field(name) {
                        instance.insert(name.into(), value.clone());
                    }
                }
                instance.insert("config".into(), metafield_config(node));
                Value::Object(instance)
            })
            .collect();
        return Ok(instances);
    }

    // Generic list for non-discount types
    let query = format!(
        "#graphql
      query ListInstances($after: String) {{
        {}(first: 50, after: $after) {{
          nodes {{
            id
            title enabled
            metafield(namespace: \"{METAFIELD_NAMESPACE}\", key: \"{handle}\") {{ value }}
          }}
          pageInfo {{ hasNextPage endCursor }}
        }}
      }}",
        mutations.list
    );
    let nodes = connection_pages(&shopify, &query, JsonMap::new(), mutations.list).await?;
    Ok(nodes
        .iter()
        .map(|node| {
            let mut instance = JsonMap::new();
            instance.insert("id".into(), json!(node.get("id")));
            for name in ["title", "enabled"] {
                if let Some(value) = node.get(name) {
                    instance.insert(name.into(), value.clone());
                }
            }
            instance.insert("config".into(), metafield_config(node));
            Value::Object(instance)
        })
        .collect())
}

/// Create a function instance
async fn create_instance(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(handle): Path<String>,
    body: Bytes,
) -> ApiResult {
    let Some(function) = find_function(app.config(), &handle) else {
        return Ok(json_error(StatusCode::NOT_FOUND, "Function not found"));
    };
    let body = json_object_body(&body);
    Ok(match create_function_instance(&app, &session, &function, &body).await? {
        Ok(data) => json_response(StatusCode::CREATED, json!({ "ok": true, "data": data })),
        Err(refusal) => refusal.response(),
    })
}

/// What a Cart Transform that has an instance answers to a second one.
pub(crate) const ALREADY_REGISTERED: &str = "This Cart Transform already has an instance";

/// A change of an instance refused, by the app or by Shopify: what the API
/// answers, and what the page of the admin shows.
#[derive(Debug)]
pub(crate) enum InstanceRefusal {
    /// The configuration sent is not one of the function: `400`.
    InvalidConfig(ConfigError),
    /// A second instance of a Cart Transform: `409`, with Shopify's
    /// `userErrors` when Shopify is the one that said so.
    AlreadyRegistered(Option<Vec<Value>>),
    /// An update of a type whose instances are deleted and created again: `400`.
    NoUpdate(FunctionType),
    /// A discount of several modes deleted without its mode: `400`.
    ModeRequired,
    /// The `userErrors` of the mutation: `400`.
    UserErrors(Vec<Value>),
}

impl InstanceRefusal {
    /// What the API says of it, in its `error`.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::InvalidConfig(error) => error.message.clone(),
            Self::AlreadyRegistered(_) => ALREADY_REGISTERED.to_string(),
            Self::NoUpdate(kind) => format!(
                "Function type \"{}\" does not support update. Delete and recreate instead.",
                kind.as_str()
            ),
            Self::ModeRequired => "mode query parameter is required for discount functions with multiple modes".to_string(),
            Self::UserErrors(errors) => user_error_messages(errors),
        }
    }

    fn response(self) -> Response {
        let message = self.message();
        match self {
            Self::AlreadyRegistered(Some(errors)) => json_response(
                StatusCode::CONFLICT,
                json!({ "error": message, "userErrors": errors }),
            ),
            Self::AlreadyRegistered(None) => json_error(StatusCode::CONFLICT, message),
            Self::UserErrors(errors) => user_errors_response(&errors, StatusCode::BAD_REQUEST),
            Self::InvalidConfig(_) | Self::NoUpdate(_) | Self::ModeRequired => {
                json_error(StatusCode::BAD_REQUEST, message)
            }
        }
    }
}

/// Creates an instance of `function` in the shop of the session from `body`,
/// what the form of the admin sends: its `config`, the `mode` of a discount,
/// and what the type of the function reads (`blockOnFailure`, `startsAt`,
/// `code`, …). The payload of Shopify's mutation, or why it was refused.
pub(crate) async fn create_function_instance(
    app: &OpenShop,
    session: &ShopSession,
    function: &AppFunction<'_>,
    body: &JsonMap,
) -> anyhow::Result<Result<Option<Value>, InstanceRefusal>> {
    let definition = function.definition;
    let handle = function.handle();
    let function_config = match parse_provider_config(&definition.config, None, body.get("config"), &JsonMap::new()) {
        Ok(config) => config,
        Err(error) => return Ok(Err(InstanceRefusal::InvalidConfig(error))),
    };
    let mode = discount_mode_from_body(body, definition.first_mode());
    let shopify = session.shopify_client(app);
    let is_cart_transform = definition.function_type == FunctionType::CartTransform;

    if is_cart_transform
        && !list_cart_transform_instances(&shopify, handle, &function.label)
            .await?
            .is_empty()
    {
        return Ok(Err(InstanceRefusal::AlreadyRegistered(None)));
    }

    let title = resolve_title(definition.owner.as_ref(), &function_config);
    let request = build_create_mutation(definition, handle, mode, &title, &function_config, body);

    let result = shopify.graphql(&request.mutation, Some(request.variables)).await?;
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        if is_cart_transform && has_user_error_code(&errors, "FUNCTION_ALREADY_REGISTERED") {
            return Ok(Err(InstanceRefusal::AlreadyRegistered(Some(errors))));
        }
        return Ok(Err(InstanceRefusal::UserErrors(errors)));
    }
    Ok(Ok(extract_payload(&result).cloned()))
}

/// Update a function instance
async fn update_instance(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path((handle, id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let Some(function) = find_function(app.config(), &handle) else {
        return Ok(json_error(StatusCode::NOT_FOUND, "Function not found"));
    };
    let body = json_object_body(&body);
    Ok(match update_function_instance(&app, &session, &function, &id, &body).await? {
        Ok(()) => json_response(StatusCode::OK, json!({ "ok": true })),
        Err(refusal) => refusal.response(),
    })
}

/// Updates the instance `id` of `function` in the shop of the session from
/// `body`, as [`create_function_instance`] reads it; refused for a type whose
/// instances are deleted and created again.
pub(crate) async fn update_function_instance(
    app: &OpenShop,
    session: &ShopSession,
    function: &AppFunction<'_>,
    id: &str,
    body: &JsonMap,
) -> anyhow::Result<Result<(), InstanceRefusal>> {
    let definition = function.definition;
    let mode = discount_mode_from_body(body, definition.first_mode());
    if get_mutations(definition, Some(mode)).update.is_none() {
        return Ok(Err(InstanceRefusal::NoUpdate(definition.function_type)));
    }

    let function_config = match parse_provider_config(&definition.config, None, body.get("config"), &JsonMap::new()) {
        Ok(config) => config,
        Err(error) => return Ok(Err(InstanceRefusal::InvalidConfig(error))),
    };
    let shopify = session.shopify_client(app);

    let title = resolve_title(definition.owner.as_ref(), &function_config);
    let request = build_update_mutation(definition, function.handle(), mode, id, &title, &function_config, body);

    let result = shopify.graphql(&request.mutation, Some(request.variables)).await?;
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        return Ok(Err(InstanceRefusal::UserErrors(errors)));
    }
    Ok(Ok(()))
}

/// Delete a function instance
async fn delete_instance(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path((handle, id)): Path<(String, String)>,
    RawQuery(raw_query): RawQuery,
) -> ApiResult {
    let Some(function) = find_function(app.config(), &handle) else {
        return Ok(json_error(StatusCode::NOT_FOUND, "Function not found"));
    };
    let query = parse_query(raw_query.as_deref());
    let mode = query.get("mode").map(String::as_str);
    Ok(match delete_function_instance(&app, &session, &function, &id, mode).await? {
        Ok(()) => json_response(StatusCode::OK, json!({ "ok": true })),
        Err(refusal) => refusal.response(),
    })
}

/// Deletes the instance `id` of `function` in the shop of the session. `mode`
/// says which kind of discount it is, required of a discount that has several.
pub(crate) async fn delete_function_instance(
    app: &OpenShop,
    session: &ShopSession,
    function: &AppFunction<'_>,
    id: &str,
    mode: Option<&str>,
) -> anyhow::Result<Result<(), InstanceRefusal>> {
    let definition = function.definition;
    let requested_mode = mode.and_then(DiscountMode::parse);
    let has_several_modes = definition.modes.as_ref().is_some_and(|modes| modes.len() > 1);
    if definition.function_type == FunctionType::Discount && has_several_modes && requested_mode.is_none() {
        return Ok(Err(InstanceRefusal::ModeRequired));
    }
    let mode = requested_mode.unwrap_or_else(|| definition.first_mode());
    let mutations = get_mutations(definition, Some(mode));

    let shopify = session.shopify_client(app);
    let request = build_delete_mutation(mutations.delete, id);
    let result = shopify.graphql(&request.mutation, Some(request.variables)).await?;
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        return Ok(Err(InstanceRefusal::UserErrors(errors)));
    }
    Ok(Ok(()))
}

pub fn create_function_routes() -> Router<OpenShop> {
    Router::new()
        .route("/api/functions", get(list_functions))
        .route(
            "/api/functions/{handle}/instances",
            get(list_instances).post(create_instance),
        )
        .route(
            "/api/functions/{handle}/instances/{id}",
            put(update_instance).delete(delete_instance),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanizes_keys_and_handles() {
        assert_eq!(humanize("volumeDiscount"), "Volume Discount");
        assert_eq!(humanize("cart-transform"), "Cart Transform");
        assert_eq!(humanize("free_shipping__rule"), "Free Shipping Rule");
        assert_eq!(humanize("  b2bPricing2Go "), "B2b Pricing2 Go");
        assert_eq!(humanize(""), "");
    }

    #[test]
    fn labels_prefer_explicit_label_then_key_then_handle() {
        let definition = Function::new(FunctionType::CartTransform, "bundle-builder");
        assert_eq!(function_label("bundles", &definition), "Bundles");
        assert_eq!(function_label("", &definition), "Bundle Builder");
        assert_eq!(
            function_label("bundles", &definition.label("  Bundle maker ")),
            "Bundle maker"
        );
    }

    #[test]
    fn reads_metafield_configuration() {
        assert_eq!(
            metafield_config(&json!({ "metafield": { "value": "{\"a\":1}" } })),
            json!({ "a": 1 })
        );
        assert_eq!(metafield_config(&json!({ "metafield": null })), json!({}));
        assert_eq!(metafield_config(&json!({ "metafield": { "value": "" } })), json!({}));
    }
}
