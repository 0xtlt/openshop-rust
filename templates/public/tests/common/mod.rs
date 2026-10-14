//! Shopify GraphQL answered by fixtures.
#![allow(dead_code)]

use openshop::testing::{MockTransport, TestContext};
use openshop::{Value, json};

pub fn graphql_fixture(query: &str) -> Value {
    if query.contains("GetLowStockVariants") {
        return json!({
            "productVariants": {
                "edges": [
                    { "node": { "displayName": "Canvas tote - Natural", "sku": null, "inventoryQuantity": 0 } },
                    { "node": { "displayName": "Linen shirt - M", "sku": "SHIRT-M", "inventoryQuantity": 2 } },
                ],
            },
        });
    }

    json!({})
}

/// Answers the Shopify GraphQL requests of a test with the fixtures.
pub fn answer_shopify(ctx: &TestContext) {
    ctx.transport.respond_with(|request| {
        let query = request.body["query"].as_str().unwrap_or_default();
        Ok(MockTransport::json(200, json!({ "data": graphql_fixture(query) })))
    });
}
