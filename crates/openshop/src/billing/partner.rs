//! The plan of a shop as the Partner API's `activeSubscription` answers it:
//! the subscriptions of Shopify App Pricing.
//!
//! See <https://shopify.dev/docs/api/partner/2026-07/active-subscription>.

use chrono::{DateTime, Utc};
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Money, decimal, optional_decimal};

/// The name of the Admin API operation that reads the GIDs the Partner API
/// takes: `shop { id }` and `currentAppInstallation { app { id } }`. Each
/// shop is asked once per process.
pub const PARTNER_IDS_OPERATION: &str = "OpenShopPartnerIds";

/// The name of the Partner API operation that reads the plan of a shop:
/// `activeSubscription(appId:, shopId:)`.
pub const PARTNER_SUBSCRIPTION_OPERATION: &str = "OpenShopPartnerSubscription";

pub(crate) const PARTNER_IDS: &str = "query OpenShopPartnerIds { shop { id } currentAppInstallation { app { id } } }";

pub(crate) const PARTNER_SUBSCRIPTION: &str = "query OpenShopPartnerSubscription($appId: ID!, $shopId: ID!) { \
     activeSubscription(appId: $appId, shopId: $shopId) { \
       billingPeriod cancelAtEndOfCycle trialEndsAt legacySubscriptionId \
       currentBillingCycle { startTime endTime } \
       items { ...OpenShopSubscriptionItem } \
       pendingUpdate { billingPeriod legacySubscriptionId items { ...OpenShopSubscriptionItem } } \
     } } \
     fragment OpenShopSubscriptionItem on SubscriptionItem { \
       handle description \
       price { __typename active currency \
         ... on FlatRatePrice { amount } \
         ... on TieredPrice { tiersMode tiers { upTo amountPerUnit amount } } } \
       discount { amount percentage originalDiscountCycles remainingDiscountCycles discountEndsAt } \
       usage { quantity cost { amount currencyCode } } \
     }";

/// The subscription of a shop to the app with Shopify App Pricing, as the
/// Partner API's `activeSubscription` answers it. It is active: the Partner
/// API answers `null` without one. A subscription of the Billing API that
/// was not migrated to Shopify App Pricing is not one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPricingSubscription {
    /// `EVERY_30_DAYS` or `ANNUAL`.
    pub billing_period: String,
    /// Whether it ends at the end of the billing cycle.
    pub cancel_at_end_of_cycle: bool,
    /// When its trial ends; `None` without a trial, or after it.
    #[serde(default)]
    pub trial_ends_at: Option<DateTime<Utc>>,
    /// The billing cycle the shop is in; `None` during the trial.
    #[serde(default)]
    pub current_billing_cycle: Option<BillingCycle>,
    /// The plan, and the usage it charges for.
    pub items: Vec<AppPricingItem>,
    /// What changes at the next billing cycle, when something does.
    #[serde(default)]
    pub pending_update: Option<PendingUpdate>,
    /// The `AppSubscription` GID of the Admin API, for a subscription of the
    /// Billing API migrated to Shopify App Pricing; `None` for one created there.
    #[serde(default)]
    pub legacy_subscription_id: Option<String>,
}

impl AppPricingSubscription {
    /// The item of the plan: the first with a flat rate, else the first.
    pub fn plan_item(&self) -> Option<&AppPricingItem> {
        self.items
            .iter()
            .find(|item| matches!(item.price, AppPricingPrice::FlatRate(_)))
            .or_else(|| self.items.first())
    }
}

/// A billing cycle of a subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingCycle {
    pub start_time: DateTime<Utc>,
    /// When the next charge happens.
    pub end_time: DateTime<Utc>,
}

/// An item of a subscription: a plan, or an event of the App Events API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppPricingItem {
    /// The handle of the plan or the event.
    #[serde(default)]
    pub handle: Option<String>,
    /// The description of the plan or the event.
    #[serde(default)]
    pub description: Option<String>,
    pub price: AppPricingPrice,
    #[serde(default)]
    pub discount: Option<AppPricingDiscount>,
    /// The usage of the current billing cycle, for a tiered price.
    #[serde(default)]
    pub usage: Option<AppPricingUsage>,
}

/// The price of an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "__typename")]
pub enum AppPricingPrice {
    #[serde(rename = "FlatRatePrice")]
    FlatRate(FlatRatePrice),
    #[serde(rename = "TieredPrice")]
    Tiered(TieredPrice),
    /// A kind of price this version of OpenShop does not read.
    #[serde(other)]
    Other,
}

/// A recurring flat rate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlatRatePrice {
    /// `false` once the plan has another price.
    pub active: bool,
    /// A decimal written as text, `"29.00"`.
    #[serde(deserialize_with = "decimal")]
    pub amount: String,
    /// `USD`.
    pub currency: String,
}

/// A price by usage, in tiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TieredPrice {
    pub active: bool,
    pub currency: String,
    /// `GRADUATED` or `VOLUME`.
    pub tiers_mode: String,
    pub tiers: Vec<PriceTier>,
}

/// A tier of a [`TieredPrice`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceTier {
    /// The upper bound; `None` for the last tier.
    #[serde(default)]
    pub up_to: Option<u64>,
    #[serde(deserialize_with = "decimal")]
    pub amount_per_unit: String,
    #[serde(deserialize_with = "decimal")]
    pub amount: String,
}

/// A discount of an item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPricingDiscount {
    /// A fixed amount off, a decimal written as text.
    #[serde(default, deserialize_with = "optional_decimal")]
    pub amount: Option<String>,
    #[serde(default)]
    pub percentage: Option<f64>,
    /// `None` for a discount without an end.
    #[serde(default)]
    pub original_discount_cycles: Option<u32>,
    #[serde(default)]
    pub remaining_discount_cycles: Option<u32>,
    #[serde(default)]
    pub discount_ends_at: Option<DateTime<Utc>>,
}

/// The usage of an item in the current billing cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppPricingUsage {
    pub quantity: f64,
    pub cost: Money,
}

/// What changes at the next billing cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingUpdate {
    pub billing_period: String,
    pub items: Vec<AppPricingItem>,
    #[serde(default)]
    pub legacy_subscription_id: Option<String>,
}

/// The GIDs the Partner API names an app and a shop by, as the Admin API
/// answers [`PARTNER_IDS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PartnerIds {
    /// `gid://shopify/App/…`.
    pub(crate) app: String,
    /// `gid://shopify/Shop/…`.
    pub(crate) shop: String,
}

pub(crate) fn read_partner_ids(data: Value) -> anyhow::Result<PartnerIds> {
    #[derive(Deserialize)]
    struct Id {
        id: String,
    }
    #[derive(Deserialize)]
    struct Installation {
        app: Id,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Data {
        shop: Id,
        current_app_installation: Installation,
    }
    let data: Data = serde_json::from_value(data)
        .map_err(|error| anyhow::anyhow!("[openshop] Unexpected answer to the IDs of the Partner API: {error}"))?;
    Ok(PartnerIds {
        app: data.current_app_installation.app.id,
        shop: data.shop.id,
    })
}

/// The subscription of the `data` of [`PARTNER_SUBSCRIPTION`]: `None` for
/// `null`, a shop without a plan.
pub(crate) fn read_partner_subscription(data: Value) -> anyhow::Result<Option<AppPricingSubscription>> {
    fn subscription<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<AppPricingSubscription>, D::Error> {
        Option::deserialize(deserializer)
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Data {
        #[serde(deserialize_with = "subscription")]
        active_subscription: Option<AppPricingSubscription>,
    }
    serde_json::from_value::<Data>(data)
        .map(|data| data.active_subscription)
        .map_err(|error| anyhow::anyhow!("[openshop] Unexpected answer to activeSubscription: {error}"))
}

/// The `data` the Partner API answers [`PARTNER_SUBSCRIPTION`] with: what
/// [`read_partner_subscription`] reads back.
#[cfg(feature = "testing")]
pub(crate) fn partner_subscription_data(subscription: Option<&AppPricingSubscription>) -> Value {
    serde_json::json!({ "activeSubscription": subscription })
}

/// The `data` the Admin API answers [`PARTNER_IDS`] with.
#[cfg(feature = "testing")]
pub(crate) fn partner_ids_data(app: &str, shop: &str) -> Value {
    serde_json::json!({ "shop": { "id": shop }, "currentAppInstallation": { "app": { "id": app } } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The example of shopify.dev, `activeSubscription` (2026-07).
    fn example() -> Value {
        json!({
            "activeSubscription": {
                "shop": { "id": "gid://shopify/Shop/5678", "myshopifyDomain": "example.myshopify.com" },
                "billingPeriod": "EVERY_30_DAYS",
                "cancelAtEndOfCycle": false,
                "trialEndsAt": null,
                "currentBillingCycle": { "startTime": "2026-04-01T00:00:00Z", "endTime": "2026-05-01T00:00:00Z" },
                "items": [{
                    "handle": "pro_plan",
                    "description": "Pro plan",
                    "price": { "__typename": "FlatRatePrice", "active": true, "currency": "USD", "amount": "29.00" },
                    "discount": null,
                    "usage": null
                }],
                "pendingUpdate": null,
                "legacySubscriptionId": "gid://shopify/AppSubscription/987654321"
            }
        })
    }

    #[test]
    fn reads_the_example_of_the_reference() {
        let subscription = read_partner_subscription(example()).unwrap().unwrap();
        assert_eq!(subscription.billing_period, "EVERY_30_DAYS");
        assert_eq!(
            subscription.current_billing_cycle.as_ref().unwrap().end_time.to_rfc3339(),
            "2026-05-01T00:00:00+00:00"
        );
        let plan = subscription.plan_item().unwrap();
        assert_eq!(plan.handle.as_deref(), Some("pro_plan"));
        assert_eq!(
            plan.price,
            AppPricingPrice::FlatRate(FlatRatePrice {
                active: true,
                amount: "29.00".into(),
                currency: "USD".into()
            })
        );
        assert_eq!(
            subscription.legacy_subscription_id.as_deref(),
            Some("gid://shopify/AppSubscription/987654321")
        );

        assert_eq!(read_partner_subscription(json!({ "activeSubscription": null })).unwrap(), None);
        assert!(read_partner_subscription(json!({})).is_err());
    }

    #[test]
    fn reads_tiers_discounts_and_usage() {
        let subscription = read_partner_subscription(json!({ "activeSubscription": {
            "billingPeriod": "ANNUAL", "cancelAtEndOfCycle": true, "trialEndsAt": "2026-11-16T10:00:00Z",
            "currentBillingCycle": null, "legacySubscriptionId": null,
            "items": [
                { "handle": "orders", "description": "Orders",
                  "price": { "__typename": "TieredPrice", "active": true, "currency": "EUR", "tiersMode": "GRADUATED",
                             "tiers": [{ "upTo": 100, "amountPerUnit": "0.10", "amount": 0 }, { "upTo": null, "amountPerUnit": "0.05", "amount": "0" }] },
                  "discount": null, "usage": { "quantity": 12.5, "cost": { "amount": "1.25", "currencyCode": "EUR" } } },
                { "handle": "plus", "description": "Plus",
                  "price": { "__typename": "FlatRatePrice", "active": true, "currency": "EUR", "amount": "99.00" },
                  "discount": { "amount": null, "percentage": 20.0, "originalDiscountCycles": 3, "remainingDiscountCycles": 2, "discountEndsAt": null },
                  "usage": null },
                { "handle": null, "description": null, "price": { "__typename": "FuturePrice", "active": true, "currency": "EUR" } }
            ],
            "pendingUpdate": { "billingPeriod": "EVERY_30_DAYS", "legacySubscriptionId": null, "items": [] }
        } }))
        .unwrap()
        .unwrap();
        // The item of the plan is the one with a flat rate.
        assert_eq!(subscription.plan_item().unwrap().handle.as_deref(), Some("plus"));
        let AppPricingPrice::Tiered(tiered) = &subscription.items[0].price else {
            panic!("a tiered price");
        };
        assert_eq!(tiered.tiers[0].amount, "0");
        assert_eq!(tiered.tiers[1].up_to, None);
        assert_eq!(subscription.items[0].usage.as_ref().unwrap().cost.amount, "1.25");
        assert_eq!(subscription.items[1].discount.as_ref().unwrap().percentage, Some(20.0));
        assert_eq!(subscription.items[2].price, AppPricingPrice::Other);
        assert_eq!(subscription.pending_update.as_ref().unwrap().billing_period, "EVERY_30_DAYS");
    }

    #[test]
    fn reads_the_ids_of_the_admin_api() {
        let ids = read_partner_ids(json!({
            "shop": { "id": "gid://shopify/Shop/1" },
            "currentAppInstallation": { "app": { "id": "gid://shopify/App/2" } }
        }))
        .unwrap();
        assert_eq!(
            ids,
            PartnerIds {
                app: "gid://shopify/App/2".into(),
                shop: "gid://shopify/Shop/1".into()
            }
        );
        assert!(read_partner_ids(json!({ "shop": null })).is_err());
    }

    #[test]
    fn the_operations_are_named_as_the_tests_answer_them() {
        let name = |query: &str| {
            crate::testing::GraphqlCall::of(&crate::testing::OutboundRequest {
                method: "POST",
                url: "https://partners.shopify.com/1234567/api/2026-10/graphql.json".to_string(),
                headers: Vec::new(),
                body: json!({ "query": query }),
            })
            .and_then(|call| call.operation)
        };
        assert_eq!(name(PARTNER_IDS).as_deref(), Some(PARTNER_IDS_OPERATION));
        assert_eq!(name(PARTNER_SUBSCRIPTION).as_deref(), Some(PARTNER_SUBSCRIPTION_OPERATION));
    }

    #[test]
    fn the_fixture_of_the_tests_reads_back_as_it_was_written() {
        let written = read_partner_subscription(example()).unwrap();
        let read = read_partner_subscription(partner_subscription_data(written.as_ref())).unwrap();
        assert_eq!(read, written);
        assert_eq!(read_partner_subscription(partner_subscription_data(None)).unwrap(), None);
        let ids = read_partner_ids(partner_ids_data("gid://shopify/App/1", "gid://shopify/Shop/2")).unwrap();
        assert_eq!(ids.shop, "gid://shopify/Shop/2");
    }
}
