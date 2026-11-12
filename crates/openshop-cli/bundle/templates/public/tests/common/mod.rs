//! Shopify GraphQL answered by fixtures.
#![allow(dead_code)]

use openshop::testing::TestContext;
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

/// The operations the flows send, by the name they give them in `graphql!`.
const OPERATIONS: &[&str] = &["GetLowStockVariants"];

/// Answers the operations of the flows with their fixtures, by name: an
/// operation a test does not answer fails with its name.
pub fn answer_shopify(ctx: &TestContext) {
    for operation in OPERATIONS {
        ctx.shopify.on(operation, graphql_fixture(operation));
    }
}
