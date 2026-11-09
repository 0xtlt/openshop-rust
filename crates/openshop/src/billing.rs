//! Billing with Shopify's managed pricing: the plans live on Shopify's side,
//! where the merchant picks one on the plan selection page Shopify hosts for
//! the app. The application reads what the shop is subscribed to and sends
//! the merchant to that page.
//!
//! Where the plan is read is the [`BillingSource`] of the application, which
//! its configuration sets, never a guess:
//!
//! - without a [Partner API](crate::PartnerApi), the Admin API:
//!   `currentAppInstallation.activeSubscriptions`, for an app whose
//!   subscriptions the Billing API made (manual pricing), or enrolled in
//!   Shopify App Pricing before April 2026;
//! - with one, the Partner API: `activeSubscription(appId:, shopId:)`, which
//!   Shopify App Pricing asks the apps enrolled since April 2026, and every
//!   app after April 28, 2026, to read. A subscription of the Billing API
//!   that was not migrated is not there.
//!
//! What this module gives:
//!
//! - [`ShopifyClient::active_plans`] reads the plans of the shop from the
//!   source of the application, and [`ActivePlans::plan`] says whether one,
//!   any or one of some names, is active. [`ShopifyClient::active_subscriptions`]
//!   reads the Admin API whatever the source, and warns once per process when
//!   the source is the Partner API;
//! - [`require_plan`] guards a loader or an action of an admin page, and
//!   [`any_plan`] is the same as an `authorize` rule: without the plan, the
//!   page receives `402` with the code [`PLAN_REQUIRED`] and the pricing page.
//!   [`ShopifyClient::plan_among`] is the same check without the error, for
//!   a page that shows its own "no plan" state;
//! - [`pricing_url`] and [`pricing_link`] are that page, for a link of the
//!   server and for App Bridge;
//! - [`AppSubscriptionsUpdate`] is the payload of `app_subscriptions/update`,
//!   which Shopify App Pricing no longer sends after April 28, 2026.
//!
//! ```
//! use openshop::billing::{self, AppSubscriptionsUpdate};
//! use openshop::prelude::*;
//!
//! pub struct ReportsPage;
//!
//! #[admin_page("/reports", nav = "Reports")]
//! impl ReportsPage {
//!     /// For the shops on the Pro or the Plus plan; the others are sent to the pricing page.
//!     #[loader]
//!     async fn report(ctx: AdminContext) -> Result<Value> {
//!         let plan = billing::require_plan(&ctx, &["pro", "plus"]).await?;
//!         Ok(json!({ "plan": plan.name }))
//!     }
//!
//!     /// For any plan.
//!     #[action(authorize = billing::any_plan)]
//!     async fn export(ctx: AdminContext) -> Result<Value> {
//!         Ok(json!({ "shop": ctx.shop }))
//!     }
//! }
//!
//! #[webhook("app_subscriptions/update")]
//! async fn subscription_updated(ctx: WebhookContext, update: AppSubscriptionsUpdate) -> Result<()> {
//!     let subscription = update.app_subscription;
//!     ctx.logger.info(format!("{} is {}", subscription.name, subscription.status));
//!     Ok(())
//! }
//! ```
//!
//! OpenShop declares no plan: their names, prices and trials are the ones of
//! the Partner Dashboard.

mod cache;
mod partner;

use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};

use crate::admin::{AdminContext, AdminPublicError};
use crate::shopify::ShopifyClient;
use crate::types::ShopScope;

pub(crate) use cache::PlanCache;
pub use cache::MIN_REFRESH;
#[cfg(feature = "testing")]
pub(crate) use partner::{partner_ids_data, partner_subscription_data};
pub use partner::{
    AppPricingDiscount, AppPricingItem, AppPricingPrice, AppPricingSubscription, AppPricingUsage, BillingCycle,
    FlatRatePrice, PARTNER_IDS_OPERATION, PARTNER_SUBSCRIPTION_OPERATION, PendingUpdate, PriceTier, TieredPrice,
};

/// The `code` of the error [`require_plan`] answers when no plan fits.
pub const PLAN_REQUIRED: &str = "PLAN_REQUIRED";

/// The name of the GraphQL operation [`ShopifyClient::active_subscriptions`]
/// sends, and so [`require_plan`] and [`any_plan`] when the plans are read
/// from the Admin API: what a test answers
/// (`ctx.shopify.active_subscriptions([…])` of the `testing` feature does it).
pub const ACTIVE_SUBSCRIPTIONS_OPERATION: &str = "OpenShopActiveSubscriptions";

/// What [`ShopifyClient::active_subscriptions`] asks Shopify.
const ACTIVE_SUBSCRIPTIONS: &str = "query OpenShopActiveSubscriptions { currentAppInstallation { activeSubscriptions { \
     id name status test trialDays createdAt currentPeriodEnd \
     lineItems { id plan { pricingDetails { __typename \
       ... on AppRecurringPricing { interval planHandle price { amount currencyCode } \
         discount { durationLimitInIntervals remainingDurationInIntervals priceAfterDiscount { amount currencyCode } } } \
       ... on AppUsagePricing { terms interval cappedAmount { amount currencyCode } balanceUsed { amount currencyCode } } \
     } } } } } }";

/// The status of a subscription, as the Admin API and the webhooks write it
/// (`ACTIVE`, in any case).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SubscriptionStatus {
    /// Created, waiting for the merchant to approve it.
    Pending,
    /// Approved; Shopify moves it to `Active` at once.
    Accepted,
    /// The merchant pays for it, or is in its trial.
    Active,
    Declined,
    Expired,
    /// The shop is frozen: billing is paused.
    Frozen,
    Cancelled,
    /// A status this version of OpenShop does not know, as Shopify wrote it.
    Other(String),
}

impl SubscriptionStatus {
    /// As Shopify writes it: `ACTIVE`.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pending => "PENDING",
            Self::Accepted => "ACCEPTED",
            Self::Active => "ACTIVE",
            Self::Declined => "DECLINED",
            Self::Expired => "EXPIRED",
            Self::Frozen => "FROZEN",
            Self::Cancelled => "CANCELLED",
            Self::Other(status) => status,
        }
    }

    fn parse(status: &str) -> Self {
        match status.to_ascii_uppercase().as_str() {
            "PENDING" => Self::Pending,
            "ACCEPTED" => Self::Accepted,
            "ACTIVE" => Self::Active,
            "DECLINED" => Self::Declined,
            "EXPIRED" => Self::Expired,
            "FROZEN" => Self::Frozen,
            "CANCELLED" => Self::Cancelled,
            _ => Self::Other(status.to_string()),
        }
    }
}

impl std::fmt::Display for SubscriptionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for SubscriptionStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SubscriptionStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(deserializer)?))
    }
}

/// An amount of money: `amount` is a decimal written as text, `"10.0"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Money {
    #[serde(deserialize_with = "decimal")]
    pub amount: String,
    pub currency_code: String,
}

/// A discount of a recurring price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionDiscount {
    pub duration_limit_in_intervals: Option<u32>,
    pub remaining_duration_in_intervals: Option<u32>,
    pub price_after_discount: Money,
}

/// The recurring price of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecurringPricing {
    pub price: Money,
    /// `EVERY_30_DAYS` or `ANNUAL`.
    pub interval: String,
    /// The handle of the plan of managed pricing, when it has one.
    #[serde(default)]
    pub plan_handle: Option<String>,
    #[serde(default)]
    pub discount: Option<SubscriptionDiscount>,
}

/// The usage price of a plan: what is charged on top, up to a cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePricing {
    pub terms: String,
    pub interval: String,
    pub capped_amount: Money,
    pub balance_used: Money,
}

/// The pricing of a line of a subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "__typename")]
pub enum SubscriptionPricing {
    #[serde(rename = "AppRecurringPricing")]
    Recurring(RecurringPricing),
    #[serde(rename = "AppUsagePricing")]
    Usage(UsagePricing),
    /// A kind of pricing this version of OpenShop does not read.
    #[serde(other)]
    Other,
}

/// A line of a subscription: a recurring price, or a usage one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionLineItem {
    pub id: String,
    pub pricing: SubscriptionPricing,
}

/// A subscription of the shop to the app, as
/// `currentAppInstallation.activeSubscriptions` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSubscription {
    /// `gid://shopify/AppSubscription/…`.
    pub id: String,
    /// The name of the plan, as the Partner Dashboard names it.
    pub name: String,
    pub status: SubscriptionStatus,
    /// A test subscription, which charges nothing: the ones of development stores.
    pub test: bool,
    /// The free trial of the subscription, in days; `0` without one.
    pub trial_days: u32,
    pub created_at: DateTime<Utc>,
    /// The end of the billing period, or of the trial.
    #[serde(default)]
    pub current_period_end: Option<DateTime<Utc>>,
    pub line_items: Vec<SubscriptionLineItem>,
}

impl AppSubscription {
    /// Whether the shop has the plan: the subscription is `ACTIVE`, trial
    /// included.
    pub fn is_active(&self) -> bool {
        self.status == SubscriptionStatus::Active
    }

    /// The handle of its plan, from its recurring line, when it has one.
    pub fn plan_handle(&self) -> Option<&str> {
        self.line_items.iter().find_map(|line| match &line.pricing {
            SubscriptionPricing::Recurring(recurring) => recurring.plan_handle.as_deref(),
            _ => None,
        })
    }

    /// Whether it is the plan named `name`, by its name or by the handle of
    /// its plan.
    fn is_named(&self, name: &str) -> bool {
        self.name == name || self.plan_handle() == Some(name)
    }
}

/// The subscriptions of the shop that are active, as Shopify lists them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ActiveSubscriptions(pub Vec<AppSubscription>);

impl ActiveSubscriptions {
    /// The active subscription to one of `plans`, each a plan name of the
    /// Partner Dashboard or the handle of a plan; any active one when `plans`
    /// is empty. `None` when the shop has none of them.
    pub fn plan(&self, plans: &[&str]) -> Option<&AppSubscription> {
        self.0
            .iter()
            .filter(|subscription| subscription.is_active())
            .find(|subscription| plans.is_empty() || plans.iter().any(|name| subscription.is_named(name)))
    }

    /// Whether the shop has one of `plans`, or any plan when `plans` is empty.
    pub fn has_plan(&self, plans: &[&str]) -> bool {
        self.plan(plans).is_some()
    }

    pub fn into_vec(self) -> Vec<AppSubscription> {
        self.0
    }
}

impl std::ops::Deref for ActiveSubscriptions {
    type Target = [AppSubscription];

    fn deref(&self) -> &[AppSubscription] {
        &self.0
    }
}

impl IntoIterator for ActiveSubscriptions {
    type Item = AppSubscription;
    type IntoIter = std::vec::IntoIter<AppSubscription>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Where an application reads the plans of the shops:
/// [`OpenShop::billing_source`](crate::OpenShop::billing_source). The Partner
/// API when it is set ([`PartnerApi`](crate::PartnerApi)), the Admin API
/// otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum BillingSource {
    /// `currentAppInstallation.activeSubscriptions` of the Admin API: the
    /// subscriptions of the Billing API, and of Shopify App Pricing for the
    /// apps enrolled before April 2026. Asked at each check.
    AdminApi,
    /// `activeSubscription` of the Partner API: the subscriptions of Shopify
    /// App Pricing. Kept per shop for the `plan_ttl` of the Partner API.
    PartnerApi,
}

/// A plan a shop has, read from the [`BillingSource`] of the application.
/// Serialized as `{ "name", "handle", "source", "subscription" }`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plan {
    /// The name of the plan: the `name` of the subscription on the Admin
    /// API; the `description` of the item of the plan on the Partner API,
    /// else its handle.
    pub name: String,
    /// The handle of the plan: the `planHandle` of the recurring line on the
    /// Admin API, the `handle` of the item of the plan on the Partner API.
    pub handle: Option<String>,
    /// The subscription, as its source gives it.
    #[serde(flatten)]
    pub subscription: PlanSubscription,
}

/// The subscription of a [`Plan`], as its source gives it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "source", content = "subscription", rename_all = "camelCase")]
pub enum PlanSubscription {
    /// An active subscription of `currentAppInstallation.activeSubscriptions`.
    AdminApi(AppSubscription),
    /// The `activeSubscription` of the Partner API.
    PartnerApi(AppPricingSubscription),
}

impl Plan {
    /// Whether it is the plan named `name`: by its handle or by its name.
    pub fn is_named(&self, name: &str) -> bool {
        self.handle.as_deref() == Some(name) || self.name == name
    }

    /// Where it was read.
    pub fn source(&self) -> BillingSource {
        match self.subscription {
            PlanSubscription::AdminApi(_) => BillingSource::AdminApi,
            PlanSubscription::PartnerApi(_) => BillingSource::PartnerApi,
        }
    }
}

impl From<AppSubscription> for Plan {
    fn from(subscription: AppSubscription) -> Self {
        Self {
            name: subscription.name.clone(),
            handle: subscription.plan_handle().map(str::to_string),
            subscription: PlanSubscription::AdminApi(subscription),
        }
    }
}

impl From<AppPricingSubscription> for Plan {
    fn from(subscription: AppPricingSubscription) -> Self {
        let item = subscription.plan_item();
        let handle = item.and_then(|item| item.handle.clone());
        let name = item
            .and_then(|item| item.description.clone())
            .or_else(|| handle.clone())
            .unwrap_or_default();
        Self {
            name,
            handle,
            subscription: PlanSubscription::PartnerApi(subscription),
        }
    }
}

/// The plans a shop has now, read from the [`BillingSource`] of the
/// application: the active subscriptions of the Admin API, or the one of the
/// Partner API.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ActivePlans(pub Vec<Plan>);

impl ActivePlans {
    /// The plan among `plans`, each the handle of a plan or its name, that
    /// the shop has; any plan when `plans` is empty. `None` when it has none
    /// of them.
    pub fn plan(&self, plans: &[&str]) -> Option<&Plan> {
        self.0
            .iter()
            .find(|plan| plans.is_empty() || plans.iter().any(|name| plan.is_named(name)))
    }

    /// Whether the shop has one of `plans`, or any plan when `plans` is empty.
    pub fn has_plan(&self, plans: &[&str]) -> bool {
        self.plan(plans).is_some()
    }

    pub fn into_vec(self) -> Vec<Plan> {
        self.0
    }

    /// The active ones of `subscriptions`.
    fn of_admin_api(subscriptions: ActiveSubscriptions) -> Self {
        Self(
            subscriptions
                .into_iter()
                .filter(AppSubscription::is_active)
                .map(Plan::from)
                .collect(),
        )
    }

    fn of_partner_api(subscription: Option<AppPricingSubscription>) -> Self {
        Self(subscription.map(Plan::from).into_iter().collect())
    }
}

impl std::ops::Deref for ActivePlans {
    type Target = [Plan];

    fn deref(&self) -> &[Plan] {
        &self.0
    }
}

impl IntoIterator for ActivePlans {
    type Item = Plan;
    type IntoIter = std::vec::IntoIter<Plan>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// What Shopify answers for the line of a subscription, before it is read.
#[derive(Deserialize)]
struct LineItemData {
    id: String,
    plan: PlanData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanData {
    pricing_details: SubscriptionPricing,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubscriptionData {
    id: String,
    name: String,
    status: SubscriptionStatus,
    test: bool,
    trial_days: u32,
    created_at: DateTime<Utc>,
    #[serde(default)]
    current_period_end: Option<DateTime<Utc>>,
    line_items: Vec<LineItemData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallationData {
    active_subscriptions: Vec<SubscriptionData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActiveSubscriptionsData {
    current_app_installation: InstallationData,
}

/// The subscriptions of the `data` of [`ACTIVE_SUBSCRIPTIONS`].
fn read_active_subscriptions(data: Value) -> anyhow::Result<ActiveSubscriptions> {
    let data: ActiveSubscriptionsData = serde_json::from_value(data)
        .map_err(|error| anyhow::anyhow!("[openshop] Unexpected answer to activeSubscriptions: {error}"))?;
    Ok(ActiveSubscriptions(
        data.current_app_installation
            .active_subscriptions
            .into_iter()
            .map(|subscription| AppSubscription {
                id: subscription.id,
                name: subscription.name,
                status: subscription.status,
                test: subscription.test,
                trial_days: subscription.trial_days,
                created_at: subscription.created_at,
                current_period_end: subscription.current_period_end,
                line_items: subscription
                    .line_items
                    .into_iter()
                    .map(|line| SubscriptionLineItem {
                        id: line.id,
                        pricing: line.plan.pricing_details,
                    })
                    .collect(),
            })
            .collect(),
    ))
}

/// The `data` Shopify answers [`ACTIVE_SUBSCRIPTIONS`] with when the shop
/// has `subscriptions`: what [`read_active_subscriptions`] reads back.
#[cfg(feature = "testing")]
pub(crate) fn active_subscriptions_data(subscriptions: &[AppSubscription]) -> Value {
    let subscriptions: Vec<Value> = subscriptions
        .iter()
        .map(|subscription| {
            serde_json::json!({
                "id": subscription.id,
                "name": subscription.name,
                "status": subscription.status,
                "test": subscription.test,
                "trialDays": subscription.trial_days,
                "createdAt": subscription.created_at,
                "currentPeriodEnd": subscription.current_period_end,
                "lineItems": subscription
                    .line_items
                    .iter()
                    .map(|line| serde_json::json!({ "id": line.id, "plan": { "pricingDetails": line.pricing } }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({ "currentAppInstallation": { "activeSubscriptions": subscriptions } })
}

impl ShopifyClient {
    /// The subscriptions of the shop to the app that are active:
    /// `currentAppInstallation.activeSubscriptions` of the Admin API, with
    /// their status, their trial and their lines, whatever the
    /// [`BillingSource`] of the application. One request, which needs no
    /// access scope; nothing is kept between two calls.
    ///
    /// For an app on manual pricing, whose charges the Billing API creates,
    /// this is the plan of the shop. For an app on Shopify App Pricing, it is
    /// not: since April 2026 its subscriptions may have no Admin API
    /// subscription, and a paying merchant looks like one without a plan.
    /// Call [`active_plans`](Self::active_plans) instead, which reads the
    /// source of the application.
    ///
    /// When the source is the Partner API, the first call of the process
    /// logs a warning that names where it was called from. The call still
    /// reads the Admin API, as before: an app enrolled before April 2026 can
    /// read its legacy subscriptions on purpose.
    ///
    /// ```no_run
    /// # async fn example(ctx: openshop::FlowContext) -> openshop::Result<()> {
    /// let subscriptions = ctx.shopify.active_subscriptions().await?;
    /// if subscriptions.has_plan(&["Pro"]) {
    ///     // …
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[track_caller]
    pub fn active_subscriptions(&self) -> impl Future<Output = anyhow::Result<ActiveSubscriptions>> + Send + '_ {
        if self.app().partner_api().is_some() && self.app().plans().first_admin_api_read() {
            let caller = std::panic::Location::caller();
            crate::runtime::logger::warn_with(
                admin_api_warning(&format!("{}:{}", caller.file(), caller.line())),
                json!({ "shop": self.shop, "shopifyApp": self.shopify_app }),
            );
        }
        self.admin_api_subscriptions()
    }

    /// `currentAppInstallation.activeSubscriptions`, without the warning of
    /// [`active_subscriptions`](Self::active_subscriptions).
    async fn admin_api_subscriptions(&self) -> anyhow::Result<ActiveSubscriptions> {
        read_active_subscriptions(self.graphql(ACTIVE_SUBSCRIPTIONS, ()).await?)
    }

    /// The plans the shop has now, read from the [`BillingSource`] of the
    /// application:
    ///
    /// - the Admin API: its active subscriptions, one request at each call;
    /// - the Partner API: its `activeSubscription`, kept for the `plan_ttl`
    ///   of the [Partner API](crate::PartnerApi). The first call for a shop
    ///   also reads the GIDs of the app and of the shop from the Admin API,
    ///   once per process.
    ///
    /// ```no_run
    /// # async fn example(ctx: openshop::FlowContext) -> openshop::Result<()> {
    /// if let Some(plan) = ctx.shopify.active_plans().await?.plan(&["pro"]) {
    ///     ctx.logger.info(format!("On {}", plan.name));
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn active_plans(&self) -> anyhow::Result<ActivePlans> {
        self.plans_passing(|_| true).await
    }

    /// The plan among `plans` (handles of plans, or their names) the shop
    /// has, or any plan when `plans` is empty; `None` when it has none of
    /// them. It is what [`require_plan`] checks, for a page that shows its
    /// own "no plan" state instead of answering `402`, or for a flow, a
    /// webhook or a route.
    ///
    /// It reads the [`BillingSource`] of the application with the rule of
    /// [`require_plan`]. On the Partner API, a kept answer that has one of
    /// `plans` is used until it is `plan_ttl` old. A kept answer without one
    /// is asked again, unless it is younger than [`MIN_REFRESH`]: a merchant
    /// who just chose the plan gets it, and a merchant without one costs at
    /// most a request every ten seconds. [`active_plans`](Self::active_plans)
    /// keeps any answer for `plan_ttl`.
    ///
    /// A failure to ask Shopify is an error, not `None`.
    ///
    /// ```
    /// use openshop::billing;
    /// use openshop::prelude::*;
    ///
    /// pub struct ReportsPage;
    ///
    /// #[admin_page("/reports", nav = "Reports")]
    /// impl ReportsPage {
    ///     /// The report, or what the page shows a shop without the plan.
    ///     #[loader]
    ///     async fn report(ctx: AdminContext) -> Result<Value> {
    ///         Ok(match ctx.shopify.plan_among(&["pro", "plus"]).await? {
    ///             Some(plan) => json!({ "plan": plan.handle }),
    ///             None => json!({ "plan": null, "pricingLink": billing::pricing_link(&ctx)? }),
    ///         })
    ///     }
    /// }
    /// ```
    pub async fn plan_among(&self, plans: &[&str]) -> anyhow::Result<Option<Plan>> {
        let active = self.plans_passing(|active| active.has_plan(plans)).await?;
        Ok(active.plan(plans).cloned())
    }

    /// [`active_plans`](Self::active_plans), asking the Partner API again
    /// whatever the age of its last answer: for the welcome link of a plan
    /// the server serves itself, which can come seconds after the check that
    /// sent the merchant to the plans. Call it there, not at each request:
    /// the Partner API takes four requests a second for the whole
    /// organization. The Admin API is asked at each call anyway.
    ///
    /// On the welcome link of a plan that a route serves, read the plan only
    /// for a launch Shopify signed ([`RouteContext::shopify_launch`](crate::RouteContext::shopify_launch)):
    /// anyone can call a route, and an open route must not spend the
    /// organization's rate limit. Read it as a best effort: when the Partner
    /// API fails, log it and send the merchant into the app anyway. With `?`,
    /// a merchant who just paid would get a `500`. A check that does not find
    /// the plan asks again once the kept answer is [`MIN_REFRESH`] old.
    ///
    /// ```
    /// use openshop::prelude::*;
    /// use openshop::response::{self, IntoResponse, Response, StatusCode, header};
    ///
    /// /// The welcome link of a plan, which the admin opens in the app's frame.
    /// #[route(GET, "/welcome")]
    /// async fn welcome(ctx: RouteContext) -> Result<Response> {
    ///     let Some(launch) = ctx.shopify_launch() else {
    ///         // Not signed by Shopify: the app's page in the admin, and no Partner API call.
    ///         let Some(shop) = ctx.request.query_shop() else {
    ///             let error = json!({ "error": "Missing or invalid shop" });
    ///             return Ok(response::json_with_status(StatusCode::BAD_REQUEST, error));
    ///         };
    ///         let location = ctx.admin_url(&shop, &format!("apps/{}", ctx.app_handle(&shop)?))?;
    ///         return Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response());
    ///     };
    ///     // Only a launch Shopify signed reads the plan again: an open route must
    ///     // not spend the organization's rate limit of the Partner API.
    ///     if launch.is_plan_welcome() {
    ///         let refreshed = async { ctx.for_shop(&launch).await?.shopify.refresh_plans().await };
    ///         if let Err(error) = refreshed.await {
    ///             ctx.logger.warn(format!("The plan of {} was not read again: {error:#}", launch.shop));
    ///         }
    ///     }
    ///     // Still in the admin's frame: the app's home, with the signed query.
    ///     let location = format!("/?{}", launch.query);
    ///     Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
    /// }
    /// ```
    pub async fn refresh_plans(&self) -> anyhow::Result<ActivePlans> {
        if self.app().partner_api().is_some() {
            self.app().plans().forget(&self.shopify_app, &self.domain()?).await;
        }
        self.active_plans().await
    }

    /// The plans of the shop, from the source of the application. A kept
    /// answer of the Partner API that `passes` is used until it is
    /// `plan_ttl` old; one that does not, until it is [`MIN_REFRESH`] old.
    pub(crate) async fn plans_passing(&self, passes: impl Fn(&ActivePlans) -> bool) -> anyhow::Result<ActivePlans> {
        let Some(partner) = self.app().partner_api() else {
            return Ok(ActivePlans::of_admin_api(self.admin_api_subscriptions().await?));
        };
        let shop = self.domain()?;
        let entry = self.app().plans().entry(&self.shopify_app, &shop);
        let mut entry = entry.lock().await;
        let asked_at = Instant::now();
        if let Some(answer) = &entry.answer {
            let plans = ActivePlans::of_partner_api(answer.subscription.clone());
            if answer.serves(asked_at, partner.plan_ttl(), passes(&plans)) {
                return Ok(plans);
            }
        }
        let ids = match &entry.ids {
            Some(ids) => ids.clone(),
            None => {
                let ids = partner::read_partner_ids(self.graphql(partner::PARTNER_IDS, ()).await?)?;
                entry.ids = Some(ids.clone());
                ids
            }
        };
        let data = partner
            .graphql(
                partner::PARTNER_SUBSCRIPTION,
                json!({ "appId": ids.app, "shopId": ids.shop }),
            )
            .await?;
        let subscription = partner::read_partner_subscription(data)?;
        entry.answer = (!partner.plan_ttl().is_zero()).then(|| cache::Answer {
            asked_at,
            subscription: subscription.clone(),
        });
        Ok(ActivePlans::of_partner_api(subscription))
    }
}

/// What [`ShopifyClient::active_subscriptions`] logs once per process when
/// the plans are read from the Partner API. `at` is where it was called.
fn admin_api_warning(at: &str) -> String {
    format!(
        "[openshop] active_subscriptions() at {at} reads the Admin API, but billing reads the plans from the \
         Partner API. A subscription of Shopify App Pricing may not be in the Admin API, so a paying merchant can \
         look like one without a plan. Call active_plans() instead, or keep active_subscriptions() only to read \
         the Admin API on purpose. This warning is logged once per process."
    )
}

/// The plan selection page Shopify hosts for the app, for the shop of
/// `scope`: on Shopify,
/// `https://admin.shopify.com/store/{store}/charges/{app_handle}/pricing_plans`;
/// with an `admin_origin`,
/// `{admin_origin}/admin/charges/{app_handle}/pricing_plans?shop={shop}`. For
/// a link the server writes, or a redirect of the top window.
///
/// Fails when the `shopify.app.toml` of the app has no `handle`: Shopify
/// does not take the client ID in this URL, and the error says what to add.
pub fn pricing_url(scope: &impl ShopScope) -> anyhow::Result<String> {
    Ok(scope
        .app()
        .shopify_endpoints()
        .pricing_plans_url(scope.shop(), scope.app_handle()?))
}

/// The same page as an App Bridge link of the admin,
/// `shopify:admin/charges/{app_handle}/pricing_plans`: what a view of an
/// embedded page opens with `target: "_top"`. The admin it is opened in names
/// the shop. Fails as [`pricing_url`] does without a `handle`.
pub fn pricing_link(scope: &impl ShopScope) -> anyhow::Result<String> {
    Ok(format!("shopify:admin/charges/{}/pricing_plans", scope.app_handle()?))
}

/// The error [`require_plan`] answers: `402`, the code [`PLAN_REQUIRED`],
/// and the pricing page in its details, `pricingUrl`
/// ([`pricing_url`]) and `pricingLink` ([`pricing_link`]). Fails as they do
/// without a `handle`.
pub fn plan_required(scope: &impl ShopScope, plans: &[&str]) -> anyhow::Result<AdminPublicError> {
    Ok(AdminPublicError::new(PLAN_REQUIRED, plan_required_message(plans))
        .status(402)
        .detail("pricingUrl", pricing_url(scope)?)
        .detail("pricingLink", pricing_link(scope)?))
}

/// What the merchant is told when no plan fits.
fn plan_required_message(plans: &[&str]) -> String {
    match plans {
        [] => "Choose a plan to use this feature".to_string(),
        [plan] => format!("This feature needs the {plan} plan"),
        [first @ .., last] => format!("This feature needs the {} or {last} plan", first.join(", ")),
    }
}

/// Guards a loader or an action of an admin page: the plan among `plans`
/// (handles of plans, or their names) the shop has, or any plan when `plans`
/// is empty, read from the [`BillingSource`] of the application. Without
/// one, the error the page receives is `402 PLAN_REQUIRED` with the pricing
/// page in its `details` ([`plan_required`]), for the view to send the
/// merchant there.
///
/// The plan is read by [`ShopifyClient::plan_among`]: on the Partner API, a
/// kept answer without the plan is asked again first, unless it is younger
/// than [`MIN_REFRESH`]. A merchant who just chose the plan is not sent back
/// to the plans. A page that shows its own "no plan" state calls
/// `plan_among` itself, and gets `None` instead of this error.
///
/// A failure to ask Shopify is an error of the server (`500`), not a missing
/// plan: a paying merchant is not sent to the pricing page because Shopify
/// did not answer. So is a `shopify.app.toml` without a `handle`, which the
/// pricing page needs.
pub async fn require_plan(ctx: &AdminContext, plans: &[&str]) -> anyhow::Result<Plan> {
    match ctx.shopify.plan_among(plans).await? {
        Some(plan) => Ok(plan),
        None => Err(plan_required(ctx, plans)?.into()),
    }
}

/// [`require_plan`] for any plan, as an `authorize` rule of a loader or an
/// action: `#[action(authorize = billing::any_plan)]`. Not for `#[access]`,
/// whose failure hides the page instead of answering `402`.
pub async fn any_plan(ctx: AdminContext) -> anyhow::Result<bool> {
    require_plan(&ctx, &[]).await.map(|_| true)
}

/// The payload of the `app_subscriptions/update` webhook (API 2026-10), which
/// Shopify sends when a subscription of the shop changes: chosen, approved,
/// cancelled, frozen…
///
/// ```
/// use openshop::billing::{AppSubscriptionsUpdate, SubscriptionStatus};
///
/// let update: AppSubscriptionsUpdate = openshop::serde_json::from_str(r#"{
///   "app_subscription": {
///     "admin_graphql_api_id": "gid://shopify/AppSubscription/1029266965",
///     "name": "Pro", "status": "ACTIVE",
///     "admin_graphql_api_shop_id": "gid://shopify/Shop/548380009",
///     "created_at": "2021-12-31T19:00:00-05:00", "updated_at": "2021-12-31T19:00:00-05:00",
///     "currency": "USD", "capped_amount": "20.0", "price": "10.00",
///     "interval": "every_30_days", "plan_handle": "pro"
///   }
/// }"#).unwrap();
/// assert_eq!(update.app_subscription.status, SubscriptionStatus::Active);
/// assert_eq!(update.app_subscription.plan_handle.as_deref(), Some("pro"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSubscriptionsUpdate {
    pub app_subscription: UpdatedAppSubscription,
}

/// The subscription of an [`AppSubscriptionsUpdate`]. Shopify names its
/// fields in snake case; the ones a subscription may not have are optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatedAppSubscription {
    /// `gid://shopify/AppSubscription/…`, the `id` of [`AppSubscription`].
    pub admin_graphql_api_id: String,
    pub name: String,
    pub status: SubscriptionStatus,
    /// `gid://shopify/Shop/…`.
    #[serde(default)]
    pub admin_graphql_api_shop_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    /// The currency of its price, `USD`.
    #[serde(default)]
    pub currency: Option<String>,
    /// The recurring price, a decimal written as text: `"10.00"`.
    #[serde(default, deserialize_with = "optional_decimal")]
    pub price: Option<String>,
    /// The cap of its usage price.
    #[serde(default, deserialize_with = "optional_decimal")]
    pub capped_amount: Option<String>,
    /// `every_30_days` or `annual`.
    #[serde(default)]
    pub interval: Option<String>,
    /// The handle of the plan of managed pricing.
    #[serde(default)]
    pub plan_handle: Option<String>,
}

/// A decimal, written as text or as a number: as text.
fn decimal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::String(text) => Ok(text),
        Value::Number(number) => Ok(number.to_string()),
        other => Err(de::Error::custom(format!("expected a decimal, got {other}"))),
    }
}

fn optional_decimal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text)),
        Value::Number(number) => Ok(Some(number.to_string())),
        other => Err(de::Error::custom(format!("expected a decimal, got {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn subscription(name: &str, status: &str, plan_handle: Option<&str>) -> Value {
        json!({
            "id": format!("gid://shopify/AppSubscription/{name}"),
            "name": name,
            "status": status,
            "test": true,
            "trialDays": 7,
            "createdAt": "2026-11-01T10:00:00Z",
            "currentPeriodEnd": null,
            "lineItems": [
                {
                    "id": "gid://shopify/AppSubscriptionLineItem/1",
                    "plan": { "pricingDetails": {
                        "__typename": "AppRecurringPricing",
                        "interval": "EVERY_30_DAYS",
                        "planHandle": plan_handle,
                        "price": { "amount": "9.99", "currencyCode": "USD" },
                        "discount": null
                    } }
                },
                {
                    "id": "gid://shopify/AppSubscriptionLineItem/2",
                    "plan": { "pricingDetails": {
                        "__typename": "AppUsagePricing",
                        "terms": "$1 per 100 orders",
                        "interval": "EVERY_30_DAYS",
                        "cappedAmount": { "amount": 50, "currencyCode": "USD" },
                        "balanceUsed": { "amount": "0.0", "currencyCode": "USD" }
                    } }
                },
                {
                    "id": "gid://shopify/AppSubscriptionLineItem/3",
                    "plan": { "pricingDetails": { "__typename": "AppFuturePricing" } }
                }
            ]
        })
    }

    fn answer(subscriptions: Vec<Value>) -> Value {
        json!({ "currentAppInstallation": { "activeSubscriptions": subscriptions } })
    }

    #[test]
    fn reads_the_active_subscriptions_shopify_answers() {
        let read = read_active_subscriptions(answer(vec![subscription("Pro", "ACTIVE", Some("pro"))])).unwrap();
        let pro = &read[0];
        assert_eq!(pro.name, "Pro");
        assert!(pro.is_active() && pro.test);
        assert_eq!(pro.trial_days, 7);
        assert_eq!(pro.created_at.to_rfc3339(), "2026-11-01T10:00:00+00:00");
        assert_eq!(pro.plan_handle(), Some("pro"));
        let SubscriptionPricing::Usage(usage) = &pro.line_items[1].pricing else {
            panic!("a usage line");
        };
        // A number or a text: the decimal as text.
        assert_eq!(usage.capped_amount.amount, "50");
        assert_eq!(pro.line_items[2].pricing, SubscriptionPricing::Other);

        assert!(read_active_subscriptions(json!({ "currentAppInstallation": null })).is_err());
    }

    #[test]
    fn a_plan_is_active_by_its_name_or_its_handle() {
        let subscriptions = read_active_subscriptions(answer(vec![
            subscription("Basic", "PENDING", None),
            subscription("Pro", "active", Some("pro-monthly")),
        ]))
        .unwrap();
        assert_eq!(subscriptions.plan(&[]).map(|plan| plan.name.as_str()), Some("Pro"));
        assert!(subscriptions.has_plan(&["Pro"]));
        assert!(subscriptions.has_plan(&["Plus", "pro-monthly"]));
        // A pending subscription is not a plan the shop has.
        assert!(!subscriptions.has_plan(&["Basic"]));
        assert!(!subscriptions.has_plan(&["pro"]));
        assert!(!ActiveSubscriptions::default().has_plan(&[]));
    }

    #[test]
    fn statuses_are_read_in_any_case_and_written_as_shopify_does() {
        let status = |text: &str| serde_json::from_value::<SubscriptionStatus>(json!(text)).unwrap();
        assert_eq!(status("ACTIVE"), SubscriptionStatus::Active);
        assert_eq!(status("cancelled"), SubscriptionStatus::Cancelled);
        assert_eq!(status("PAUSED"), SubscriptionStatus::Other("PAUSED".to_string()));
        assert_eq!(json!(SubscriptionStatus::Frozen), json!("FROZEN"));
        assert_eq!(SubscriptionStatus::Other("PAUSED".into()).to_string(), "PAUSED");
    }

    #[test]
    fn reads_the_payload_of_app_subscriptions_update() {
        // The sample of shopify.dev, API 2026-10.
        let update: AppSubscriptionsUpdate = serde_json::from_value(json!({
            "app_subscription": {
                "admin_graphql_api_id": "gid://shopify/AppSubscription/1029266965",
                "name": "Webhook Test",
                "status": "PENDING",
                "admin_graphql_api_shop_id": "gid://shopify/Shop/548380009",
                "created_at": "2021-12-31T19:00:00-05:00",
                "updated_at": "2021-12-31T19:00:00-05:00",
                "currency": "USD",
                "capped_amount": "20.0",
                "price": "10.00",
                "interval": "every_30_days",
                "plan_handle": "plan-123"
            }
        }))
        .unwrap();
        let subscription = update.app_subscription;
        assert_eq!(subscription.status, SubscriptionStatus::Pending);
        assert_eq!(
            subscription.created_at.unwrap().to_rfc3339(),
            "2022-01-01T00:00:00+00:00"
        );
        assert_eq!(subscription.price.as_deref(), Some("10.00"));
        assert_eq!(subscription.capped_amount.as_deref(), Some("20.0"));
        assert_eq!(subscription.plan_handle.as_deref(), Some("plan-123"));

        // A subscription without a usage price, a plan handle or dates.
        let update: AppSubscriptionsUpdate = serde_json::from_value(json!({
            "app_subscription": {
                "admin_graphql_api_id": "gid://shopify/AppSubscription/1",
                "name": "Basic",
                "status": "CANCELLED",
                "capped_amount": null,
                "price": 5,
                "plan_handle": null
            }
        }))
        .unwrap();
        assert_eq!(update.app_subscription.status, SubscriptionStatus::Cancelled);
        assert_eq!(update.app_subscription.price.as_deref(), Some("5"));
        assert_eq!(update.app_subscription.capped_amount, None);
    }

    #[test]
    fn the_operation_is_named_as_the_tests_answer_it() {
        assert_eq!(
            crate::testing::GraphqlCall::of(&crate::testing::OutboundRequest {
                method: "POST",
                url: "https://test.myshopify.com/admin/api/2026-10/graphql.json".to_string(),
                headers: Vec::new(),
                body: json!({ "query": ACTIVE_SUBSCRIPTIONS }),
            })
            .and_then(|call| call.operation)
            .as_deref(),
            Some(ACTIVE_SUBSCRIPTIONS_OPERATION)
        );
    }

    #[test]
    fn the_fixture_of_the_tests_reads_back_as_it_was_written() {
        let written = read_active_subscriptions(answer(vec![
            subscription("Pro", "ACTIVE", Some("pro")),
            subscription("Basic", "FROZEN", None),
        ]))
        .unwrap();
        // `AppFuturePricing` reads as `Other`, which is written as such.
        let read = read_active_subscriptions(active_subscriptions_data(&written)).unwrap();
        assert_eq!(read, written);
        assert_eq!(read_active_subscriptions(active_subscriptions_data(&[])).unwrap(), ActiveSubscriptions::default());
    }

    #[test]
    fn the_error_of_a_missing_plan_names_the_plans() {
        assert_eq!(plan_required_message(&[]), "Choose a plan to use this feature");
        assert_eq!(plan_required_message(&["Pro"]), "This feature needs the Pro plan");
        assert_eq!(plan_required_message(&["Pro", "Plus"]), "This feature needs the Pro or Plus plan");
        assert_eq!(
            plan_required_message(&["Basic", "Pro", "Plus"]),
            "This feature needs the Basic, Pro or Plus plan"
        );
    }
}
