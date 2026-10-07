//! Boots the demo application for a test, with Shopify GraphQL answered by fixtures.
#![allow(dead_code)]

use openshop::testing::{MockTransport, TestContext, TestOptions};
use openshop::{Value, json};

pub fn graphql_fixture(query: &str) -> Value {
    if query.contains("GetRecentOrders") {
        return json!({
            "orders": {
                "edges": [{
                    "node": {
                        "id": "gid://shopify/Order/1",
                        "name": "#1001",
                        "customer": { "displayName": "Test Customer" },
                        "totalPriceSet": { "shopMoney": { "amount": "42.00", "currencyCode": "EUR" } },
                        "lineItems": { "edges": [{ "node": { "title": "Test product", "quantity": 1 } }] },
                    },
                }],
            },
        });
    }

    if query.contains("GetProductFull") {
        return json!({
            "product": {
                "id": "gid://shopify/Product/1",
                "title": "Test product",
                "handle": "test-product",
                "descriptionHtml": "<p>Test</p>",
                "productType": "Demo",
                "vendor": "OpenShop",
                "status": "ACTIVE",
                "tags": ["demo"],
                "variantsCount": { "count": 1 },
                "totalInventory": 12,
                "createdAt": "2026-01-01T00:00:00.000Z",
                "updatedAt": "2026-01-01T00:00:00.000Z",
                "publishedAt": "2026-01-01T00:00:00.000Z",
                "onlineStoreUrl": null,
                "options": [{ "name": "Title", "values": ["Default Title"] }],
                "media": { "edges": [] },
                "variants": {
                    "edges": [{
                        "node": {
                            "id": "gid://shopify/ProductVariant/1",
                            "title": "Default Title",
                            "sku": "SKU-1",
                            "price": "42.00",
                            "compareAtPrice": null,
                            "inventoryQuantity": 12,
                            "selectedOptions": [{ "name": "Title", "value": "Default Title" }],
                        },
                    }],
                },
                "seo": { "title": "Test product", "description": "Test" },
            },
        });
    }

    json!({})
}

pub async fn context() -> TestContext {
    let ctx = TestContext::with_options(
        openshop_demo::APP.config().expect("valid demo config"),
        TestOptions {
            access_token: Some("test-access-token".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("test context");

    ctx.transport.respond_with(|request| {
        let query = request.body["query"].as_str().unwrap_or_default();
        Ok(MockTransport::json(200, json!({ "data": graphql_fixture(query) })))
    });
    ctx
}
