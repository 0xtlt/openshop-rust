//! Shopify Admin GraphQL mutations that manage Shopify Function instances.

use chrono::Utc;
use serde_json::{Value, json};

use crate::runtime::time::to_iso;
use crate::types::{DiscountMode, FunctionDefinition, FunctionOwner, FunctionTitle, JsonMap, ShopifyFunctionType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputStyle {
    Wrapped,
    Flat,
}

/// Names of the GraphQL fields for one kind of function owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationSet {
    pub list: &'static str,
    pub create: &'static str,
    pub update: Option<&'static str>,
    pub delete: &'static str,
    pub input_style: InputStyle,
}

pub fn discount_mutations(mode: DiscountMode) -> MutationSet {
    match mode {
        DiscountMode::Automatic => MutationSet {
            list: "discountNodes",
            create: "discountAutomaticAppCreate",
            update: Some("discountAutomaticAppUpdate"),
            delete: "discountAutomaticDelete",
            input_style: InputStyle::Wrapped,
        },
        DiscountMode::Code => MutationSet {
            list: "discountNodes",
            create: "discountCodeAppCreate",
            update: Some("discountCodeAppUpdate"),
            delete: "discountCodeDelete",
            input_style: InputStyle::Wrapped,
        },
    }
}

pub fn function_mutations(function_type: ShopifyFunctionType) -> Option<MutationSet> {
    Some(match function_type {
        ShopifyFunctionType::Discount => return None,
        ShopifyFunctionType::CartTransform => MutationSet {
            list: "cartTransforms",
            create: "cartTransformCreate",
            update: None,
            delete: "cartTransformDelete",
            input_style: InputStyle::Flat,
        },
        ShopifyFunctionType::DeliveryCustomization => MutationSet {
            list: "deliveryCustomizations",
            create: "deliveryCustomizationCreate",
            update: Some("deliveryCustomizationUpdate"),
            delete: "deliveryCustomizationDelete",
            input_style: InputStyle::Wrapped,
        },
        ShopifyFunctionType::PaymentCustomization => MutationSet {
            list: "paymentCustomizations",
            create: "paymentCustomizationCreate",
            update: Some("paymentCustomizationUpdate"),
            delete: "paymentCustomizationDelete",
            input_style: InputStyle::Wrapped,
        },
        ShopifyFunctionType::CheckoutValidation => MutationSet {
            list: "validations",
            create: "validationCreate",
            update: Some("validationUpdate"),
            delete: "validationDelete",
            input_style: InputStyle::Wrapped,
        },
        ShopifyFunctionType::FulfillmentConstraints => MutationSet {
            list: "fulfillmentConstraintRules",
            create: "fulfillmentConstraintRuleCreate",
            update: None,
            delete: "fulfillmentConstraintRuleDelete",
            input_style: InputStyle::Flat,
        },
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct MutationRequest {
    pub mutation: String,
    pub variables: Value,
}

pub const METAFIELD_NAMESPACE: &str = "$app:openshop";

/// For discounts the set depends on the mode (default: the function's first mode).
pub fn get_mutations(definition: &FunctionDefinition, mode: Option<DiscountMode>) -> Option<MutationSet> {
    if definition.function_type == ShopifyFunctionType::Discount {
        return Some(discount_mutations(mode.unwrap_or_else(|| definition.first_mode())));
    }
    function_mutations(definition.function_type)
}

/// The payload of the single mutation in a response (ignoring `extensions`).
pub fn extract_payload(data: &Value) -> Option<&Value> {
    data.as_object()?
        .iter()
        .find(|(key, _)| key.as_str() != "extensions")
        .map(|(_, payload)| payload)
}

/// The `userErrors` of the mutation payload, as Shopify returned them.
pub fn extract_errors(data: &Value) -> Vec<Value> {
    extract_payload(data)
        .and_then(|payload| payload.get("userErrors"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The messages of user errors, joined for display.
pub fn user_error_messages(errors: &[Value]) -> String {
    errors
        .iter()
        .map(|error| error.get("message").and_then(Value::as_str).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn has_user_error_code(errors: &[Value], code: &str) -> bool {
    errors
        .iter()
        .any(|error| error.get("code").and_then(Value::as_str) == Some(code))
}

pub fn resolve_title(owner: Option<&FunctionOwner>, config: &JsonMap) -> String {
    match owner.map(|owner| &owner.title) {
        None => "Untitled".to_string(),
        Some(FunctionTitle::Static(title)) => title.clone(),
        Some(FunctionTitle::Dynamic(title)) => title(config),
    }
}

pub fn build_metafield(handle: &str, config: &JsonMap) -> Value {
    json!({
        "namespace": METAFIELD_NAMESPACE,
        "key": handle,
        "type": "json",
        "value": Value::Object(config.clone()).to_string(),
    })
}

fn combines_with(owner: Option<&FunctionOwner>) -> Value {
    owner
        .and_then(|owner| owner.combines_with)
        .map(|combines| json!(combines))
        .unwrap_or_else(|| json!({}))
}

fn body_or(body: &JsonMap, key: &str, fallback: Value) -> Value {
    match body.get(key) {
        Some(Value::Null) | None => fallback,
        Some(value) => value.clone(),
    }
}

/// Inserts only the keys the request provided, like `JSON.stringify` dropping `undefined`.
fn insert_defined(target: &mut JsonMap, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        target.insert(key.to_string(), value.clone());
    }
}

pub fn build_create_mutation(
    definition: &FunctionDefinition,
    handle: &str,
    mode: DiscountMode,
    title: &str,
    config: &JsonMap,
    body: &JsonMap,
) -> Option<MutationRequest> {
    let metafield = build_metafield(handle, config);
    let now = || Value::from(to_iso(&Utc::now()));

    match definition.function_type {
        ShopifyFunctionType::Discount if mode == DiscountMode::Automatic => Some(MutationRequest {
            mutation: "#graphql
          mutation CreateAutoDiscount($input: DiscountAutomaticAppInput!) {
            discountAutomaticAppCreate(automaticAppDiscount: $input) {
              automaticAppDiscount { discountId }
              userErrors { field message }
            }
          }"
            .to_string(),
            variables: json!({
                "input": {
                    "functionHandle": handle,
                    "title": title,
                    "startsAt": body_or(body, "startsAt", now()),
                    "endsAt": body_or(body, "endsAt", Value::Null),
                    "combinesWith": combines_with(definition.owner.as_ref()),
                    "metafields": [metafield],
                },
            }),
        }),

        ShopifyFunctionType::Discount => {
            let mut input = JsonMap::new();
            input.insert("functionHandle".into(), Value::from(handle));
            input.insert("title".into(), Value::from(title));
            insert_defined(&mut input, "code", body.get("code"));
            input.insert("startsAt".into(), body_or(body, "startsAt", now()));
            input.insert("endsAt".into(), body_or(body, "endsAt", Value::Null));
            input.insert("usageLimit".into(), body_or(body, "usageLimit", Value::Null));
            input.insert("combinesWith".into(), combines_with(definition.owner.as_ref()));
            input.insert("metafields".into(), json!([metafield]));
            Some(MutationRequest {
                mutation: "#graphql
        mutation CreateCodeDiscount($input: DiscountCodeAppInput!) {
          discountCodeAppCreate(codeAppDiscount: $input) {
            codeAppDiscount { discountId }
            userErrors { field message }
          }
        }"
                .to_string(),
                variables: json!({ "input": input }),
            })
        }

        ShopifyFunctionType::CartTransform => Some(MutationRequest {
            mutation: "#graphql
        mutation CreateCartTransform($functionHandle: String!, $blockOnFailure: Boolean, $metafields: [MetafieldInput!]) {
          cartTransformCreate(functionHandle: $functionHandle, blockOnFailure: $blockOnFailure, metafields: $metafields) {
            cartTransform { id }
            userErrors { field message code }
          }
        }"
            .to_string(),
            variables: json!({
                "functionHandle": handle,
                "blockOnFailure": body_or(body, "blockOnFailure", Value::Bool(false)),
                "metafields": [metafield],
            }),
        }),

        ShopifyFunctionType::FulfillmentConstraints => Some(MutationRequest {
            mutation: "#graphql
        mutation CreateFulfillmentConstraint($functionHandle: String!, $deliveryMethodTypes: [DeliveryMethodType!]!, $metafields: [MetafieldInput!]) {
          fulfillmentConstraintRuleCreate(functionHandle: $functionHandle, deliveryMethodTypes: $deliveryMethodTypes, metafields: $metafields) {
            fulfillmentConstraintRule { id }
            userErrors { field message }
          }
        }"
            .to_string(),
            variables: json!({
                "functionHandle": handle,
                "deliveryMethodTypes": body_or(body, "deliveryMethodTypes", json!(["SHIPPING"])),
                "metafields": [metafield],
            }),
        }),

        ShopifyFunctionType::DeliveryCustomization => Some(MutationRequest {
            mutation: "#graphql
        mutation CreateDeliveryCustomization($input: DeliveryCustomizationInput!) {
          deliveryCustomizationCreate(deliveryCustomization: $input) {
            deliveryCustomization { id }
            userErrors { field message }
          }
        }"
            .to_string(),
            variables: json!({
                "input": { "functionHandle": handle, "title": title, "enabled": true, "metafields": [metafield] },
            }),
        }),

        ShopifyFunctionType::PaymentCustomization => Some(MutationRequest {
            mutation: "#graphql
        mutation CreatePaymentCustomization($input: PaymentCustomizationInput!) {
          paymentCustomizationCreate(paymentCustomization: $input) {
            paymentCustomization { id }
            userErrors { field message }
          }
        }"
            .to_string(),
            variables: json!({
                "input": { "functionHandle": handle, "title": title, "enabled": true, "metafields": [metafield] },
            }),
        }),

        ShopifyFunctionType::CheckoutValidation => Some(MutationRequest {
            mutation: "#graphql
        mutation CreateValidation($input: ValidationCreateInput!) {
          validationCreate(validation: $input) {
            validation { id }
            userErrors { field message }
          }
        }"
            .to_string(),
            variables: json!({
                "input": {
                    "functionHandle": handle,
                    "title": title,
                    "enable": true,
                    "blockOnFailure": body_or(body, "blockOnFailure", Value::Bool(false)),
                    "metafields": [metafield],
                },
            }),
        }),
    }
}

pub fn build_update_mutation(
    definition: &FunctionDefinition,
    handle: &str,
    mode: DiscountMode,
    id: &str,
    title: &str,
    config: &JsonMap,
    body: &JsonMap,
) -> Option<MutationRequest> {
    let metafield = build_metafield(handle, config);
    let combines = definition
        .owner
        .as_ref()
        .and_then(|owner| owner.combines_with)
        .map(|combines| json!(combines));

    let mut input = JsonMap::new();
    input.insert("title".into(), Value::from(title));

    let (mutation, input) = match definition.function_type {
        ShopifyFunctionType::Discount => {
            insert_defined(&mut input, "startsAt", body.get("startsAt"));
            insert_defined(&mut input, "endsAt", body.get("endsAt"));
            if mode == DiscountMode::Code {
                insert_defined(&mut input, "usageLimit", body.get("usageLimit"));
            }
            insert_defined(&mut input, "combinesWith", combines.as_ref());
            let mutation = if mode == DiscountMode::Automatic {
                "#graphql
        mutation UpdateAutoDiscount($id: ID!, $input: DiscountAutomaticAppInput!) {
          discountAutomaticAppUpdate(id: $id, automaticAppDiscount: $input) {
            userErrors { field message }
          }
        }"
            } else {
                "#graphql
        mutation UpdateCodeDiscount($id: ID!, $input: DiscountCodeAppInput!) {
          discountCodeAppUpdate(id: $id, codeAppDiscount: $input) {
            userErrors { field message }
          }
        }"
            };
            (mutation, input)
        }
        ShopifyFunctionType::DeliveryCustomization => {
            insert_defined(&mut input, "enabled", body.get("enabled"));
            (
                "#graphql
        mutation UpdateDeliveryCustomization($id: ID!, $input: DeliveryCustomizationInput!) {
          deliveryCustomizationUpdate(id: $id, deliveryCustomization: $input) {
            userErrors { field message }
          }
        }",
                input,
            )
        }
        ShopifyFunctionType::PaymentCustomization => {
            insert_defined(&mut input, "enabled", body.get("enabled"));
            (
                "#graphql
        mutation UpdatePaymentCustomization($id: ID!, $input: PaymentCustomizationInput!) {
          paymentCustomizationUpdate(id: $id, paymentCustomization: $input) {
            userErrors { field message }
          }
        }",
                input,
            )
        }
        ShopifyFunctionType::CheckoutValidation => {
            insert_defined(&mut input, "enable", body.get("enabled"));
            insert_defined(&mut input, "blockOnFailure", body.get("blockOnFailure"));
            (
                "#graphql
        mutation UpdateValidation($id: ID!, $input: ValidationUpdateInput!) {
          validationUpdate(id: $id, validation: $input) {
            userErrors { field message }
          }
        }",
                input,
            )
        }
        ShopifyFunctionType::CartTransform | ShopifyFunctionType::FulfillmentConstraints => return None,
    };

    let mut input = input;
    input.insert("metafields".into(), json!([metafield]));
    Some(MutationRequest {
        mutation: mutation.to_string(),
        variables: json!({ "id": id, "input": input }),
    })
}

pub fn build_delete_mutation(mutation_name: &str, id: &str) -> MutationRequest {
    MutationRequest {
        mutation: format!(
            "#graphql
      mutation DeleteFunctionInstance($id: ID!) {{
        {mutation_name}(id: $id) {{
          userErrors {{ field message }}
        }}
      }}"
        ),
        variables: json!({ "id": id }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CombinesWith;

    fn object(value: Value) -> JsonMap {
        value.as_object().cloned().unwrap()
    }

    fn discount() -> FunctionDefinition {
        FunctionDefinition::new(ShopifyFunctionType::Discount, "volume-discount")
            .modes([DiscountMode::Automatic, DiscountMode::Code])
            .owner(
                FunctionOwner::dynamic_title(|config| format!("{}% off", config["percentage"])).combines_with(
                    CombinesWith {
                        product_discounts: Some(true),
                        order_discounts: Some(false),
                        shipping_discounts: Some(true),
                    },
                ),
            )
    }

    #[test]
    fn picks_mutation_sets_by_type_and_mode() {
        assert_eq!(
            get_mutations(&discount(), None).unwrap().create,
            "discountAutomaticAppCreate"
        );
        assert_eq!(
            get_mutations(&discount(), Some(DiscountMode::Code)).unwrap().delete,
            "discountCodeDelete"
        );
        let cart = FunctionDefinition::new(ShopifyFunctionType::CartTransform, "bundles");
        let set = get_mutations(&cart, None).unwrap();
        assert_eq!(set.update, None);
        assert_eq!(set.input_style, InputStyle::Flat);
    }

    #[test]
    fn resolves_static_dynamic_and_missing_titles() {
        let config = object(json!({ "percentage": 10 }));
        assert_eq!(resolve_title(discount().owner.as_ref(), &config), "10% off");
        assert_eq!(resolve_title(Some(&FunctionOwner::new("Fixed")), &config), "Fixed");
        assert_eq!(resolve_title(None, &config), "Untitled");
    }

    #[test]
    fn builds_the_metafield_as_json_text() {
        let metafield = build_metafield("volume-discount", &object(json!({ "minQuantity": 3 })));
        assert_eq!(
            metafield,
            json!({ "namespace": "$app:openshop", "key": "volume-discount", "type": "json", "value": "{\"minQuantity\":3}" })
        );
    }

    #[test]
    fn builds_automatic_discount_creation() {
        let config = object(json!({ "percentage": 10 }));
        let body = object(json!({ "startsAt": "2026-01-01T00:00:00.000Z" }));
        let request = build_create_mutation(
            &discount(),
            "volume-discount",
            DiscountMode::Automatic,
            "10% off",
            &config,
            &body,
        )
        .unwrap();
        assert!(request.mutation.contains("discountAutomaticAppCreate"));
        let input = &request.variables["input"];
        assert_eq!(input["functionHandle"], "volume-discount");
        assert_eq!(input["title"], "10% off");
        assert_eq!(input["startsAt"], "2026-01-01T00:00:00.000Z");
        assert_eq!(input["endsAt"], Value::Null);
        assert_eq!(
            input["combinesWith"],
            json!({ "productDiscounts": true, "orderDiscounts": false, "shippingDiscounts": true })
        );
        assert_eq!(input["metafields"][0]["key"], "volume-discount");
    }

    #[test]
    fn builds_code_discount_creation_with_code_and_usage_limit() {
        let body = object(json!({ "code": "SAVE10", "usageLimit": 5 }));
        let request = build_create_mutation(
            &discount(),
            "volume-discount",
            DiscountMode::Code,
            "t",
            &JsonMap::new(),
            &body,
        )
        .unwrap();
        assert!(request.mutation.contains("discountCodeAppCreate"));
        let input = &request.variables["input"];
        assert_eq!(input["code"], "SAVE10");
        assert_eq!(input["usageLimit"], 5);
        assert!(input["startsAt"].is_string());
    }

    #[test]
    fn builds_flat_and_wrapped_creations_for_other_types() {
        let empty = JsonMap::new();
        let cart = FunctionDefinition::new(ShopifyFunctionType::CartTransform, "bundles");
        let request = build_create_mutation(&cart, "bundles", DiscountMode::Automatic, "t", &empty, &empty).unwrap();
        assert_eq!(request.variables["functionHandle"], "bundles");
        assert_eq!(request.variables["blockOnFailure"], false);

        let rules = FunctionDefinition::new(ShopifyFunctionType::FulfillmentConstraints, "rules");
        let request = build_create_mutation(&rules, "rules", DiscountMode::Automatic, "t", &empty, &empty).unwrap();
        assert_eq!(request.variables["deliveryMethodTypes"], json!(["SHIPPING"]));

        let validation = FunctionDefinition::new(ShopifyFunctionType::CheckoutValidation, "limits");
        let body = object(json!({ "blockOnFailure": true }));
        let request =
            build_create_mutation(&validation, "limits", DiscountMode::Automatic, "Limits", &empty, &body).unwrap();
        assert_eq!(request.variables["input"]["enable"], true);
        assert_eq!(request.variables["input"]["blockOnFailure"], true);

        let delivery = FunctionDefinition::new(ShopifyFunctionType::DeliveryCustomization, "hide");
        let request =
            build_create_mutation(&delivery, "hide", DiscountMode::Automatic, "Hide", &empty, &empty).unwrap();
        assert_eq!(request.variables["input"]["enabled"], true);
    }

    #[test]
    fn update_omits_fields_the_request_did_not_send() {
        let request = build_update_mutation(
            &discount(),
            "volume-discount",
            DiscountMode::Automatic,
            "gid://shopify/DiscountAutomaticNode/1",
            "t",
            &JsonMap::new(),
            &object(json!({ "endsAt": null })),
        )
        .unwrap();
        let input = request.variables["input"].as_object().unwrap();
        assert!(!input.contains_key("startsAt"));
        assert_eq!(input["endsAt"], Value::Null);
        assert!(!input.contains_key("usageLimit"));
        assert!(input.contains_key("combinesWith"));
        assert_eq!(request.variables["id"], "gid://shopify/DiscountAutomaticNode/1");
    }

    #[test]
    fn update_is_unsupported_for_flat_types() {
        let cart = FunctionDefinition::new(ShopifyFunctionType::CartTransform, "bundles");
        assert!(
            build_update_mutation(
                &cart,
                "bundles",
                DiscountMode::Automatic,
                "id",
                "t",
                &JsonMap::new(),
                &JsonMap::new()
            )
            .is_none()
        );
    }

    #[test]
    fn extracts_user_errors_and_payload() {
        let data = json!({
            "extensions": { "cost": 1 },
            "cartTransformCreate": {
                "cartTransform": null,
                "userErrors": [{ "field": ["functionHandle"], "message": "Already registered", "code": "FUNCTION_ALREADY_REGISTERED" }],
            },
        });
        let errors = extract_errors(&data);
        assert_eq!(errors.len(), 1);
        assert_eq!(user_error_messages(&errors), "Already registered");
        assert!(has_user_error_code(&errors, "FUNCTION_ALREADY_REGISTERED"));
        assert!(!has_user_error_code(&errors, "OTHER"));
        assert_eq!(errors[0]["field"], json!(["functionHandle"]));
        assert!(extract_payload(&data).unwrap().get("userErrors").is_some());
        assert!(extract_errors(&json!({})).is_empty());
        assert!(extract_errors(&Value::Null).is_empty());
    }

    #[test]
    fn builds_delete_mutations() {
        let request = build_delete_mutation("cartTransformDelete", "gid://shopify/CartTransform/1");
        assert!(request.mutation.contains("cartTransformDelete(id: $id)"));
        assert_eq!(request.variables, json!({ "id": "gid://shopify/CartTransform/1" }));
    }
}
