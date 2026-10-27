//! `graphql!`: operations checked against Shopify's schema at
//! compile time, run by `ShopifyClient` with the token of the installation.

use crate::common::noop_flow;
use openshop::builder::Flow;
use openshop::shopify::input;
use openshop::testing::{TestContext, TestOptions};
use openshop::{OpenShopConfig, RunOutcome, ShopifyClient, Value, graphql, json};

/// A context whose shop is installed, so that it has a Shopify client.
async fn context_with(config: OpenShopConfig) -> TestContext {
    let options = TestOptions {
        access_token: Some("test-access-token".to_string()),
        ..Default::default()
    };
    TestContext::with_options(config, options).await.expect("test context")
}

async fn context() -> TestContext {
    context_with(
        OpenShopConfig::builder()
            .flow(noop_flow("sync"))
            .build()
            .expect("valid config"),
    )
    .await
}

fn shopify(ctx: &TestContext) -> ShopifyClient {
    ctx.app.shopify_client(&ctx.shop, "default")
}

/// Makes Shopify answer every operation with `data`.
fn respond(ctx: &TestContext, data: Value) {
    ctx.transport.respond_json(json!({ "data": data }));
}

/// The body of the last request sent to Shopify.
fn sent(ctx: &TestContext) -> Value {
    ctx.transport.last_request().expect("a request to Shopify").body
}

#[openshop::test(context())]
async fn an_inline_query_returns_typed_data(ctx: &TestContext) {
    respond(
        ctx,
        json!({ "orders": { "nodes": [
            { "id": "gid://shopify/Order/1", "name": "#1001" },
            { "id": "gid://shopify/Order/2", "name": "#1002" },
        ] } }),
    );
    let limit: u32 = 2;

    let data = shopify(ctx)
        .execute(graphql!(
            "query($first: Int!) { orders(first: $first) { nodes { id name } } }",
            first = limit
        ))
        .await
        .unwrap();

    let names: Vec<&str> = data.orders.nodes.iter().map(|order| order.name.as_str()).collect();
    assert_eq!(names, ["#1001", "#1002"]);
    assert_eq!(data.orders.nodes[0].id, "gid://shopify/Order/1");

    // Sent to the version of the schema, with the token of the installation.
    let request = ctx.transport.last_request().unwrap();
    assert_eq!(
        request.url,
        format!("https://{}/admin/api/2026-10/graphql.json", ctx.shop)
    );
    assert_eq!(request.header("x-shopify-access-token"), Some("test-access-token"));
    assert_eq!(request.body["variables"], json!({ "first": 2 }));
}

graphql! {
    /// Writes metafields and reports what Shopify refused.
    mod metafields_set = "mutation($metafields: [MetafieldsSetInput!]!) {
        metafieldsSet(metafields: $metafields) {
            metafields { id key }
            userErrors { field message code }
        }
    }";
    pub mod shop_name = "query ShopName { shop { name currencyCode } }";
}

#[openshop::test(context())]
async fn a_module_names_the_types_of_an_operation(ctx: &TestContext) {
    let shopify = shopify(ctx);
    respond(
        ctx,
        json!({ "metafieldsSet": {
            "metafields": [{ "id": "gid://shopify/Metafield/1", "key": "color" }],
            "userErrors": [{ "field": ["metafields", "0", "value"], "message": "is too long", "code": "TOO_LONG" }],
        } }),
    );

    let metafields = vec![input!(MetafieldsSetInput {
        ownerId: "gid://shopify/Product/1",
        namespace: "custom",
        key: "color",
        value: "blue",
        type: "single_line_text_field",
    })];
    let data: metafields_set::Data = shopify.execute(metafields_set::Variables { metafields }).await.unwrap();

    let payload = data.metafields_set.expect("a payload");
    assert_eq!(payload.metafields.unwrap_or_default()[0].key, "color");
    assert_eq!(payload.user_errors[0].message, "is too long");
    assert_eq!(
        sent(ctx)["variables"],
        json!({ "metafields": [{
            "ownerId": "gid://shopify/Product/1",
            "namespace": "custom",
            "key": "color",
            "value": "blue",
            "type": "single_line_text_field",
        }] })
    );

    respond(ctx, json!({ "shop": { "name": "Test shop", "currencyCode": "EUR" } }));
    let data = shopify.execute(shop_name::operation()).await.unwrap();
    assert_eq!(data.shop.name, "Test shop");
}

#[openshop::test(context())]
async fn an_operation_built_at_run_time_returns_its_data(ctx: &TestContext) {
    respond(ctx, json!({ "shop": { "name": "Test shop" } }));
    let shopify = shopify(ctx);

    let data = shopify.graphql("{ shop { name } }", ()).await.unwrap();
    assert_eq!(data, json!({ "shop": { "name": "Test shop" } }));
    assert_eq!(sent(ctx), json!({ "query": "{ shop { name } }" }));

    let data = shopify
        .graphql(
            "query($id: ID!) { shop { name } }",
            json!({ "id": "gid://shopify/Shop/1" }),
        )
        .await
        .unwrap();
    assert_eq!(data["shop"]["name"], "Test shop");
    assert_eq!(sent(ctx)["variables"], json!({ "id": "gid://shopify/Shop/1" }));
}

#[tokio::test]
async fn typed_data_is_a_step_output() {
    let flow = Flow::new("shopName", |ctx| async move {
        let data = ctx
            .step
            .run("fetch-shop", || async {
                ctx.shopify.execute(graphql!("{ shop { name currencyCode } }")).await
            })
            .await?;
        ctx.step
            .run("greet", || async {
                Ok(format!("{} sells in {}", data.shop.name, data.shop.currency_code))
            })
            .await?;
        Ok(())
    });
    let ctx = context_with(OpenShopConfig::builder().flow(flow).build().unwrap()).await;
    respond(&ctx, json!({ "shop": { "name": "Test shop", "currencyCode": "EUR" } }));

    assert_eq!(
        ctx.run_flow("shopName", json!({})).await.unwrap(),
        RunOutcome::Completed
    );

    let outputs: Vec<(String, Value)> =
        sqlx::query_as("SELECT step_name, output FROM step_results ORDER BY created_at, step_name")
            .fetch_all(&ctx.db)
            .await
            .unwrap();
    assert_eq!(
        outputs,
        [
            (
                "fetch-shop".to_string(),
                // Stored under the names of the GraphQL fields.
                json!({ "shop": { "name": "Test shop", "currencyCode": "EUR" } })
            ),
            ("greet".to_string(), json!("Test shop sells in EUR")),
        ]
    );
    ctx.shutdown().await.unwrap();
}

#[openshop::test(context())]
async fn an_answer_that_does_not_fit_the_operation_is_an_error(ctx: &TestContext) {
    let shopify = shopify(ctx);

    respond(
        ctx,
        json!({ "orders": { "nodes": [{ "id": "gid://shopify/Order/1" }] } }),
    );
    let error = shopify
        .execute(graphql!("{ orders(first: 1) { nodes { id name } } }"))
        .await
        .unwrap_err();
    let error = error.downcast::<openshop::shopify_api::Error>().unwrap();
    assert!(matches!(error, openshop::shopify_api::Error::Decode { .. }), "{error}");
    assert!(error.to_string().contains("orders.nodes[0]"), "{error}");

    ctx.transport
        .respond_json(json!({ "errors": [{ "message": "Field 'x' doesn't exist" }] }));
    let error = shopify
        .execute(graphql!("{ orders(first: 1) { nodes { id name } } }"))
        .await
        .unwrap_err();
    let error = error.downcast::<openshop::shopify_api::Error>().unwrap();
    assert!(matches!(error, openshop::shopify_api::Error::Graphql(_)), "{error}");
    assert!(error.to_string().contains("Field 'x' doesn't exist"), "{error}");
}

async fn context_without_token() -> TestContext {
    let config = OpenShopConfig::builder().flow(noop_flow("sync")).build().unwrap();
    TestContext::with_options(config, TestOptions::default()).await.unwrap()
}

#[openshop::test(context_without_token())]
async fn a_shop_without_a_token_is_not_installed(ctx: &TestContext) {
    let error = shopify(ctx).execute(graphql!("{ shop { name } }")).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "[openshop] No access token found for app \"default\" and shop \"{}\". Is the app installed?",
            ctx.shop
        )
    );
    assert!(ctx.transport.requests().is_empty());
}
