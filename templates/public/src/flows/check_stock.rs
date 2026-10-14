use openshop::prelude::*;
use serde::{Deserialize, Serialize};

use crate::models::stock_settings::StockSettings;

/// The step whose output is the variants that run low: what the home of the
/// admin shows of the last check.
pub const FIND_LOW_STOCK: &str = "find-low-stock";

/// A variant that runs low, as the step stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LowStockVariant {
    pub name: String,
    pub sku: Option<String>,
    pub available: i64,
}

/// Looks for the variants of the shop under the threshold its merchant chose.
#[flow("checkStock")]
pub async fn check_stock(ctx: FlowContext) -> Result<()> {
    let low: Vec<LowStockVariant> = ctx
        .step
        .run(FIND_LOW_STOCK, async || {
            let threshold = StockSettings::threshold_of(&ctx).await?;
            ctx.logger.info_with(
                "Looking for variants that run low...",
                json!({ "threshold": threshold }),
            );

            // Checked against Shopify's schema when the crate compiles; `data` is typed.
            let data = ctx
                .shopify
                .execute(graphql!(
                    "
                      query GetLowStockVariants($query: String!) {
                        productVariants(first: 50, query: $query, sortKey: INVENTORY_QUANTITY) {
                          edges {
                            node {
                              displayName
                              sku
                              inventoryQuantity
                            }
                          }
                        }
                      }
                    ",
                    query = format!("inventory_quantity:<{threshold}")
                ))
                .await?;

            Ok(data
                .product_variants
                .edges
                .into_iter()
                .map(|edge| LowStockVariant {
                    name: edge.node.display_name,
                    sku: edge.node.sku,
                    available: edge.node.inventory_quantity.unwrap_or_default(),
                })
                .collect())
        })
        .await?;

    ctx.logger
        .info_with(format!("{} variants run low", low.len()), json!({ "count": low.len() }));
    Ok(())
}
