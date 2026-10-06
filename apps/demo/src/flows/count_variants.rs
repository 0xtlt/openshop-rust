use openshop::{Flow, Value, json};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CountVariantsInput {
    pub product_id: String,
}

const PRODUCT_FULL: &str = "#graphql
  query GetProductFull($id: ID!) {
    product(id: $id) {
      id
      title
      handle
      descriptionHtml
      productType
      vendor
      status
      tags
      variantsCount {
        count
      }
      totalInventory
      createdAt
      updatedAt
      publishedAt
      onlineStoreUrl
      options {
        name
        values
      }
      media(first: 10) {
        edges {
          node {
            ... on MediaImage {
              id
              image {
                url
                altText
                width
                height
              }
            }
          }
        }
      }
      variants(first: 20) {
        edges {
          node {
            id
            title
            sku
            price
            compareAtPrice
            inventoryQuantity
            selectedOptions {
              name
              value
            }
          }
        }
      }
      seo {
        title
        description
      }
    }
  }
";

pub fn count_variants() -> Flow {
    Flow::new("countVariants", |ctx| async move {
        let input: CountVariantsInput = ctx.input()?;

        let product: Value = ctx
            .step
            .run("fetch-product", || async {
                ctx.logger.info(
                    json!({ "productId": input.product_id }),
                    "Fetching full product details...",
                );
                let data = ctx
                    .shopify
                    .graphql(PRODUCT_FULL, Some(json!({ "id": input.product_id })))
                    .await?;
                Ok(data.get("product").cloned().unwrap_or(Value::Null))
            })
            .await?;

        ctx.logger.info(
            json!({
                "title": product.get("title"),
                "variants": product.pointer("/variantsCount/count"),
                "inventory": product.get("totalInventory"),
            }),
            "Product fetched",
        );

        let summary: Value = ctx
            .step
            .run("build-summary", || async {
                let edges = |pointer: &str| product.pointer(pointer).and_then(Value::as_array);
                let variant_skus: Option<Vec<Value>> = edges("/variants/edges").map(|variants| {
                    variants
                        .iter()
                        .map(|edge| {
                            json!({
                                "title": edge.pointer("/node/title"),
                                "sku": edge.pointer("/node/sku"),
                                "price": edge.pointer("/node/price"),
                                "inventory": edge.pointer("/node/inventoryQuantity"),
                            })
                        })
                        .collect()
                });

                Ok(json!({
                    "id": product.get("id"),
                    "title": product.get("title"),
                    "handle": product.get("handle"),
                    "vendor": product.get("vendor"),
                    "type": product.get("productType"),
                    "status": product.get("status"),
                    "tags": product.get("tags"),
                    "totalVariants": product.pointer("/variantsCount/count"),
                    "totalInventory": product.get("totalInventory"),
                    "options": product.get("options"),
                    "imageCount": edges("/media/edges").map_or(0, Vec::len),
                    "variantSkus": variant_skus,
                    "seo": product.get("seo"),
                    "createdAt": product.get("createdAt"),
                    "updatedAt": product.get("updatedAt"),
                }))
            })
            .await?;

        ctx.logger.info(
            json!({ "variantCount": summary["totalVariants"], "imageCount": summary["imageCount"] }),
            format!(
                "Product \"{}\" has {} variant(s)",
                summary["title"].as_str().unwrap_or_default(),
                summary["totalVariants"]
            ),
        );
        Ok(())
    })
    .input::<CountVariantsInput>()
}
