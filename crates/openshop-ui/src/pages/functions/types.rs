use crate::types::ConfigField;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

/// `GET /api/functions` response item.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionDef {
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub handle: String,
    #[serde(default)]
    pub modes: Option<Vec<String>>,
    #[serde(default)]
    pub supports_update: bool,
    #[serde(default)]
    pub singleton: Option<bool>,
    #[serde(default)]
    pub fields: IndexMap<String, ConfigField>,
}

/// `GET /api/functions/:handle/instances` response item, queried live from Shopify.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionInstance {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    /// `active` or `inactive`.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub block_on_failure: Option<bool>,
    #[serde(default)]
    pub config: IndexMap<String, Value>,
}

pub fn type_label(kind: &str) -> &str {
    match kind {
        "discount" => "Discount",
        "cart-transform" => "Cart Transform",
        "delivery-customization" => "Delivery Customization",
        "payment-customization" => "Payment Customization",
        "checkout-validation" => "Checkout Validation",
        "fulfillment-constraints" => "Fulfillment Constraints",
        other => other,
    }
}
