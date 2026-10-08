//! `graphql!`: operations checked against Shopify's schema at compile time,
//! with typed variables and a typed result.

mod common;

use common::noop_flow;
use openshop::builder::Flow;
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

async fn shopify(ctx: &TestContext) -> ShopifyClient {
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
    let shopify = shopify(ctx).await;
    respond(
        ctx,
        json!({ "orders": { "nodes": [
            { "id": "gid://shopify/Order/1", "name": "#1001" },
            { "id": "gid://shopify/Order/2", "name": "#1002" },
        ] } }),
    );
    let limit: u32 = 2;

    let data = shopify
        .execute(graphql!(
            "query($first: Int!) { orders(first: $first) { nodes { id name } } }",
            first = limit
        ))
        .await
        .unwrap();

    let names: Vec<&str> = data.orders.nodes.iter().map(|order| order.name.as_str()).collect();
    assert_eq!(names, ["#1001", "#1002"]);
    assert_eq!(data.orders.nodes[0].id, "gid://shopify/Order/1");
    assert_eq!(
        sent(ctx),
        json!({
            "query": "query($first: Int!) { orders(first: $first) { nodes { id name } } }",
            "variables": { "first": 2 },
        })
    );
}

#[openshop::test(context())]
async fn nullability_lists_aliases_and_scalars_follow_the_schema(ctx: &TestContext) {
    let shopify = shopify(ctx).await;
    respond(
        ctx,
        json!({ "order": {
            "id": "gid://shopify/Order/1",
            "note": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "tags": ["vip", "b2b"],
            "displayFinancialStatus": "PAID",
            "total": { "shopMoney": { "amount": "42.50", "currencyCode": "EUR" } },
            "customer": null,
            "metafield": { "jsonValue": { "gift": true }, "type": "json" },
            "lineItems": { "nodes": [{ "title": "Shirt", "quantity": 2 }] },
        } }),
    );

    let data = shopify
        .execute(graphql!(
            r#"#graphql
            query Order($id: ID!, $namespace: String) {
              order(id: $id) {
                id
                note
                createdAt
                tags
                displayFinancialStatus
                total: totalPriceSet { shopMoney { amount currencyCode } }
                customer { displayName }
                metafield(namespace: $namespace, key: "settings") { jsonValue type }
                lineItems(first: 5) { nodes { title quantity } }
              }
            }"#,
            id = "gid://shopify/Order/1",
            namespace = "custom",
        ))
        .await
        .unwrap();

    // `order` is nullable in the schema.
    let order = data.order.expect("an order");
    let note: Option<String> = order.note;
    assert_eq!(note, None);
    let created_at: String = order.created_at;
    assert_eq!(created_at, "2026-01-01T00:00:00Z");
    let tags: Vec<String> = order.tags;
    assert_eq!(tags, ["vip", "b2b"]);
    // Enum values are strings.
    let status: Option<String> = order.display_financial_status;
    assert_eq!(status.as_deref(), Some("PAID"));
    assert_eq!(order.total.shop_money.amount, "42.50");
    assert_eq!(order.total.shop_money.currency_code, "EUR");
    assert!(order.customer.is_none());
    let metafield = order.metafield.expect("a metafield");
    let json_value: Value = metafield.json_value;
    assert_eq!(json_value, json!({ "gift": true }));
    // `type` is a Rust keyword.
    assert_eq!(metafield.r#type, "json");
    let quantity: i64 = order.line_items.nodes[0].quantity;
    assert_eq!(quantity, 2);

    assert_eq!(
        sent(ctx)["variables"],
        json!({ "id": "gid://shopify/Order/1", "namespace": "custom" })
    );
}

#[openshop::test(context())]
async fn fragments_on_other_types_are_optional_fields(ctx: &TestContext) {
    let shopify = shopify(ctx).await;
    respond(
        ctx,
        json!({ "nodes": [
            { "__typename": "Product", "id": "gid://shopify/Product/1", "title": "Shirt" },
            { "__typename": "Order", "id": "gid://shopify/Order/1", "name": "#1001", "createdAt": "2026-01-01T00:00:00Z" },
            null,
        ] }),
    );
    let ids = vec![
        "gid://shopify/Product/1",
        "gid://shopify/Order/1",
        "gid://shopify/Order/404",
    ];

    let data = shopify
        .execute(graphql!(
            "query Nodes($ids: [ID!]!) {
               nodes(ids: $ids) {
                 __typename
                 id
                 ... on Product { title }
                 ... on Order { name ...OrderDates }
               }
             }
             fragment OrderDates on Order { createdAt }",
            ids = &ids
        ))
        .await
        .unwrap();

    let [Some(product), Some(order), None] = data.nodes.as_slice() else {
        panic!("two nodes and a missing one, got {:?}", data.nodes);
    };
    assert_eq!(product.typename, "Product");
    // `id` belongs to the interface: always there.
    assert_eq!(product.id, "gid://shopify/Product/1");
    assert_eq!(product.title.as_deref(), Some("Shirt"));
    assert_eq!(product.name, None);
    assert_eq!(order.typename, "Order");
    assert_eq!(order.title, None);
    assert_eq!(order.name.as_deref(), Some("#1001"));
    assert_eq!(order.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    assert_eq!(sent(ctx)["variables"], json!({ "ids": ids }));
}

#[openshop::test(context())]
async fn optional_variables_can_be_left_out(ctx: &TestContext) {
    let shopify = shopify(ctx).await;
    respond(
        ctx,
        json!({ "orders": {
            "nodes": [{ "id": "gid://shopify/Order/1" }],
            "pageInfo": { "hasNextPage": true, "endCursor": "cursor-1" },
        } }),
    );
    let data = shopify
        .execute(graphql!(
            "query($first: Int = 10, $after: String, $withName: Boolean!) {
               orders(first: $first, after: $after) {
                 nodes { id name @include(if: $withName) }
                 pageInfo { hasNextPage endCursor }
               }
             }",
            withName = false
        ))
        .await
        .unwrap();

    // A field under `@include` may be absent.
    let name: &Option<String> = &data.orders.nodes[0].name;
    assert_eq!(*name, None);
    assert!(data.orders.page_info.has_next_page);
    assert_eq!(data.orders.page_info.end_cursor.as_deref(), Some("cursor-1"));
    assert_eq!(sent(ctx)["variables"], json!({ "withName": false }));

    // A nullable variable takes a bare value, an `Option`, or nothing.
    let cursor: Option<String> = data.orders.page_info.end_cursor;
    shopify
        .execute(graphql!(
            "query($first: Int = 10, $after: String, $withName: Boolean!) {
               orders(first: $first, after: $after) {
                 nodes { id name @include(if: $withName) }
                 pageInfo { hasNextPage endCursor }
               }
             }",
            withName = true,
            after = cursor,
            first = 5_usize,
        ))
        .await
        .unwrap();
    assert_eq!(
        sent(ctx)["variables"],
        json!({ "withName": true, "after": "cursor-1", "first": 5 })
    );
}

graphql! {
    /// Writes metafields and reports what Shopify refused.
    mod metafields_set = "mutation($metafields: [MetafieldsSetInput!]!) {
        metafieldsSet(metafields: $metafields) {
            metafields { id key }
            userErrors { field message code }
        }
    }";
}

graphql! {
    pub mod shop_name = "{ shop { name currencyCode } }";
}

#[openshop::test(context())]
async fn a_module_names_the_types_of_an_operation(ctx: &TestContext) {
    let shopify = shopify(ctx).await;
    respond(
        ctx,
        json!({ "metafieldsSet": {
            "metafields": [{ "id": "gid://shopify/Metafield/1", "key": "color" }],
            "userErrors": [{ "field": ["metafields", "0", "value"], "message": "is too long", "code": "TOO_LONG" }],
        } }),
    );

    let variables = metafields_set::Variables {
        metafields: vec![metafields_set::MetafieldsSetInput {
            owner_id: "gid://shopify/Product/1".to_string(),
            key: "color".to_string(),
            value: "blue".to_string(),
            namespace: Some("custom".to_string()),
            ..Default::default()
        }],
    };
    let data: metafields_set::Data = shopify.execute(metafields_set::operation(variables)).await.unwrap();

    let payload = data.metafields_set.expect("a payload");
    let written: Vec<metafields_set::DataMetafieldsSetMetafields> = payload.metafields.unwrap_or_default();
    assert_eq!(written[0].key, "color");
    let error = &payload.user_errors[0];
    assert_eq!(error.message, "is too long");
    assert_eq!(error.code.as_deref(), Some("TOO_LONG"));
    assert_eq!(
        error.field.as_deref(),
        Some(&["metafields".to_string(), "0".to_string(), "value".to_string()][..])
    );

    let request = sent(ctx);
    assert_eq!(request["query"], metafields_set::QUERY);
    // Fields left to `None` are not sent.
    assert_eq!(
        request["variables"],
        json!({ "metafields": [{
            "ownerId": "gid://shopify/Product/1",
            "key": "color",
            "value": "blue",
            "namespace": "custom",
        }] })
    );

    respond(ctx, json!({ "shop": { "name": "Test shop", "currencyCode": "EUR" } }));
    let data = shopify.execute(shop_name::operation()).await.unwrap();
    assert_eq!(
        (data.shop.name.as_str(), data.shop.currency_code.as_str()),
        ("Test shop", "EUR")
    );
    assert_eq!(sent(ctx), json!({ "query": "{ shop { name currencyCode } }" }));
}

#[openshop::test(context())]
async fn an_inline_operation_takes_input_objects_as_any_serializable_value(ctx: &TestContext) {
    let shopify = shopify(ctx).await;
    respond(ctx, json!({ "metafieldsSet": { "userErrors": [] } }));
    let metafields = json!([{ "ownerId": "gid://shopify/Product/1", "key": "color", "value": "blue" }]);

    let data = shopify
        .execute(graphql!(
            "mutation($metafields: [MetafieldsSetInput!]!) {
               metafieldsSet(metafields: $metafields) { userErrors { message } }
             }",
            metafields = &metafields
        ))
        .await
        .unwrap();

    assert!(data.metafields_set.expect("a payload").user_errors.is_empty());
    assert_eq!(sent(ctx)["variables"], json!({ "metafields": metafields }));
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
    let shopify = shopify(ctx).await;

    respond(
        ctx,
        json!({ "orders": { "nodes": [{ "id": "gid://shopify/Order/1" }] } }),
    );
    let error = shopify
        .execute(graphql!("{ orders(first: 1) { nodes { id name } } }"))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "[openshop] Shopify's answer does not match the operation at `orders.nodes[0]`: missing field `name`"
    );

    ctx.transport
        .respond_json(json!({ "errors": [{ "message": "Throttled" }] }));
    let error = shopify
        .execute(graphql!("{ orders(first: 1) { nodes { id name } } }"))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "[openshop] GraphQL errors: Throttled");
}

#[test]
fn the_bundled_schema_is_the_one_of_the_default_api_version() {
    // `ctx.shopify` calls SHOPIFY_API_VERSION: operations must be checked against it.
    assert_eq!(
        openshop_graphql::Schema::bundled().api_version,
        openshop::shopify::SHOPIFY_API_VERSION
    );
}
