//! Shopify Functions admin API (`/api/functions`): definitions come from the
//! configuration, instances live in Shopify behind the Admin GraphQL API.

mod common;

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use common::{graphql_requests, install, send, two_shopify_apps};
use openshop::builder::{
    CombinesWith, DiscountMode, Field, FieldOption, Function, FunctionOwner, FunctionType, OpenShopBuilder,
};
use openshop::testing::OutboundRequest;
use openshop::testing::{MockTransport, TestContext, TestOptions, session_token};
use openshop::{OpenShopConfig, Value, json, validators};
use reqwest::{Method, RequestBuilder};

const ACCESS_TOKEN: &str = "shpat_test";
const SHOP_B: &str = "functions-shop-b.myshopify.com";
const API_VERSION: &str = "2026-10";

// ─── Application under test ──────────────────────────────────────────

fn functions_builder() -> OpenShopBuilder {
    let message = || Field::text("Message");
    OpenShopConfig::builder()
        .function(
            "discount",
            Function::new(FunctionType::Discount, "volume-discount")
                .modes([DiscountMode::Automatic, DiscountMode::Code])
                .owner(
                    FunctionOwner::dynamic_title(|config| format!("Volume {}", config["percent"])).combines_with(
                        CombinesWith {
                            product_discounts: Some(true),
                            ..Default::default()
                        },
                    ),
                )
                .field(
                    "percent",
                    Field::number("Percent").validate(validators::range(Some(0.0), None)),
                ),
        )
        .function(
            "cartTransform",
            Function::new(FunctionType::CartTransform, "cart-transform")
                .label("Bundle cart transform")
                .field("message", message()),
        )
        .function(
            "fulfillmentConstraints",
            Function::new(FunctionType::FulfillmentConstraints, "fulfillment-rules").field("message", message()),
        )
        .function(
            "deliveryCustomization",
            Function::new(FunctionType::DeliveryCustomization, "delivery-rules")
                .owner(FunctionOwner::new("Delivery rules"))
                .field("message", message()),
        )
        .function(
            "paymentCustomization",
            Function::new(FunctionType::PaymentCustomization, "payment-rules")
                .owner(FunctionOwner::new("Payment rules"))
                .field("message", message()),
        )
        .function(
            "checkoutValidation",
            Function::new(FunctionType::CheckoutValidation, "checkout-validation")
                .owner(FunctionOwner::new("Checkout validation"))
                .field("message", message()),
        )
}

/// Definitions exercising defaults: discounts without modes or owner, and every field type.
fn extras_config() -> OpenShopConfig {
    OpenShopConfig::builder()
        .function("free_shipping", Function::new(FunctionType::Discount, "free-shipping"))
        .function(
            "codeOnly",
            Function::new(FunctionType::Discount, "code-only")
                .modes([DiscountMode::Code])
                .owner(FunctionOwner::new("Code only")),
        )
        .function(
            "tiers",
            Function::new(FunctionType::PaymentCustomization, "tiered-rules")
                .owner(FunctionOwner::dynamic_title(|config| {
                    format!("Tier {}", config["tier"].as_str().unwrap_or_default())
                }))
                .field(
                    "threshold",
                    Field::number("Threshold").validate(validators::integer_min(1)),
                )
                .field(
                    "tier",
                    Field::select(
                        "Tier",
                        [FieldOption::new("Gold", "gold"), FieldOption::new("Silver", "silver")],
                    )
                    .validate(validators::one_of(["gold", "silver"])),
                )
                .field("note", Field::text("Note").placeholder("Shown to staff").optional())
                .field("strict", Field::checkbox("Strict").optional()),
        )
        .build()
        .unwrap()
}

fn installed() -> TestOptions {
    TestOptions {
        access_token: Some(ACCESS_TOKEN.to_string()),
        ..Default::default()
    }
}

// ─── Mocked Shopify ──────────────────────────────────────────────────

#[derive(Default)]
struct ShopifyState {
    /// `userErrors` of the next mutation only.
    next_user_errors: Option<Value>,
    cart_transform_nodes: Vec<Value>,
}

type Shopify = Arc<Mutex<ShopifyState>>;

const MUTATIONS: [&str; 19] = [
    "discountAutomaticAppCreate",
    "discountCodeAppCreate",
    "cartTransformCreate",
    "fulfillmentConstraintRuleCreate",
    "deliveryCustomizationCreate",
    "paymentCustomizationCreate",
    "validationCreate",
    "discountAutomaticAppUpdate",
    "discountCodeAppUpdate",
    "deliveryCustomizationUpdate",
    "paymentCustomizationUpdate",
    "validationUpdate",
    "discountAutomaticDelete",
    "discountCodeDelete",
    "cartTransformDelete",
    "fulfillmentConstraintRuleDelete",
    "deliveryCustomizationDelete",
    "paymentCustomizationDelete",
    "validationDelete",
];

const GENERIC_LISTS: [(&str, &str); 4] = [
    ("deliveryCustomizations", "DeliveryCustomization"),
    ("paymentCustomizations", "PaymentCustomization"),
    ("validations", "Validation"),
    ("fulfillmentConstraintRules", "FulfillmentConstraintRule"),
];

fn graphql_data(state: &mut ShopifyState, query: &str) -> Value {
    if query.contains("ListDiscountInstances") {
        return json!({
            "discountNodes": {
                "nodes": [
                    {
                        "id": "gid://shopify/DiscountNode/1",
                        "discount": {
                            "title": "Volume 10",
                            "status": "ACTIVE",
                            "startsAt": "2026-01-01T00:00:00Z",
                            "endsAt": null,
                            "usageLimit": 5,
                            "combinesWith": { "productDiscounts": true },
                        },
                        "metafield": { "value": json!({ "percent": 10 }).to_string() },
                    },
                    {
                        "id": "gid://shopify/DiscountNode/2",
                        "discount": {
                            "title": "Legacy",
                            "status": "EXPIRED",
                            "startsAt": "2025-01-01T00:00:00Z",
                            "endsAt": "2025-02-01T00:00:00Z",
                        },
                        "metafield": null,
                    },
                ],
            },
        });
    }

    if query.contains("ListShopifyFunctions") {
        return json!({
            "shopifyFunctions": {
                "nodes": [
                    { "id": "function-cart", "handle": "cart-transform" },
                    { "id": "function-other", "handle": "other-cart-transform" },
                ],
            },
        });
    }

    if query.contains("ListCartTransformInstances") {
        return json!({ "cartTransforms": { "nodes": state.cart_transform_nodes } });
    }

    if query.contains("query ListInstances") {
        let (field, kind) = GENERIC_LISTS
            .into_iter()
            .find(|(field, _)| query.contains(&format!("{field}(first: 50, after: $after)")))
            .unwrap_or(("unknownList", "Unknown"));
        return json!({
            field: {
                "nodes": [{
                    "id": format!("gid://shopify/{kind}/1"),
                    "title": "Delivery rules",
                    "enabled": true,
                    "metafield": { "value": json!({ "message": "ship it" }).to_string() },
                }],
            },
        });
    }

    let key = MUTATIONS
        .into_iter()
        .find(|key| query.contains(key))
        .unwrap_or("unknownMutation");
    let user_errors = state.next_user_errors.take().unwrap_or_else(|| json!([]));
    json!({
        key: {
            "userErrors": user_errors,
            "automaticAppDiscount": { "discountId": "gid://shopify/Discount/automatic" },
            "codeAppDiscount": { "discountId": "gid://shopify/Discount/code" },
            "cartTransform": { "id": "gid://shopify/CartTransform/1" },
            "fulfillmentConstraintRule": { "id": "gid://shopify/FulfillmentConstraintRule/1" },
            "deliveryCustomization": { "id": "gid://shopify/DeliveryCustomization/1" },
            "paymentCustomization": { "id": "gid://shopify/PaymentCustomization/1" },
            "validation": { "id": "gid://shopify/Validation/1" },
        },
    })
}

/// Scripts the Admin GraphQL API; other outbound calls (token exchange) are not mocked.
fn mock_shopify(ctx: &TestContext) -> Shopify {
    let state = Shopify::default();
    let shared = state.clone();
    ctx.transport.respond_with(move |request| {
        if !request.url.ends_with("/graphql.json") {
            return Ok(MockTransport::json(404, json!({ "error": "not mocked" })));
        }
        let query = request.body["query"].as_str().unwrap_or_default();
        let data = graphql_data(&mut shared.lock().unwrap(), query);
        Ok(MockTransport::json(200, json!({ "data": data })))
    });
    state
}

async fn setup_with(config: OpenShopConfig) -> (TestContext, Shopify) {
    let ctx = TestContext::with_options(config, installed()).await.unwrap();
    let shopify = mock_shopify(&ctx);
    (ctx, shopify)
}

async fn setup() -> (TestContext, Shopify) {
    setup_with(functions_builder().build().unwrap()).await
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn with_session(ctx: &TestContext, method: Method, path: &str, session_token: &str) -> RequestBuilder {
    ctx.http
        .request(method, format!("{}{path}", ctx.url))
        .header("Authorization", format!("Bearer {session_token}"))
}

fn last_call(ctx: &TestContext) -> OutboundRequest {
    graphql_requests(ctx).pop().expect("a GraphQL call")
}

fn query_of(call: &OutboundRequest) -> &str {
    call.body["query"].as_str().expect("GraphQL document")
}

fn metafields(handle: &str, config: Value) -> Value {
    json!([{ "namespace": "$app:openshop", "key": handle, "type": "json", "value": config.to_string() }])
}

/// The configuration stored in the metafield of a create or update mutation.
fn stored_config(call: &OutboundRequest) -> Value {
    let variables = &call.body["variables"];
    let metafields = variables.get("metafields").unwrap_or(&variables["input"]["metafields"]);
    serde_json::from_str(metafields[0]["value"].as_str().expect("metafield value")).unwrap()
}

fn gid(id: &str) -> String {
    url::form_urlencoded::byte_serialize(id.as_bytes()).collect()
}

// ─── Definitions ─────────────────────────────────────────────────────

#[tokio::test]
async fn lists_function_definitions_without_calling_shopify() {
    let (ctx, _shopify) = setup().await;

    let (status, data) = send(ctx.get("/api/functions")).await;
    assert_eq!(status, 200);
    let message = json!({ "message": { "type": "text", "label": "Message" } });
    assert_eq!(
        data,
        json!([
            {
                "key": "discount",
                "label": "Discount",
                "type": "discount",
                "handle": "volume-discount",
                "modes": ["automatic", "code"],
                "supportsUpdate": true,
                "singleton": false,
                // Validators stay on the server.
                "fields": { "percent": { "type": "number", "label": "Percent" } },
            },
            {
                "key": "cartTransform",
                "label": "Bundle cart transform",
                "type": "cart-transform",
                "handle": "cart-transform",
                "supportsUpdate": false,
                "singleton": true,
                "fields": message,
            },
            {
                "key": "fulfillmentConstraints",
                "label": "Fulfillment Constraints",
                "type": "fulfillment-constraints",
                "handle": "fulfillment-rules",
                "supportsUpdate": false,
                "singleton": false,
                "fields": message,
            },
            {
                "key": "deliveryCustomization",
                "label": "Delivery Customization",
                "type": "delivery-customization",
                "handle": "delivery-rules",
                "supportsUpdate": true,
                "singleton": false,
                "fields": message,
            },
            {
                "key": "paymentCustomization",
                "label": "Payment Customization",
                "type": "payment-customization",
                "handle": "payment-rules",
                "supportsUpdate": true,
                "singleton": false,
                "fields": message,
            },
            {
                "key": "checkoutValidation",
                "label": "Checkout Validation",
                "type": "checkout-validation",
                "handle": "checkout-validation",
                "supportsUpdate": true,
                "singleton": false,
                "fields": message,
            },
        ])
    );
    assert!(graphql_requests(&ctx).is_empty());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn lists_default_modes_and_field_metadata() {
    let (ctx, _shopify) = setup_with(extras_config()).await;

    let (status, data) = send(ctx.get("/api/functions")).await;
    assert_eq!(status, 200);
    assert_eq!(
        data,
        json!([
            {
                "key": "free_shipping",
                "label": "Free Shipping",
                "type": "discount",
                "handle": "free-shipping",
                // Discounts are automatic unless they say otherwise.
                "modes": ["automatic"],
                "supportsUpdate": true,
                "singleton": false,
                "fields": {},
            },
            {
                "key": "codeOnly",
                "label": "Code Only",
                "type": "discount",
                "handle": "code-only",
                "modes": ["code"],
                "supportsUpdate": true,
                "singleton": false,
                "fields": {},
            },
            {
                "key": "tiers",
                "label": "Tiers",
                "type": "payment-customization",
                "handle": "tiered-rules",
                "supportsUpdate": true,
                "singleton": false,
                "fields": {
                    "threshold": { "type": "number", "label": "Threshold" },
                    "tier": {
                        "type": "select",
                        "label": "Tier",
                        "options": [{ "label": "Gold", "value": "gold" }, { "label": "Silver", "value": "silver" }],
                    },
                    "note": { "type": "text", "label": "Note", "placeholder": "Shown to staff", "required": false },
                    "strict": { "type": "checkbox", "label": "Strict", "required": false },
                },
            },
        ])
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn lists_nothing_without_configured_functions() {
    let (ctx, _shopify) = setup_with(OpenShopConfig::builder().build().unwrap()).await;

    let (status, data) = send(ctx.get("/api/functions")).await;
    assert_eq!((status, data), (200, json!([])));
    let (status, error) = send(ctx.get("/api/functions/volume-discount/instances")).await;
    assert_eq!((status, error), (404, json!({ "error": "Function not found" })));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn function_routes_require_a_session_token() {
    let (ctx, _shopify) = setup().await;

    let requests = [
        (Method::GET, "/api/functions"),
        (Method::GET, "/api/functions/delivery-rules/instances"),
        (Method::POST, "/api/functions/delivery-rules/instances"),
        (Method::PUT, "/api/functions/delivery-rules/instances/delivery-1"),
        (Method::DELETE, "/api/functions/delivery-rules/instances/delivery-1"),
    ];
    for (method, path) in requests {
        let request = ctx
            .http
            .request(method.clone(), format!("{}{path}", ctx.url))
            .json(&json!({ "config": { "message": "anonymous" } }));
        let (status, _) = send(request).await;
        assert_eq!(status, 401, "{method} {path}");
    }
    assert!(graphql_requests(&ctx).is_empty());
    ctx.shutdown().await.unwrap();
}

// ─── Listing instances ───────────────────────────────────────────────

#[tokio::test]
async fn lists_discount_instances_from_shopify() {
    let (ctx, _shopify) = setup().await;

    let (status, data) = send(ctx.get("/api/functions/volume-discount/instances")).await;
    assert_eq!(status, 200);
    assert_eq!(
        data,
        json!([
            {
                "id": "gid://shopify/DiscountNode/1",
                "title": "Volume 10",
                "status": "ACTIVE",
                "startsAt": "2026-01-01T00:00:00Z",
                "endsAt": null,
                "config": { "percent": 10 },
            },
            {
                "id": "gid://shopify/DiscountNode/2",
                "title": "Legacy",
                "status": "EXPIRED",
                "startsAt": "2025-01-01T00:00:00Z",
                "endsAt": "2025-02-01T00:00:00Z",
                // No metafield, no configuration.
                "config": {},
            },
        ])
    );

    let calls = graphql_requests(&ctx);
    assert_eq!(calls.len(), 1);
    let query = query_of(&calls[0]);
    assert!(query.contains("ListDiscountInstances"));
    assert!(query.contains(r#"metafield(namespace: "$app:openshop", key: "volume-discount")"#));
    assert_eq!(
        calls[0].body["variables"],
        json!({ "query": "function_handle:volume-discount" })
    );
    assert_eq!(
        calls[0].url,
        format!("https://{}/admin/api/{API_VERSION}/graphql.json", ctx.shop)
    );
    assert_eq!(calls[0].header("X-Shopify-Access-Token"), Some(ACCESS_TOKEN));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn lists_generic_non_discount_instances_from_shopify() {
    let (ctx, _shopify) = setup().await;

    let (status, data) = send(ctx.get("/api/functions/delivery-rules/instances")).await;
    assert_eq!(status, 200);
    assert_eq!(
        data,
        json!([{
            "id": "gid://shopify/DeliveryCustomization/1",
            "title": "Delivery rules",
            "enabled": true,
            "config": { "message": "ship it" },
        }])
    );
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("deliveryCustomizations"));
    assert!(query_of(&call).contains(r#"metafield(namespace: "$app:openshop", key: "delivery-rules")"#));
    assert!(call.body.get("variables").is_none());

    // Every other owner type has its own connection.
    for (handle, field, kind) in [
        ("payment-rules", "paymentCustomizations", "PaymentCustomization"),
        ("checkout-validation", "validations", "Validation"),
        (
            "fulfillment-rules",
            "fulfillmentConstraintRules",
            "FulfillmentConstraintRule",
        ),
    ] {
        let (status, data) = send(ctx.get(&format!("/api/functions/{handle}/instances"))).await;
        assert_eq!(status, 200, "{handle}");
        assert_eq!(data[0]["id"], format!("gid://shopify/{kind}/1"));
        assert_eq!(data[0]["config"], json!({ "message": "ship it" }));
        let call = last_call(&ctx);
        assert!(
            query_of(&call).contains(&format!("{field}(first: 50, after: $after)")),
            "{handle}"
        );
        assert!(query_of(&call).contains(&format!(r#"key: "{handle}""#)), "{handle}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn normalizes_and_filters_cart_transform_instances_by_deployed_function_handle() {
    let (ctx, shopify) = setup().await;
    shopify.lock().unwrap().cart_transform_nodes = vec![
        json!({
            "id": "gid://shopify/CartTransform/1",
            "functionId": "function-cart",
            "blockOnFailure": true,
            "metafield": { "value": json!({ "message": "bundle" }).to_string() },
        }),
        json!({
            "id": "gid://shopify/CartTransform/2",
            "functionId": "function-other",
            "blockOnFailure": false,
            "metafield": null,
        }),
    ];

    let (status, data) = send(ctx.get("/api/functions/cart-transform/instances")).await;
    assert_eq!(status, 200);
    assert_eq!(
        data,
        json!([{
            "id": "gid://shopify/CartTransform/1",
            "label": "Bundle cart transform",
            "state": "active",
            "blockOnFailure": true,
            "config": { "message": "bundle" },
        }])
    );
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("ListCartTransformInstances"));
    assert_eq!(call.body["variables"], json!({ "metafieldKey": "cart-transform" }));

    // An instance without a metafield has an empty configuration.
    shopify.lock().unwrap().cart_transform_nodes = vec![json!({
        "id": "gid://shopify/CartTransform/3",
        "functionId": "function-cart",
        "blockOnFailure": null,
        "metafield": null,
    })];
    let (_, data) = send(ctx.get("/api/functions/cart-transform/instances")).await;
    assert_eq!(
        data,
        json!([{
            "id": "gid://shopify/CartTransform/3",
            "label": "Bundle cart transform",
            "state": "active",
            "blockOnFailure": false,
            "config": {},
        }])
    );
    ctx.shutdown().await.unwrap();
}

// ─── Paginated listings ──────────────────────────────────────────────

/// Answers every page of a listing: `pages` lists the nodes of each page, and
/// the cursor of page `n` is `cursor-n`. `connections` names the connections the
/// query reads; each answers the same pages.
fn serve_pages(ctx: &TestContext, connections: &'static [&'static str], pages: Vec<Vec<Value>>) {
    ctx.transport.respond_with(move |request| {
        let after = request.body["variables"]["after"].as_str();
        let page = after.map_or(0, |cursor| {
            cursor.trim_start_matches("cursor-").parse::<usize>().unwrap()
        });
        let query = request.body["query"].as_str().unwrap_or_default();
        let mut data = serde_json::Map::new();
        for connection in connections {
            if !query.contains(&format!("{connection}(first:")) {
                continue;
            }
            let more = page + 1 < pages.len();
            data.insert(
                connection.to_string(),
                json!({
                    "nodes": pages[page],
                    "pageInfo": { "hasNextPage": more, "endCursor": more.then(|| format!("cursor-{}", page + 1)) },
                }),
            );
        }
        Ok(MockTransport::json(200, json!({ "data": data })))
    });
}

fn discount_node(id: usize) -> Value {
    json!({
        "id": format!("gid://shopify/DiscountNode/{id}"),
        "discount": { "title": format!("Volume {id}"), "status": "ACTIVE" },
        "metafield": null,
    })
}

#[tokio::test]
async fn lists_every_page_of_discount_instances() {
    let (ctx, _shopify) = setup().await;
    serve_pages(
        &ctx,
        &["discountNodes"],
        vec![
            vec![discount_node(1), discount_node(2)],
            vec![discount_node(3)],
            vec![discount_node(4)],
        ],
    );

    let (status, data) = send(ctx.get("/api/functions/volume-discount/instances")).await;
    assert_eq!(status, 200);
    let ids: Vec<&str> = data
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [1, 2, 3, 4]
            .map(|id| format!("gid://shopify/DiscountNode/{id}"))
            .as_slice()
    );

    let calls = graphql_requests(&ctx);
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[0].body["variables"],
        json!({ "query": "function_handle:volume-discount" })
    );
    assert_eq!(
        calls[1].body["variables"],
        json!({ "query": "function_handle:volume-discount", "after": "cursor-1" })
    );
    assert_eq!(calls[2].body["variables"]["after"], "cursor-2");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn lists_every_page_of_generic_and_cart_transform_instances() {
    let (ctx, _shopify) = setup().await;
    let generic = |id: usize| json!({ "id": format!("gid://shopify/DeliveryCustomization/{id}"), "title": "Rules", "enabled": true });
    serve_pages(
        &ctx,
        &["deliveryCustomizations"],
        vec![vec![generic(1)], vec![generic(2)]],
    );
    let (status, data) = send(ctx.get("/api/functions/delivery-rules/instances")).await;
    assert_eq!(status, 200);
    assert_eq!(data.as_array().unwrap().len(), 2);
    assert_eq!(graphql_requests(&ctx).len(), 2);

    // The functions of the app, and then its cart transforms.
    let cart = |id: usize| json!({ "id": format!("gid://shopify/CartTransform/{id}"), "functionId": "function-cart" });
    serve_pages(
        &ctx,
        &["shopifyFunctions", "cartTransforms"],
        vec![
            vec![json!({ "id": "function-cart", "handle": "cart-transform" }), cart(1)],
            vec![cart(2)],
        ],
    );
    let (status, data) = send(ctx.get("/api/functions/cart-transform/instances")).await;
    assert_eq!(status, 200);
    let ids: Vec<&str> = data
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["gid://shopify/CartTransform/1", "gid://shopify/CartTransform/2"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_listing_stops_at_a_bounded_number_of_pages() {
    let (ctx, _shopify) = setup().await;
    // A connection that never ends, and one whose cursor does not advance.
    ctx.transport.respond_with(|request| {
        let after = request.body["variables"]["after"]
            .as_str()
            .unwrap_or("none")
            .to_string();
        Ok(MockTransport::json(
            200,
            json!({ "data": { "discountNodes": {
                "nodes": [discount_node(1)],
                "pageInfo": { "hasNextPage": true, "endCursor": format!("{after}+") },
            } } }),
        ))
    });
    let (status, data) = send(ctx.get("/api/functions/volume-discount/instances")).await;
    assert_eq!(status, 200);
    assert_eq!(data.as_array().unwrap().len(), graphql_requests(&ctx).len());
    assert_eq!(graphql_requests(&ctx).len(), 20);

    ctx.transport.respond_with(|_| {
        Ok(MockTransport::json(
            200,
            json!({ "data": { "discountNodes": {
                "nodes": [discount_node(1)],
                "pageInfo": { "hasNextPage": true, "endCursor": "stuck" },
            } } }),
        ))
    });
    let before = graphql_requests(&ctx).len();
    let (status, data) = send(ctx.get("/api/functions/volume-discount/instances")).await;
    assert_eq!(status, 200);
    assert_eq!(data.as_array().unwrap().len(), 2);
    assert_eq!(graphql_requests(&ctx).len() - before, 2);
    ctx.shutdown().await.unwrap();
}

// ─── Tenant scoping ──────────────────────────────────────────────────

#[tokio::test]
async fn uses_the_jwt_shop_installation_for_shopify_function_reads_and_writes() {
    let (ctx, _shopify) = setup().await;
    install(&ctx.db, "default", SHOP_B, Some("token-b"), false).await;
    let shop_b_api = format!("https://{SHOP_B}/admin/api/");

    let (status, _) = send(ctx.request_as(Method::GET, "/api/functions/volume-discount/instances", SHOP_B, "1")).await;
    assert_eq!(status, 200);
    let call = last_call(&ctx);
    assert!(call.url.starts_with(&shop_b_api), "{}", call.url);
    assert_eq!(call.header("X-Shopify-Access-Token"), Some("token-b"));

    let foreign_id = "gid://shopify/DiscountNode/other-shop";
    let path = format!("/api/functions/volume-discount/instances/{}?mode=code", gid(foreign_id));
    let (status, _) = send(
        ctx.request_as(Method::PUT, &path, SHOP_B, "1")
            .json(&json!({ "mode": "code", "config": { "percent": 20 } })),
    )
    .await;
    assert_eq!(status, 200);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountCodeAppUpdate"));
    assert!(call.url.starts_with(&shop_b_api), "{}", call.url);
    assert_eq!(call.header("X-Shopify-Access-Token"), Some("token-b"));
    assert_eq!(call.body["variables"]["id"], foreign_id);

    let (status, _) = send(ctx.request_as(Method::DELETE, &path, SHOP_B, "1")).await;
    assert_eq!(status, 200);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountCodeDelete"));
    assert!(call.url.starts_with(&shop_b_api), "{}", call.url);
    assert_eq!(call.header("X-Shopify-Access-Token"), Some("token-b"));
    assert_eq!(call.body["variables"], json!({ "id": foreign_id }));

    let (status, _) = send(
        ctx.request_as(Method::POST, "/api/functions/delivery-rules/instances", SHOP_B, "1")
            .json(&json!({ "config": { "message": "shop b" } })),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(last_call(&ctx).header("X-Shopify-Access-Token"), Some("token-b"));

    // The context's own shop keeps using its own installation.
    send(ctx.get("/api/functions/volume-discount/instances")).await;
    let call = last_call(&ctx);
    assert!(call.url.starts_with(&format!("https://{}/admin/api/", ctx.shop)));
    assert_eq!(call.header("X-Shopify-Access-Token"), Some(ACCESS_TOKEN));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn uses_the_installation_of_the_session_shopify_app() {
    let config = functions_builder().shopify(two_shopify_apps()).build().unwrap();
    let options = TestOptions {
        api_key: "client-a-key".to_string(),
        secret: "client-a-secret".to_string(),
        ..Default::default()
    };
    let ctx = TestContext::with_options(config, options).await.unwrap();
    mock_shopify(&ctx);
    let shop = ctx.shop.clone();
    install(&ctx.db, "clientA", &shop, Some("token-a"), false).await;
    install(&ctx.db, "clientB", &shop, Some("token-b"), false).await;
    let session_b = session_token(&shop, "client-b-secret", "client-b-key", "1");

    let (status, _) = send(ctx.get("/api/functions/volume-discount/instances")).await;
    assert_eq!(status, 200);
    assert_eq!(last_call(&ctx).header("X-Shopify-Access-Token"), Some("token-a"));

    let path = "/api/functions/volume-discount/instances";
    let (status, _) = send(with_session(&ctx, Method::GET, path, &session_b)).await;
    assert_eq!(status, 200);
    assert_eq!(last_call(&ctx).header("X-Shopify-Access-Token"), Some("token-b"));

    let (status, _) = send(
        with_session(&ctx, Method::POST, "/api/functions/payment-rules/instances", &session_b)
            .json(&json!({ "config": { "message": "app b" } })),
    )
    .await;
    assert_eq!(status, 201);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("paymentCustomizationCreate"));
    assert_eq!(call.header("X-Shopify-Access-Token"), Some("token-b"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn fails_without_calling_shopify_when_the_shop_has_no_access_token() {
    let (ctx, _shopify) = setup().await;

    // Definitions do not need an installation.
    let (status, _) = send(ctx.request_as(Method::GET, "/api/functions", SHOP_B, "1")).await;
    assert_eq!(status, 200);

    let requests = [
        (Method::GET, "/api/functions/volume-discount/instances"),
        (Method::POST, "/api/functions/delivery-rules/instances"),
        (Method::PUT, "/api/functions/delivery-rules/instances/delivery-1"),
        (Method::DELETE, "/api/functions/delivery-rules/instances/delivery-1"),
    ];
    for (method, path) in requests {
        let request = ctx
            .request_as(method.clone(), path, SHOP_B, "1")
            .json(&json!({ "config": { "message": "none" } }));
        let (status, body) = send(request).await;
        assert_eq!(status, 500, "{method} {path}");
        assert_eq!(body, "Internal Server Error");
    }
    assert!(graphql_requests(&ctx).is_empty());
    ctx.shutdown().await.unwrap();
}

// ─── Creating instances ──────────────────────────────────────────────

#[tokio::test]
async fn creates_discount_instances_for_automatic_and_code_modes() {
    let (ctx, _shopify) = setup().await;
    let path = "/api/functions/volume-discount/instances";

    let (status, body) = send(ctx.post(path).json(&json!({
        "mode": "automatic",
        "startsAt": "2026-01-01T00:00:00Z",
        "config": { "percent": 10 },
    })))
    .await;
    assert_eq!(status, 201);
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["data"]["automaticAppDiscount"]["discountId"],
        "gid://shopify/Discount/automatic"
    );
    assert_eq!(body["data"]["userErrors"], json!([]));
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountAutomaticAppCreate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "input": {
                "functionHandle": "volume-discount",
                "title": "Volume 10",
                "startsAt": "2026-01-01T00:00:00Z",
                "endsAt": null,
                "combinesWith": { "productDiscounts": true },
                "metafields": metafields("volume-discount", json!({ "percent": 10 })),
            },
        })
    );

    let (status, _) = send(ctx.post(path).json(&json!({
        "mode": "code",
        "code": "SAVE10",
        "usageLimit": 5,
        "startsAt": "2026-02-01T00:00:00Z",
        "endsAt": "2026-03-01T00:00:00Z",
        "config": { "percent": 10 },
    })))
    .await;
    assert_eq!(status, 201);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountCodeAppCreate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "input": {
                "functionHandle": "volume-discount",
                "title": "Volume 10",
                "code": "SAVE10",
                "startsAt": "2026-02-01T00:00:00Z",
                "endsAt": "2026-03-01T00:00:00Z",
                "usageLimit": 5,
                "combinesWith": { "productDiscounts": true },
                "metafields": metafields("volume-discount", json!({ "percent": 10 })),
            },
        })
    );
    assert_eq!(graphql_requests(&ctx).len(), 2);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn creates_flat_and_wrapped_non_discount_instances() {
    let (ctx, _shopify) = setup().await;

    let (status, body) = send(
        ctx.post("/api/functions/cart-transform/instances")
            .json(&json!({ "blockOnFailure": true, "config": { "message": "cart" } })),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(body["data"]["cartTransform"]["id"], "gid://shopify/CartTransform/1");
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("cartTransformCreate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "functionHandle": "cart-transform",
            "blockOnFailure": true,
            "metafields": metafields("cart-transform", json!({ "message": "cart" })),
        })
    );

    let (status, _) = send(
        ctx.post("/api/functions/fulfillment-rules/instances")
            .json(&json!({ "deliveryMethodTypes": ["SHIPPING", "PICK_UP"], "config": { "message": "fulfillment" } })),
    )
    .await;
    assert_eq!(status, 201);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("fulfillmentConstraintRuleCreate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "functionHandle": "fulfillment-rules",
            "deliveryMethodTypes": ["SHIPPING", "PICK_UP"],
            "metafields": metafields("fulfillment-rules", json!({ "message": "fulfillment" })),
        })
    );

    let (status, _) = send(
        ctx.post("/api/functions/delivery-rules/instances")
            .json(&json!({ "config": { "message": "delivery" } })),
    )
    .await;
    assert_eq!(status, 201);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("deliveryCustomizationCreate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "input": {
                "functionHandle": "delivery-rules",
                "title": "Delivery rules",
                "enabled": true,
                "metafields": metafields("delivery-rules", json!({ "message": "delivery" })),
            },
        })
    );

    let (status, _) = send(
        ctx.post("/api/functions/payment-rules/instances")
            .json(&json!({ "config": { "message": "payment" } })),
    )
    .await;
    assert_eq!(status, 201);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("paymentCustomizationCreate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "input": {
                "functionHandle": "payment-rules",
                "title": "Payment rules",
                "enabled": true,
                "metafields": metafields("payment-rules", json!({ "message": "payment" })),
            },
        })
    );

    let (status, _) = send(
        ctx.post("/api/functions/checkout-validation/instances")
            .json(&json!({ "blockOnFailure": true, "config": { "message": "validation" } })),
    )
    .await;
    assert_eq!(status, 201);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("validationCreate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "input": {
                "functionHandle": "checkout-validation",
                "title": "Checkout validation",
                "enable": true,
                "blockOnFailure": true,
                "metafields": metafields("checkout-validation", json!({ "message": "validation" })),
            },
        })
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn create_applies_defaults_for_fields_the_request_omits() {
    let (ctx, _shopify) = setup().await;

    // Without a mode the function's first mode is used; the discount starts now.
    let before = Utc::now() - chrono::Duration::seconds(1);
    for body in [
        json!({ "config": { "percent": 12.5 } }),
        json!({ "mode": "weekly", "config": { "percent": 12.5 } }),
    ] {
        let (status, _) = send(ctx.post("/api/functions/volume-discount/instances").json(&body)).await;
        assert_eq!(status, 201);
        let call = last_call(&ctx);
        assert!(query_of(&call).contains("discountAutomaticAppCreate"));
        let input = &call.body["variables"]["input"];
        assert_eq!(input["title"], "Volume 12.5");
        assert_eq!(input["endsAt"], Value::Null);
        let starts_at = input["startsAt"].as_str().expect("startsAt");
        assert!(starts_at.len() == 24 && starts_at.ends_with('Z'), "{starts_at}");
        let starts_at = DateTime::parse_from_rfc3339(starts_at).unwrap();
        assert!(starts_at >= before && starts_at <= Utc::now() + chrono::Duration::seconds(1));
    }

    let (status, _) = send(
        ctx.post("/api/functions/volume-discount/instances")
            .json(&json!({ "mode": "code", "config": { "percent": 5 } })),
    )
    .await;
    assert_eq!(status, 201);
    let input = last_call(&ctx).body["variables"]["input"].clone();
    assert!(input.get("code").is_none());
    assert_eq!(input["usageLimit"], Value::Null);
    assert_eq!(input["endsAt"], Value::Null);
    assert!(input["startsAt"].is_string());

    let (status, _) = send(
        ctx.post("/api/functions/cart-transform/instances")
            .json(&json!({ "config": { "message": "cart" } })),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(last_call(&ctx).body["variables"]["blockOnFailure"], false);

    let (status, _) = send(
        ctx.post("/api/functions/fulfillment-rules/instances")
            .json(&json!({ "config": { "message": "fulfillment" } })),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(
        last_call(&ctx).body["variables"]["deliveryMethodTypes"],
        json!(["SHIPPING"])
    );

    let (status, _) = send(
        ctx.post("/api/functions/checkout-validation/instances")
            .json(&json!({ "config": { "message": "validation" } })),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(last_call(&ctx).body["variables"]["input"]["blockOnFailure"], false);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn discounts_without_modes_or_owner_fall_back_to_defaults() {
    let (ctx, _shopify) = setup_with(extras_config()).await;

    // No owner: untitled, combining with nothing. No modes: automatic.
    let (status, _) = send(
        ctx.post("/api/functions/free-shipping/instances")
            .json(&json!({ "startsAt": "2026-01-01T00:00:00Z" })),
    )
    .await;
    assert_eq!(status, 201);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountAutomaticAppCreate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "input": {
                "functionHandle": "free-shipping",
                "title": "Untitled",
                "startsAt": "2026-01-01T00:00:00Z",
                "endsAt": null,
                "combinesWith": {},
                "metafields": metafields("free-shipping", json!({})),
            },
        })
    );
    let (status, _) = send(ctx.delete("/api/functions/free-shipping/instances/discount-1")).await;
    assert_eq!(status, 200);
    assert!(query_of(&last_call(&ctx)).contains("discountAutomaticDelete(id: $id)"));

    // A code-only discount never needs the mode spelled out.
    let (status, _) = send(
        ctx.post("/api/functions/code-only/instances")
            .json(&json!({ "code": "ONLY", "startsAt": "2026-01-01T00:00:00Z" })),
    )
    .await;
    assert_eq!(status, 201);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountCodeAppCreate"));
    assert_eq!(call.body["variables"]["input"]["title"], "Code only");
    assert_eq!(call.body["variables"]["input"]["code"], "ONLY");

    let (status, _) = send(
        ctx.put("/api/functions/code-only/instances/discount-2")
            .json(&json!({ "usageLimit": 3 })),
    )
    .await;
    assert_eq!(status, 200);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountCodeAppUpdate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "id": "discount-2",
            "input": { "title": "Code only", "usageLimit": 3, "metafields": metafields("code-only", json!({})) },
        })
    );

    let (status, _) = send(ctx.delete("/api/functions/code-only/instances/discount-2")).await;
    assert_eq!(status, 200);
    assert!(query_of(&last_call(&ctx)).contains("discountCodeDelete(id: $id)"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejects_a_second_cart_transform_instance() {
    let (ctx, shopify) = setup().await;
    shopify.lock().unwrap().cart_transform_nodes = vec![json!({
        "id": "gid://shopify/CartTransform/1",
        "functionId": "function-cart",
        "blockOnFailure": false,
        "metafield": null,
    })];

    let (status, body) = send(
        ctx.post("/api/functions/cart-transform/instances")
            .json(&json!({ "blockOnFailure": true, "config": { "message": "duplicate" } })),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body, json!({ "error": "This Cart Transform already has an instance" }));
    let calls = graphql_requests(&ctx);
    assert_eq!(calls.len(), 2);
    assert!(query_of(&calls[0]).contains("ListShopifyFunctions"));
    assert!(query_of(&calls[1]).contains("ListCartTransformInstances"));

    // Instances of other cart transform functions do not count.
    shopify.lock().unwrap().cart_transform_nodes = vec![json!({
        "id": "gid://shopify/CartTransform/2",
        "functionId": "function-other",
        "blockOnFailure": false,
        "metafield": null,
    })];
    let (status, _) = send(
        ctx.post("/api/functions/cart-transform/instances")
            .json(&json!({ "config": { "message": "first" } })),
    )
    .await;
    assert_eq!(status, 201);
    assert!(query_of(&last_call(&ctx)).contains("cartTransformCreate"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn maps_shopify_cart_transform_duplicate_races_to_conflict() {
    let (ctx, shopify) = setup().await;
    let user_errors = json!([{
        "field": "functionHandle",
        "message": "A cart transform already exists for this function",
        "code": "FUNCTION_ALREADY_REGISTERED",
    }]);
    shopify.lock().unwrap().next_user_errors = Some(user_errors.clone());

    let (status, body) = send(
        ctx.post("/api/functions/cart-transform/instances")
            .json(&json!({ "blockOnFailure": true, "config": { "message": "racing duplicate" } })),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(
        body,
        json!({ "error": "This Cart Transform already has an instance", "userErrors": user_errors })
    );
    assert!(query_of(&last_call(&ctx)).contains("userErrors { field message code }"));
    assert_eq!(graphql_requests(&ctx).len(), 3);

    // Any other cart transform error stays a plain validation failure.
    shopify.lock().unwrap().next_user_errors = Some(json!([{
        "field": ["functionHandle"],
        "message": "Function not found",
        "code": "FUNCTION_NOT_FOUND",
    }]));
    let (status, body) = send(
        ctx.post("/api/functions/cart-transform/instances")
            .json(&json!({ "config": { "message": "missing" } })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "Function not found");

    // The code only means a conflict for cart transforms.
    shopify.lock().unwrap().next_user_errors = Some(user_errors.clone());
    let (status, body) = send(
        ctx.post("/api/functions/delivery-rules/instances")
            .json(&json!({ "config": { "message": "delivery" } })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        body,
        json!({ "error": "A cart transform already exists for this function", "userErrors": user_errors })
    );
    ctx.shutdown().await.unwrap();
}

// ─── Updating instances ──────────────────────────────────────────────

#[tokio::test]
async fn updates_supported_instances_from_a_single_parsed_body() {
    let (ctx, _shopify) = setup().await;

    let (status, body) = send(
        ctx.put("/api/functions/volume-discount/instances/discount-1")
            .json(&json!({
                "mode": "automatic",
                "startsAt": "2026-01-01T00:00:00Z",
                "config": { "percent": 15 },
            })),
    )
    .await;
    assert_eq!((status, body), (200, json!({ "ok": true })));
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountAutomaticAppUpdate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "id": "discount-1",
            "input": {
                "title": "Volume 15",
                "startsAt": "2026-01-01T00:00:00Z",
                "combinesWith": { "productDiscounts": true },
                "metafields": metafields("volume-discount", json!({ "percent": 15 })),
            },
        })
    );

    let (status, _) = send(
        ctx.put("/api/functions/volume-discount/instances/discount-2")
            .json(&json!({
                "mode": "code",
                "usageLimit": 12,
                "config": { "percent": 20 },
            })),
    )
    .await;
    assert_eq!(status, 200);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountCodeAppUpdate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "id": "discount-2",
            "input": {
                "title": "Volume 20",
                "usageLimit": 12,
                "combinesWith": { "productDiscounts": true },
                "metafields": metafields("volume-discount", json!({ "percent": 20 })),
            },
        })
    );

    let (status, _) = send(
        ctx.put("/api/functions/delivery-rules/instances/delivery-1")
            .json(&json!({ "enabled": false, "config": { "message": "delivery" } })),
    )
    .await;
    assert_eq!(status, 200);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("deliveryCustomizationUpdate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "id": "delivery-1",
            "input": {
                "title": "Delivery rules",
                "enabled": false,
                "metafields": metafields("delivery-rules", json!({ "message": "delivery" })),
            },
        })
    );

    let (status, _) = send(
        ctx.put("/api/functions/payment-rules/instances/payment-1")
            .json(&json!({ "enabled": true, "config": { "message": "payment" } })),
    )
    .await;
    assert_eq!(status, 200);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("paymentCustomizationUpdate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "id": "payment-1",
            "input": {
                "title": "Payment rules",
                "enabled": true,
                "metafields": metafields("payment-rules", json!({ "message": "payment" })),
            },
        })
    );

    let (status, _) = send(
        ctx.put("/api/functions/checkout-validation/instances/validation-1")
            .json(&json!({ "enabled": true, "blockOnFailure": true, "config": { "message": "validation" } })),
    )
    .await;
    assert_eq!(status, 200);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("validationUpdate"));
    assert_eq!(
        call.body["variables"],
        json!({
            "id": "validation-1",
            "input": {
                "title": "Checkout validation",
                "enable": true,
                "blockOnFailure": true,
                "metafields": metafields("checkout-validation", json!({ "message": "validation" })),
            },
        })
    );
    assert_eq!(graphql_requests(&ctx).len(), 5);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn update_omits_input_keys_the_request_did_not_provide() {
    let (ctx, _shopify) = setup().await;
    let keys = |call: &OutboundRequest| -> Vec<String> {
        let mut keys: Vec<String> = call.body["variables"]["input"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    };

    let id = "gid://shopify/DeliveryCustomization/7";
    let path = format!("/api/functions/delivery-rules/instances/{}", gid(id));
    let (status, _) = send(ctx.put(&path).json(&json!({ "config": { "message": "kept" } }))).await;
    assert_eq!(status, 200);
    let call = last_call(&ctx);
    assert_eq!(call.body["variables"]["id"], id);
    assert_eq!(keys(&call), ["metafields", "title"]);

    let (status, _) = send(
        ctx.put("/api/functions/checkout-validation/instances/validation-1")
            .json(&json!({ "config": { "message": "kept" } })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(keys(&last_call(&ctx)), ["metafields", "title"]);

    // An explicit null is a value: it clears the end date.
    let (status, _) = send(
        ctx.put("/api/functions/volume-discount/instances/discount-1")
            .json(&json!({ "endsAt": null, "usageLimit": 9, "config": { "percent": 1 } })),
    )
    .await;
    assert_eq!(status, 200);
    let call = last_call(&ctx);
    // The first mode is automatic, which has no usage limit.
    assert!(query_of(&call).contains("discountAutomaticAppUpdate"));
    assert_eq!(keys(&call), ["combinesWith", "endsAt", "metafields", "title"]);
    assert_eq!(call.body["variables"]["input"]["endsAt"], Value::Null);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejects_unsupported_updates() {
    let (ctx, _shopify) = setup().await;

    for (handle, function_type) in [
        ("cart-transform", "cart-transform"),
        ("fulfillment-rules", "fulfillment-constraints"),
    ] {
        let (status, body) = send(
            ctx.put(&format!("/api/functions/{handle}/instances/instance-1"))
                .json(&json!({ "config": { "message": "cart" } })),
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(
            body["error"],
            format!("Function type \"{function_type}\" does not support update. Delete and recreate instead.")
        );
    }
    assert!(graphql_requests(&ctx).is_empty());
    ctx.shutdown().await.unwrap();
}

// ─── Deleting instances ──────────────────────────────────────────────

#[tokio::test]
async fn deletes_mapped_function_instances() {
    let (ctx, _shopify) = setup().await;

    let (status, body) = send(ctx.delete("/api/functions/delivery-rules/instances/delivery-1")).await;
    assert_eq!((status, body), (200, json!({ "ok": true })));
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("deliveryCustomizationDelete"));
    assert_eq!(call.body["variables"], json!({ "id": "delivery-1" }));

    let (status, _) = send(ctx.delete("/api/functions/volume-discount/instances/discount-2?mode=code")).await;
    assert_eq!(status, 200);
    assert!(query_of(&last_call(&ctx)).contains("discountCodeDelete"));

    let id = "gid://shopify/Example/42";
    for (handle, suffix, mutation) in [
        ("volume-discount", "?mode=automatic", "discountAutomaticDelete"),
        ("cart-transform", "", "cartTransformDelete"),
        ("fulfillment-rules", "", "fulfillmentConstraintRuleDelete"),
        ("payment-rules", "", "paymentCustomizationDelete"),
        ("checkout-validation", "", "validationDelete"),
    ] {
        let path = format!("/api/functions/{handle}/instances/{}{suffix}", gid(id));
        let (status, body) = send(ctx.delete(&path)).await;
        assert_eq!((status, body), (200, json!({ "ok": true })), "{handle}");
        let call = last_call(&ctx);
        assert!(query_of(&call).contains(&format!("{mutation}(id: $id)")), "{handle}");
        assert_eq!(call.body["variables"], json!({ "id": id }));
    }
    assert_eq!(graphql_requests(&ctx).len(), 7);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn requires_mode_when_deleting_multi_mode_discounts() {
    let (ctx, _shopify) = setup().await;

    for path in [
        "/api/functions/volume-discount/instances/discount-2",
        "/api/functions/volume-discount/instances/discount-2?mode=weekly",
        "/api/functions/volume-discount/instances/discount-2?mode=",
    ] {
        let (status, body) = send(ctx.delete(path)).await;
        assert_eq!(status, 400, "{path}");
        assert!(body["error"].as_str().unwrap().contains("mode"));
        assert_eq!(
            body,
            json!({ "error": "mode query parameter is required for discount functions with multiple modes" })
        );
    }
    assert!(graphql_requests(&ctx).is_empty());
    ctx.shutdown().await.unwrap();
}

// ─── Errors ──────────────────────────────────────────────────────────

#[tokio::test]
async fn returns_errors_for_unknown_functions_and_invalid_config() {
    let (ctx, _shopify) = setup().await;

    let not_found = json!({ "error": "Function not found" });
    let (status, body) = send(ctx.get("/api/functions/missing/instances")).await;
    assert_eq!((status, body), (404, not_found.clone()));
    for request in [
        ctx.post("/api/functions/missing/instances"),
        ctx.put("/api/functions/missing/instances/instance-1"),
        ctx.delete("/api/functions/missing/instances/instance-1"),
        // Functions are addressed by handle, not by configuration key.
        ctx.get("/api/functions/discount/instances"),
    ] {
        let (status, body) = send(request.json(&json!({ "config": { "percent": 10 } }))).await;
        assert_eq!((status, body), (404, not_found.clone()));
    }

    let (status, body) = send(
        ctx.post("/api/functions/volume-discount/instances")
            .json(&json!({ "config": { "percent": -1 } })),
    )
    .await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().starts_with("Field \"percent\": "));
    assert_eq!(
        body,
        json!({ "error": "Field \"percent\": must be at least 0 (was -1)" })
    );

    let (status, body) = send(
        ctx.put("/api/functions/volume-discount/instances/discount-1")
            .json(&json!({ "mode": "code", "config": { "percent": -1 } })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        body,
        json!({ "error": "Field \"percent\": must be at least 0 (was -1)" })
    );

    // Missing configuration, a missing body and a non-JSON body are all incomplete.
    let required = json!({ "error": "Field \"percent\" is required" });
    for request in [
        ctx.post("/api/functions/volume-discount/instances").json(&json!({})),
        ctx.post("/api/functions/volume-discount/instances")
            .json(&json!({ "config": [10] })),
        ctx.post("/api/functions/volume-discount/instances")
            .json(&json!({ "config": { "percent": "" } })),
        ctx.post("/api/functions/volume-discount/instances"),
        ctx.post("/api/functions/volume-discount/instances").body("percent=10"),
        ctx.put("/api/functions/volume-discount/instances/discount-1")
            .json(&json!({})),
    ] {
        let (status, body) = send(request).await;
        assert_eq!((status, body), (400, required.clone()));
    }
    assert!(graphql_requests(&ctx).is_empty());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn validates_and_coerces_instance_config() {
    let (ctx, _shopify) = setup_with(extras_config()).await;
    let create = |config: Value| {
        send(
            ctx.post("/api/functions/tiered-rules/instances")
                .json(&json!({ "config": config })),
        )
    };

    // Form values arrive as strings; unknown keys and empty optional fields are dropped.
    let (status, _) = create(json!({
        "threshold": "5",
        "tier": "gold",
        "note": "",
        "strict": "true",
        "unexpected": "dropped",
    }))
    .await;
    assert_eq!(status, 201);
    let call = last_call(&ctx);
    assert_eq!(
        stored_config(&call),
        json!({ "threshold": 5, "tier": "gold", "strict": true })
    );
    assert_eq!(call.body["variables"]["input"]["title"], "Tier gold");

    let (status, _) = create(json!({ "threshold": 2.0, "tier": "silver", "note": "VIP only", "strict": false })).await;
    assert_eq!(status, 201);
    assert_eq!(
        stored_config(&last_call(&ctx)),
        json!({ "threshold": 2.0, "tier": "silver", "note": "VIP only", "strict": false })
    );
    let stored = graphql_requests(&ctx).len();

    let invalid = [
        (json!({ "tier": "gold" }), "Field \"threshold\" is required"),
        // A number field that cannot be parsed is refused, not treated as missing.
        (
            json!({ "threshold": "many", "tier": "gold" }),
            "Field \"threshold\": must be a number (was \"many\")",
        ),
        (
            json!({ "threshold": 0, "tier": "gold" }),
            "Field \"threshold\": must be at least 1 (was 0)",
        ),
        (
            json!({ "threshold": 1.5, "tier": "gold" }),
            "Field \"threshold\": must be an integer (was 1.5)",
        ),
        (json!({ "threshold": 2 }), "Field \"tier\" is required"),
        (
            json!({ "threshold": 2, "tier": "bronze" }),
            "Field \"tier\": must be one of \"gold\", \"silver\" (was \"bronze\")",
        ),
    ];
    for (config, message) in invalid {
        let (status, body) = create(config).await;
        assert_eq!((status, body), (400, json!({ "error": message })));
    }
    assert_eq!(graphql_requests(&ctx).len(), stored);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn surfaces_shopify_user_errors() {
    let (ctx, shopify) = setup().await;
    let fail_next = |errors: Value| shopify.lock().unwrap().next_user_errors = Some(errors);

    fail_next(json!([{ "field": "title", "message": "Title is invalid" }]));
    let (status, body) = send(
        ctx.post("/api/functions/delivery-rules/instances")
            .json(&json!({ "config": { "message": "delivery" } })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "Title is invalid");
    assert_eq!(
        body["userErrors"],
        json!([{ "field": "title", "message": "Title is invalid" }])
    );

    // Several errors are joined into one message.
    let errors = json!([
        { "field": ["input", "title"], "message": "Title is too long" },
        { "field": null, "message": "Function is not deployed" },
    ]);
    fail_next(errors.clone());
    let (status, body) = send(
        ctx.put("/api/functions/delivery-rules/instances/delivery-1")
            .json(&json!({ "config": { "message": "delivery" } })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        body,
        json!({ "error": "Title is too long, Function is not deployed", "userErrors": errors })
    );

    fail_next(json!([{ "field": ["id"], "message": "Customization does not exist" }]));
    let (status, body) = send(ctx.delete("/api/functions/delivery-rules/instances/delivery-1")).await;
    assert_eq!(status, 400);
    assert_eq!(
        body,
        json!({
            "error": "Customization does not exist",
            "userErrors": [{ "field": ["id"], "message": "Customization does not exist" }],
        })
    );

    // The next mutation succeeds again.
    let (status, _) = send(ctx.delete("/api/functions/delivery-rules/instances/delivery-1")).await;
    assert_eq!(status, 200);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn reports_shopify_failures_as_internal_errors() {
    let (ctx, _shopify) = setup().await;
    let attempts = |ctx: &TestContext| {
        [
            ctx.get("/api/functions/volume-discount/instances"),
            ctx.get("/api/functions/cart-transform/instances"),
            ctx.post("/api/functions/delivery-rules/instances")
                .json(&json!({ "config": { "message": "delivery" } })),
            ctx.put("/api/functions/delivery-rules/instances/delivery-1")
                .json(&json!({ "config": { "message": "delivery" } })),
            ctx.delete("/api/functions/delivery-rules/instances/delivery-1"),
        ]
    };

    ctx.transport
        .respond_with(|_| Ok(MockTransport::json(503, json!({ "errors": "Service Unavailable" }))));
    for request in attempts(&ctx) {
        let (status, body) = send(request).await;
        assert_eq!(status, 500);
        assert_eq!(body, "Internal Server Error");
    }

    ctx.transport.respond_with(|_| {
        Ok(MockTransport::json(
            200,
            json!({ "errors": [{ "message": "Access denied for discountNodes field" }] }),
        ))
    });
    for request in attempts(&ctx) {
        let (status, body) = send(request).await;
        assert_eq!(status, 500);
        assert_eq!(body, "Internal Server Error");
    }
    ctx.shutdown().await.unwrap();
}
