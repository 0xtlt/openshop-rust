use openshop::{FlowContext, Result, Value, bail, flow, graphql, json};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CountVariantsInput {
    pub product_id: String,
}

#[flow("countVariants")]
pub async fn count_variants(ctx: FlowContext, input: CountVariantsInput) -> Result<()> {
    let product = ctx
        .step
        .run("fetch-product", || async {
            ctx.logger.info(
                json!({ "productId": input.product_id }),
                "Fetching full product details...",
            );
            let data = ctx
                .shopify
                .execute(graphql!(
                    "#graphql
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
                    ",
                    id = &input.product_id
                ))
                .await?;
            Ok(data.product)
        })
        .await?;
    let Some(product) = product else {
        bail!("Product {} does not exist", input.product_id);
    };
    let total_variants = product.variants_count.as_ref().map(|variants| variants.count);

    ctx.logger.info(
        json!({
            "title": product.title,
            "variants": total_variants,
            "inventory": product.total_inventory,
        }),
        "Product fetched",
    );

    let summary: Value = ctx
        .step
        .run("build-summary", || async {
            let variant_skus: Vec<Value> = product
                .variants
                .edges
                .iter()
                .map(|edge| {
                    json!({
                        "title": edge.node.title,
                        "sku": edge.node.sku,
                        "price": edge.node.price,
                        "inventory": edge.node.inventory_quantity,
                    })
                })
                .collect();

            Ok(json!({
                "id": product.id,
                "title": product.title,
                "handle": product.handle,
                "vendor": product.vendor,
                "type": product.product_type,
                "status": product.status,
                "tags": product.tags,
                "totalVariants": total_variants,
                "totalInventory": product.total_inventory,
                "options": product.options,
                "imageCount": product.media.edges.len(),
                "variantSkus": variant_skus,
                "seo": product.seo,
                "createdAt": product.created_at,
                "updatedAt": product.updated_at,
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
}
