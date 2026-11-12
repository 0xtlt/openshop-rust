//! The page `functions` of the framework, rendered by the server: the
//! functions of the app, their instances read from Shopify's Admin API (a
//! mock here, as in `tests/it/functions.rs`), and the forms that create,
//! update and delete one.

use std::sync::{Arc, Mutex};

use openshop::builder::{DiscountMode, Field, Function, FunctionOwner, FunctionType};
use openshop::server::ServerOptions;
use openshop::testing::{MockTransport, OutboundRequest, TestContext, TestOptions};
use openshop::{OpenShopConfig, UiSource, Value, json, validators};
use reqwest::header::AUTHORIZATION;
use reqwest::{Method, StatusCode};

use super::{Reply, fetch, form, request, send};
use crate::common::{decode_uri_component, graphql_requests};

const SHOP: &str = "functions-pages.myshopify.com";
const DISCOUNT_1: &str = "/functions/volume-discount/gid%3A%2F%2Fshopify%2FDiscountNode%2F1";
const DELIVERY_1: &str = "/functions/delivery-rules/gid%3A%2F%2Fshopify%2FDeliveryCustomization%2F1";
const CART_1: &str = "/functions/cart-transform/gid%3A%2F%2Fshopify%2FCartTransform%2F1";

fn config() -> OpenShopConfig {
    OpenShopConfig::builder()
        .function(
            "discount",
            Function::new(FunctionType::Discount, "volume-discount")
                .modes([DiscountMode::Automatic, DiscountMode::Code])
                .owner(FunctionOwner::dynamic_title(|config| {
                    format!("Volume {}", config["percent"])
                }))
                .field(
                    "percent",
                    Field::number("Percent").validate(validators::range(Some(0.0), None)),
                ),
        )
        .function(
            "cartTransform",
            Function::new(FunctionType::CartTransform, "cart-transform")
                .label("Bundle cart transform")
                .field("message", Field::text("Message")),
        )
        .function(
            "deliveryCustomization",
            Function::new(FunctionType::DeliveryCustomization, "delivery-rules")
                .owner(FunctionOwner::new("Delivery rules"))
                .field("message", Field::text("Message"))
                .field("token", Field::password("Token").optional()),
        )
        .build()
        .unwrap()
}

/// What the mock of Shopify answers.
#[derive(Default)]
struct ShopifyState {
    /// `userErrors` of the next mutation only.
    next_user_errors: Option<Value>,
    cart_transforms: Vec<Value>,
    /// Every call fails.
    down: bool,
}

type Shopify = Arc<Mutex<ShopifyState>>;

const MUTATIONS: [&str; 9] = [
    "discountAutomaticAppCreate",
    "discountCodeAppCreate",
    "cartTransformCreate",
    "deliveryCustomizationCreate",
    "discountAutomaticAppUpdate",
    "deliveryCustomizationUpdate",
    "discountAutomaticDelete",
    "discountCodeDelete",
    "deliveryCustomizationDelete",
];

fn graphql_data(state: &mut ShopifyState, query: &str) -> Value {
    if query.contains("ListDiscountInstances") {
        return json!({ "discountNodes": { "nodes": [
            {
                "id": "gid://shopify/DiscountNode/1",
                "discount": { "title": "Volume 10", "status": "ACTIVE", "startsAt": "2026-01-01T00:00:00Z" },
                "metafield": { "value": json!({ "percent": 10 }).to_string() },
            },
            {
                "id": "gid://shopify/DiscountNode/2",
                "discount": { "title": "Legacy <old>", "status": "EXPIRED" },
                "metafield": null,
            },
        ] } });
    }
    if query.contains("ListShopifyFunctions") {
        return json!({ "shopifyFunctions": { "nodes": [{ "id": "function-cart", "handle": "cart-transform" }] } });
    }
    if query.contains("ListCartTransformInstances") {
        return json!({ "cartTransforms": { "nodes": state.cart_transforms } });
    }
    if query.contains("query ListInstances") {
        return json!({ "deliveryCustomizations": { "nodes": [{
            "id": "gid://shopify/DeliveryCustomization/1",
            "title": "Delivery rules",
            "enabled": true,
            "metafield": { "value": json!({ "message": "ship it", "token": "s3cret" }).to_string() },
        }] } });
    }
    let key = MUTATIONS
        .into_iter()
        .find(|key| query.contains(key))
        .unwrap_or("unknownMutation");
    let user_errors = state.next_user_errors.take().unwrap_or_else(|| json!([]));
    json!({ key: { "userErrors": user_errors } })
}

fn mock_shopify(ctx: &TestContext) -> Shopify {
    let state = Shopify::default();
    let shared = state.clone();
    ctx.transport.respond_with(move |request| {
        let mut state = shared.lock().unwrap();
        if !request.url.ends_with("/graphql.json") {
            return Ok(MockTransport::json(404, json!({ "error": "not mocked" })));
        }
        if state.down {
            return Ok(MockTransport::json(503, json!({ "errors": "Service Unavailable" })));
        }
        let query = request.body["query"].as_str().unwrap_or_default();
        Ok(MockTransport::json(
            200,
            json!({ "data": graphql_data(&mut state, query) }),
        ))
    });
    state
}

async fn setup(config: OpenShopConfig) -> (TestContext, Shopify) {
    let ctx = TestContext::with_options(
        config,
        TestOptions {
            shop: SHOP.to_string(),
            access_token: Some("offline-token".to_string()),
            server: ServerOptions {
                ui: UiSource::Server,
                dev: false,
            },
            ..Default::default()
        },
    )
    .await
    .expect("test context");
    let shopify = mock_shopify(&ctx);
    (ctx, shopify)
}

fn cart_transform(id: u32) -> Value {
    json!({
        "id": format!("gid://shopify/CartTransform/{id}"),
        "functionId": "function-cart",
        "blockOnFailure": true,
        "metafield": { "value": json!({ "message": "bundle" }).to_string() },
    })
}

fn last_call(ctx: &TestContext) -> OutboundRequest {
    graphql_requests(ctx).pop().expect("a GraphQL call")
}

fn query_of(call: &OutboundRequest) -> &str {
    call.body["query"].as_str().expect("GraphQL document")
}

/// The configuration stored in the metafield of a create or update mutation.
fn stored_config(call: &OutboundRequest) -> Value {
    let variables = &call.body["variables"];
    let metafields = variables.get("metafields").unwrap_or(&variables["input"]["metafields"]);
    serde_json::from_str(metafields[0]["value"].as_str().expect("metafield value")).unwrap()
}

/// A toast, as the script reads it.
fn toast(reply: &Reply, name: &str) -> Option<String> {
    reply.header(name).map(decode_uri_component)
}

#[tokio::test]
async fn the_functions_of_the_app_are_listed() {
    let (ctx, _shopify) = setup(config()).await;

    let page = send(fetch(&ctx, Method::GET, "/functions")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(body.contains(r#"<s-page heading="Functions">"#), "{body}");
    assert!(body.contains("<s-heading>Bundle cart transform</s-heading>"), "{body}");
    assert!(body.contains("<s-badge>Cart Transform</s-badge>"), "{body}");
    assert!(
        body.contains(r#"<s-badge tone="info">automatic, code</s-badge>"#),
        "{body}"
    );
    assert!(body.contains("2 config fields"), "{body}");
    assert!(
        body.contains(r#"<s-button href="/functions/delivery-rules">Manage</s-button>"#),
        "{body}"
    );
    // The functions are the app's: Shopify is not asked.
    assert!(graphql_requests(&ctx).is_empty());
    // A form the page does not have.
    let unknown = send(form(fetch(&ctx, Method::POST, "/functions"), "_action=create")).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    ctx.shutdown().await.unwrap();

    let (ctx, _shopify) = setup(OpenShopConfig::builder().build().unwrap()).await;
    let page = send(fetch(&ctx, Method::GET, "/functions")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("No functions configured"), "{}", page.body);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_instances_of_a_function_are_read_from_shopify() {
    let (ctx, shopify) = setup(config()).await;

    let page = send(fetch(&ctx, Method::GET, "/functions/volume-discount")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(body.contains(r#"<s-page heading="Discount">"#), "{body}");
    assert!(body.contains("2 instances — Discount"), "{body}");
    assert!(body.contains("<s-table-cell>Volume 10</s-table-cell>"), "{body}");
    assert!(
        body.contains("<s-table-cell>Legacy &lt;old&gt;</s-table-cell>"),
        "{body}"
    );
    assert!(body.contains(r#"<s-badge tone="success">ACTIVE</s-badge>"#), "{body}");
    assert!(body.contains(r#"<s-badge tone="warning">EXPIRED</s-badge>"#), "{body}");
    assert!(
        body.contains(r#"<s-text color="subdued">percent: 10</s-text>"#),
        "{body}"
    );
    assert!(
        body.contains(&format!(r#"href="{DISCOUNT_1}">See</s-button>"#)),
        "{body}"
    );
    assert!(
        body.contains(r#"href="/functions/volume-discount/new">Create instance</s-button>"#),
        "{body}"
    );
    assert!(query_of(&last_call(&ctx)).contains("ListDiscountInstances"));

    // A password is not written out.
    let delivery = send(fetch(&ctx, Method::GET, "/functions/delivery-rules")).await;
    assert!(
        delivery.body.contains(r#"<s-badge tone="success">Active</s-badge>"#),
        "{}",
        delivery.body
    );
    assert!(
        delivery.body.contains("message: ship it, token: ••••••••"),
        "{}",
        delivery.body
    );
    assert!(!delivery.body.contains("s3cret"), "{}", delivery.body);

    // A Cart Transform: one instance at most.
    shopify.lock().unwrap().cart_transforms = vec![cart_transform(1)];
    let cart = send(fetch(&ctx, Method::GET, "/functions/cart-transform")).await;
    assert!(cart.body.contains("1 instance — Cart Transform"), "{}", cart.body);
    assert!(
        cart.body
            .contains(r#"<s-table-header listSlot="primary">Label</s-table-header>"#),
        "{}",
        cart.body
    );
    assert!(
        cart.body.contains("<s-table-cell>Bundle cart transform</s-table-cell>"),
        "{}",
        cart.body
    );
    assert!(!cart.body.contains("Create"), "{}", cart.body);
    shopify.lock().unwrap().cart_transforms.clear();
    let none = send(fetch(&ctx, Method::GET, "/functions/cart-transform")).await;
    assert!(none.body.contains("No instances yet"), "{}", none.body);
    assert!(
        none.body
            .contains(r#"href="/functions/cart-transform/new">Create first instance</s-button>"#),
        "{}",
        none.body
    );

    // A function the app does not declare.
    let unknown = send(fetch(&ctx, Method::GET, "/functions/nope")).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert!(
        unknown
            .body
            .contains(r#"heading="The app declares no function &quot;nope&quot;.""#),
        "{}",
        unknown.body
    );
    let deeper = send(fetch(&ctx, Method::GET, "/functions/volume-discount/1/more")).await;
    assert_eq!(deeper.status, StatusCode::NOT_FOUND);

    // Shopify fails: the page says so, and a Cart Transform is not offered blindly.
    shopify.lock().unwrap().down = true;
    let failed = send(fetch(&ctx, Method::GET, "/functions/volume-discount")).await;
    assert_eq!(failed.status, StatusCode::OK);
    assert!(
        failed
            .body
            .contains(r#"<s-banner tone="critical">The instances could not be loaded from Shopify.</s-banner>"#),
        "{}",
        failed.body
    );
    assert!(failed.body.contains("Create instance"), "{}", failed.body);
    let cart = send(fetch(&ctx, Method::GET, "/functions/cart-transform")).await;
    assert!(
        cart.body.contains("The instances could not be loaded."),
        "{}",
        cart.body
    );
    assert!(!cart.body.contains("Create"), "{}", cart.body);
    let instance = send(fetch(&ctx, Method::GET, DELIVERY_1)).await;
    assert!(
        instance.body.contains("The instance could not be loaded."),
        "{}",
        instance.body
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_instance_is_created_from_its_form() {
    let (ctx, shopify) = setup(config()).await;

    let page = send(fetch(&ctx, Method::GET, "/functions/volume-discount/new")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(
        body.contains(
            r#"<form method="post"><input type="hidden" name="_action" value="create"><s-page heading="New Discount">"#
        ),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-select name="mode" label="Discount mode" value="automatic">"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-number-field name="config.percent" label="Percent" value="">"#),
        "{body}"
    );
    assert!(graphql_requests(&ctx).is_empty());

    // Created: to the instances, with a toast.
    let created = send(form(
        fetch(&ctx, Method::POST, "/functions/volume-discount/new"),
        "_action=create&mode=code&config.percent=10",
    ))
    .await;
    assert_eq!(created.status, StatusCode::NO_CONTENT, "{}", created.body);
    assert_eq!(
        created.header("x-openshop-location"),
        Some("/functions/volume-discount")
    );
    assert_eq!(toast(&created, "x-openshop-toast").as_deref(), Some("Instance created"));
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountCodeAppCreate"), "{}", query_of(&call));
    assert_eq!(call.body["variables"]["input"]["title"], "Volume 10");
    assert_eq!(stored_config(&call), json!({ "percent": 10 }));

    // A configuration refused field by field: the form again, Shopify not asked.
    let calls = graphql_requests(&ctx).len();
    let refused = send(form(
        fetch(&ctx, Method::POST, "/functions/volume-discount/new"),
        "_action=create&mode=code&config.percent=-1",
    ))
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refused.body.contains(
            r#"<s-number-field name="config.percent" label="Percent" value="-1" error="Must be at least 0 (was -1)">"#
        ),
        "{}",
        refused.body
    );
    assert!(
        refused.body.contains(r#"<s-option value="code" selected>"#),
        "{}",
        refused.body
    );
    assert!(
        refused
            .body
            .contains(r#"<s-banner tone="critical">Field &quot;percent&quot;: must be at least 0 (was -1)</s-banner>"#),
        "{}",
        refused.body
    );
    let required = send(form(
        fetch(&ctx, Method::POST, "/functions/volume-discount/new"),
        "_action=create&config.percent=",
    ))
    .await;
    assert_eq!(required.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        required.body.contains(r#"value="" error="Required""#),
        "{}",
        required.body
    );
    assert_eq!(graphql_requests(&ctx).len(), calls);

    // Shopify refuses it: its message in an error toast, the page stays.
    shopify.lock().unwrap().next_user_errors = Some(json!([
        { "field": ["input", "title"], "message": "Title is invalid" },
        { "field": null, "message": "Function is not deployed" },
    ]));
    let invalid = send(form(
        fetch(&ctx, Method::POST, "/functions/delivery-rules/new"),
        "_action=create&config.message=hi",
    ))
    .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        toast(&invalid, "x-openshop-toast-error").as_deref(),
        Some("Title is invalid, Function is not deployed")
    );
    assert!(invalid.body.is_empty());

    // A Cart Transform: whether it blocks the checkout, and one at most.
    let cart = send(form(
        fetch(&ctx, Method::POST, "/functions/cart-transform/new"),
        "_action=create&blockOnFailure=true&config.message=bundle",
    ))
    .await;
    assert_eq!(cart.status, StatusCode::NO_CONTENT, "{}", cart.body);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("cartTransformCreate"));
    assert_eq!(call.body["variables"]["blockOnFailure"], true);
    assert_eq!(stored_config(&call), json!({ "message": "bundle" }));

    shopify.lock().unwrap().next_user_errors =
        Some(json!([{ "field": ["blockOnFailure"], "message": "Not allowed here" }]));
    let named = send(form(
        fetch(&ctx, Method::POST, "/functions/cart-transform/new"),
        "_action=create&blockOnFailure=true&config.message=bundle",
    ))
    .await;
    assert_eq!(named.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        named
            .body
            .contains(r#"label="Block checkout when the Cart Transform fails" checked error="Not allowed here">"#),
        "{}",
        named.body
    );

    shopify.lock().unwrap().cart_transforms = vec![cart_transform(1)];
    let second = send(form(
        fetch(&ctx, Method::POST, "/functions/cart-transform/new"),
        "_action=create&config.message=again",
    ))
    .await;
    assert_eq!(second.status, StatusCode::CONFLICT);
    assert_eq!(
        toast(&second, "x-openshop-toast-error").as_deref(),
        Some("This Cart Transform already has an instance")
    );

    // Without the script: a redirect to where the script goes.
    let plain = send(form(
        request(&ctx, Method::POST, "/functions/delivery-rules/new").header(AUTHORIZATION, ctx.authorization_header()),
        "_action=create&config.message=plain",
    ))
    .await;
    assert_eq!(plain.status, StatusCode::SEE_OTHER);
    assert_eq!(plain.header("location"), Some("/functions/delivery-rules"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_instance_is_updated_and_deleted_from_its_page() {
    let (ctx, shopify) = setup(config()).await;

    let page = send(fetch(&ctx, Method::GET, DELIVERY_1)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(
        body.contains(r#"<input type="hidden" name="_action" value="update"><s-page heading="Edit instance">"#),
        "{body}"
    );
    assert!(
        body.contains(
            r#"<s-link slot="breadcrumb-actions" href="/functions/delivery-rules">Delivery Customization</s-link>"#
        ),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-text-field name="config.message" label="Message" value="ship it">"#),
        "{body}"
    );
    // The password saved is kept, never shown.
    assert!(body.contains(r#"placeholder="Saved; leave blank to keep""#), "{body}");
    assert!(!body.contains("s3cret"), "{body}");
    assert!(
        body.contains(r#"commandFor="delete-function-instance-modal" command="--show">Delete instance"#),
        "{body}"
    );
    assert!(
        body.contains(
            r#"<input type="hidden" name="_action" value="delete"><s-modal id="delete-function-instance-modal""#
        ),
        "{body}"
    );

    // Updated: the page again, with a toast; the blank password keeps the saved one.
    let updated = send(form(
        fetch(&ctx, Method::POST, DELIVERY_1),
        "_action=update&config.message=new+rules&config.token=",
    ))
    .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
    assert_eq!(toast(&updated, "x-openshop-toast").as_deref(), Some("Instance updated"));
    assert!(
        updated.body.contains(r#"<s-page heading="Edit instance">"#),
        "{}",
        updated.body
    );
    let calls = graphql_requests(&ctx);
    let update = calls
        .iter()
        .find(|call| query_of(call).contains("deliveryCustomizationUpdate"))
        .expect("an update");
    assert_eq!(update.body["variables"]["id"], "gid://shopify/DeliveryCustomization/1");
    assert_eq!(update.body["variables"]["input"]["title"], "Delivery rules");
    assert_eq!(
        stored_config(update),
        json!({ "message": "new rules", "token": "s3cret" })
    );
    let fresh = send(form(
        fetch(&ctx, Method::POST, DELIVERY_1),
        "_action=update&config.message=x&config.token=fresh",
    ))
    .await;
    assert_eq!(fresh.status, StatusCode::OK);
    let update = graphql_requests(&ctx)
        .into_iter()
        .rfind(|call| query_of(call).contains("deliveryCustomizationUpdate"))
        .unwrap();
    assert_eq!(stored_config(&update), json!({ "message": "x", "token": "fresh" }));

    // An instance Shopify does not list.
    let gone = send(fetch(
        &ctx,
        Method::GET,
        "/functions/delivery-rules/gid%3A%2F%2Fshopify%2FDeliveryCustomization%2F9",
    ))
    .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert!(gone.body.contains("This instance no longer exists."), "{}", gone.body);

    // Deleted: to the instances, with a toast.
    let deleted = send(form(fetch(&ctx, Method::POST, DELIVERY_1), "_action=delete")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    assert_eq!(deleted.header("x-openshop-location"), Some("/functions/delivery-rules"));
    assert_eq!(toast(&deleted, "x-openshop-toast").as_deref(), Some("Instance deleted"));
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("deliveryCustomizationDelete"));
    assert_eq!(
        call.body["variables"],
        json!({ "id": "gid://shopify/DeliveryCustomization/1" })
    );
    shopify.lock().unwrap().next_user_errors =
        Some(json!([{ "field": ["id"], "message": "Customization does not exist" }]));
    let refused = send(form(fetch(&ctx, Method::POST, DELIVERY_1), "_action=delete")).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        toast(&refused, "x-openshop-toast-error").as_deref(),
        Some("Customization does not exist")
    );

    // A discount of two modes: which one it is, in the modal of its deletion.
    let discount = send(fetch(&ctx, Method::GET, DISCOUNT_1)).await;
    assert_eq!(discount.status, StatusCode::OK, "{}", discount.body);
    assert_eq!(
        discount.body.matches(r#"<s-select name="mode""#).count(),
        2,
        "{}",
        discount.body
    );
    assert!(discount.body.contains(r#"value="10""#), "{}", discount.body);
    let code = send(form(fetch(&ctx, Method::POST, DISCOUNT_1), "_action=delete&mode=code")).await;
    assert_eq!(code.status, StatusCode::NO_CONTENT);
    let call = last_call(&ctx);
    assert!(query_of(&call).contains("discountCodeDelete"));
    assert_eq!(call.body["variables"], json!({ "id": "gid://shopify/DiscountNode/1" }));
    let without = send(form(fetch(&ctx, Method::POST, DISCOUNT_1), "_action=delete")).await;
    assert_eq!(without.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(without.body.contains("data-openshop-open"), "{}", without.body);
    assert!(
        without
            .body
            .contains(r#"error="Choose the kind of discount this instance is""#),
        "{}",
        without.body
    );
    let automatic = send(form(
        fetch(&ctx, Method::POST, DISCOUNT_1),
        "_action=update&mode=automatic&config.percent=15",
    ))
    .await;
    assert_eq!(automatic.status, StatusCode::OK);
    // The update, then the page again, read from Shopify.
    let calls = graphql_requests(&ctx);
    let [.., update, listing] = calls.as_slice() else {
        panic!("an update and a listing")
    };
    assert!(query_of(update).contains("discountAutomaticAppUpdate"));
    assert_eq!(stored_config(update), json!({ "percent": 15 }));
    assert!(query_of(listing).contains("ListDiscountInstances"));

    // A Cart Transform is deleted and created again, never updated.
    shopify.lock().unwrap().cart_transforms = vec![cart_transform(1)];
    let cart = send(fetch(&ctx, Method::GET, CART_1)).await;
    assert_eq!(cart.status, StatusCode::OK, "{}", cart.body);
    assert!(
        cart.body.contains("This function type does not support updates."),
        "{}",
        cart.body
    );
    assert!(
        cart.body.contains(r#"type="submit" disabled>Save</s-button>"#),
        "{}",
        cart.body
    );
    assert!(cart.body.contains("checked disabled"), "{}", cart.body);
    let calls = graphql_requests(&ctx).len();
    let update = send(form(
        fetch(&ctx, Method::POST, CART_1),
        "_action=update&config.message=x",
    ))
    .await;
    assert_eq!(update.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        toast(&update, "x-openshop-toast-error").as_deref(),
        Some("Function type \"cart-transform\" does not support update. Delete and recreate instead.")
    );
    assert_eq!(graphql_requests(&ctx).len(), calls);
    // An action of another page.
    let misplaced = send(form(fetch(&ctx, Method::POST, CART_1), "_action=create")).await;
    assert_eq!(misplaced.status, StatusCode::NOT_FOUND);
    ctx.shutdown().await.unwrap();
}
