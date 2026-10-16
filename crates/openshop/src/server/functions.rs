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

use super::function_config::validate_function_config;
use super::function_mutations::{
    METAFIELD_NAMESPACE, build_create_mutation, build_delete_mutation, build_update_mutation, extract_errors,
    extract_payload, get_mutations, has_user_error_code, resolve_title, user_error_messages,
};
use super::http::{ApiResult, json_error, json_object_body, json_response, parse_query};
use super::shop::ShopSession;
use crate::app::{OpenShop, OpenShopConfig};
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

fn find_function<'a>(config: &'a OpenShopConfig, handle: &str) -> Option<(&'a Function, String)> {
    config
        .functions
        .iter()
        .find(|(_, definition)| definition.handle == handle)
        .map(|(key, definition)| (definition, function_label(key, definition)))
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
    let query = format!(
        "#graphql
    query ListCartTransformInstances($metafieldKey: String!) {{
      shopifyFunctions(first: 100) {{ nodes {{ id handle }} }}
      cartTransforms(first: 50) {{
        nodes {{
          id functionId blockOnFailure
          metafield(namespace: \"{METAFIELD_NAMESPACE}\", key: $metafieldKey) {{ value }}
        }}
      }}
    }}"
    );
    let data = shopify.graphql(&query, Some(json!({ "metafieldKey": handle }))).await?;
    let handles: HashMap<&str, &str> = connection_nodes(&data, "shopifyFunctions")
        .into_iter()
        .filter_map(|function| Some((function.get("id")?.as_str()?, function.get("handle")?.as_str()?)))
        .collect();

    Ok(connection_nodes(&data, "cartTransforms")
        .into_iter()
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
    let functions: Vec<Value> = app
        .config()
        .functions
        .iter()
        .map(|(key, definition)| {
            let fields: JsonMap = definition
                .config
                .iter()
                .map(|(name, field)| (name.clone(), Value::Object(field.metadata())))
                .collect();
            let is_discount = definition.function_type == FunctionType::Discount;

            let mut item = JsonMap::new();
            item.insert("key".into(), Value::from(key.clone()));
            item.insert("label".into(), Value::from(function_label(key, definition)));
            item.insert("type".into(), json!(definition.function_type));
            item.insert("handle".into(), Value::from(definition.handle.clone()));
            if is_discount {
                let modes = definition
                    .modes
                    .clone()
                    .unwrap_or_else(|| vec![DiscountMode::Automatic]);
                item.insert("modes".into(), json!(modes));
            }
            item.insert(
                "supportsUpdate".into(),
                Value::Bool(!matches!(
                    definition.function_type,
                    FunctionType::CartTransform | FunctionType::FulfillmentConstraints
                )),
            );
            item.insert(
                "singleton".into(),
                Value::Bool(definition.function_type == FunctionType::CartTransform),
            );
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
    let Some((definition, label)) = find_function(app.config(), &handle) else {
        return Ok(json_error(StatusCode::NOT_FOUND, "Function not found"));
    };
    let Some(mutations) = get_mutations(definition, None) else {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            format!(
                "Function type \"{}\" has no GraphQL API",
                definition.function_type.as_str()
            ),
        ));
    };
    let shopify = session.shopify_client(&app);

    if definition.function_type == FunctionType::CartTransform {
        let instances = list_cart_transform_instances(&shopify, &handle, &label).await?;
        return Ok(json_response(StatusCode::OK, Value::Array(instances)));
    }

    if definition.function_type == FunctionType::Discount {
        let query = format!(
            "#graphql
        query ListDiscountInstances($query: String!) {{
          discountNodes(first: 50, query: $query) {{
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
          }}
        }}",
            definition.handle
        );
        let data = shopify
            .graphql(&query, Some(json!({ "query": format!("function_handle:{handle}") })))
            .await?;
        let instances: Vec<Value> = connection_nodes(&data, "discountNodes")
            .into_iter()
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
        return Ok(json_response(StatusCode::OK, Value::Array(instances)));
    }

    // Generic list for non-discount types
    let query = format!(
        "#graphql
      query ListInstances {{
        {}(first: 50) {{
          nodes {{
            id
            title enabled
            metafield(namespace: \"{METAFIELD_NAMESPACE}\", key: \"{handle}\") {{ value }}
          }}
        }}
      }}",
        mutations.list
    );
    let data = shopify.graphql(&query, None).await?;
    let instances: Vec<Value> = connection_nodes(&data, mutations.list)
        .into_iter()
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
        .collect();
    Ok(json_response(StatusCode::OK, Value::Array(instances)))
}

/// Create a function instance
async fn create_instance(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(handle): Path<String>,
    body: Bytes,
) -> ApiResult {
    let Some((definition, label)) = find_function(app.config(), &handle) else {
        return Ok(json_error(StatusCode::NOT_FOUND, "Function not found"));
    };
    if get_mutations(definition, None).is_none() {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            format!(
                "Function type \"{}\" has no GraphQL API",
                definition.function_type.as_str()
            ),
        ));
    }

    let body = json_object_body(&body);
    let function_config = match validate_function_config(&definition.config, body.get("config")) {
        Ok(config) => config,
        Err(error) => return Ok(json_error(StatusCode::BAD_REQUEST, error)),
    };
    let mode = discount_mode_from_body(&body, definition.first_mode());
    let shopify = session.shopify_client(&app);
    let is_cart_transform = definition.function_type == FunctionType::CartTransform;
    let already_registered = "This Cart Transform already has an instance";

    if is_cart_transform
        && !list_cart_transform_instances(&shopify, &handle, &label)
            .await?
            .is_empty()
    {
        return Ok(json_error(StatusCode::CONFLICT, already_registered));
    }

    let title = resolve_title(definition.owner.as_ref(), &function_config);
    let Some(request) = build_create_mutation(definition, &handle, mode, &title, &function_config, &body) else {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            format!("Unsupported function type: {}", definition.function_type.as_str()),
        ));
    };

    let result = shopify.graphql(&request.mutation, Some(request.variables)).await?;
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        if is_cart_transform && has_user_error_code(&errors, "FUNCTION_ALREADY_REGISTERED") {
            return Ok(json_response(
                StatusCode::CONFLICT,
                json!({ "error": already_registered, "userErrors": errors }),
            ));
        }
        return Ok(user_errors_response(&errors, StatusCode::BAD_REQUEST));
    }

    Ok(json_response(
        StatusCode::CREATED,
        json!({ "ok": true, "data": extract_payload(&result) }),
    ))
}

fn definition_by_handle<'a>(config: &'a OpenShopConfig, handle: &str) -> Option<&'a Function> {
    config.functions.values().find(|definition| definition.handle == handle)
}

/// Update a function instance
async fn update_instance(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path((handle, id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let Some(definition) = definition_by_handle(app.config(), &handle) else {
        return Ok(json_error(StatusCode::NOT_FOUND, "Function not found"));
    };

    let body = json_object_body(&body);
    let mode = discount_mode_from_body(&body, definition.first_mode());
    if get_mutations(definition, Some(mode))
        .and_then(|mutations| mutations.update)
        .is_none()
    {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            format!(
                "Function type \"{}\" does not support update. Delete and recreate instead.",
                definition.function_type.as_str()
            ),
        ));
    }

    let function_config = match validate_function_config(&definition.config, body.get("config")) {
        Ok(config) => config,
        Err(error) => return Ok(json_error(StatusCode::BAD_REQUEST, error)),
    };
    let shopify = session.shopify_client(&app);

    let title = resolve_title(definition.owner.as_ref(), &function_config);
    let Some(request) = build_update_mutation(definition, &handle, mode, &id, &title, &function_config, &body) else {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            format!(
                "Unsupported function type for update: {}",
                definition.function_type.as_str()
            ),
        ));
    };

    let result = shopify.graphql(&request.mutation, Some(request.variables)).await?;
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        return Ok(user_errors_response(&errors, StatusCode::BAD_REQUEST));
    }
    Ok(json_response(StatusCode::OK, json!({ "ok": true })))
}

/// Delete a function instance
async fn delete_instance(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path((handle, id)): Path<(String, String)>,
    RawQuery(raw_query): RawQuery,
) -> ApiResult {
    let Some(definition) = definition_by_handle(app.config(), &handle) else {
        return Ok(json_error(StatusCode::NOT_FOUND, "Function not found"));
    };

    let query = parse_query(raw_query.as_deref());
    let requested_mode = query.get("mode").and_then(|mode| DiscountMode::parse(mode));
    let has_several_modes = definition.modes.as_ref().is_some_and(|modes| modes.len() > 1);
    if definition.function_type == FunctionType::Discount && has_several_modes && requested_mode.is_none() {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            "mode query parameter is required for discount functions with multiple modes",
        ));
    }
    let mode = requested_mode.unwrap_or_else(|| definition.first_mode());
    let Some(mutations) = get_mutations(definition, Some(mode)) else {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            format!(
                "Function type \"{}\" has no GraphQL API",
                definition.function_type.as_str()
            ),
        ));
    };

    let shopify = session.shopify_client(&app);
    let request = build_delete_mutation(mutations.delete, &id);
    let result = shopify.graphql(&request.mutation, Some(request.variables)).await?;
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        return Ok(user_errors_response(&errors, StatusCode::BAD_REQUEST));
    }
    Ok(json_response(StatusCode::OK, json!({ "ok": true })))
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
