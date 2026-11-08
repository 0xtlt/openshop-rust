# Billing

`openshop::billing` works with Shopify's managed pricing (Shopify App
Pricing): the plans are declared in the Partner Dashboard, and Shopify hosts
the page where a merchant picks one. OpenShop declares no plan of its own. An
application reads what the shop is subscribed to, sends the merchant to the
plan selection page, and hears of the changes. To set it up step by step, see
[Charge with managed pricing](../guides/charge-with-managed-pricing.md).

## Active subscriptions

`ShopifyClient::active_subscriptions()` reads
`currentAppInstallation.activeSubscriptions` of the shop through the client of
any context (`ctx.shopify`): one request to the Admin API, which needs no
access scope. Nothing is kept between two calls.

```rust,ignore
let subscriptions = ctx.shopify.active_subscriptions().await?;
if let Some(subscription) = subscriptions.plan(&["Pro", "Plus"]) {
    ctx.logger.info(format!("On {} ({})", subscription.name, subscription.status));
}
```

| `ActiveSubscriptions` | Returns |
| --- | --- |
| `plan(&["Pro", "Plus"])` | `Option<&AppSubscription>`: the active subscription to one of the plans. Each is a plan name of the Partner Dashboard or the handle of a plan. With `&[]`, any active subscription. |
| `has_plan(&[…])` | `bool`: whether `plan(&[…])` finds one. |
| `[index]`, `iter()`, `len()`… | The subscriptions as Shopify listed them: `ActiveSubscriptions` dereferences to `[AppSubscription]`. |
| `into_vec()` | `Vec<AppSubscription>`. |

A subscription counts when its status is `ACTIVE`, a trial included. A
`PENDING` one, which the merchant has not approved, does not.

| `AppSubscription` field | Type | Shopify field |
| --- | --- | --- |
| `id` | `String` | `id`, `gid://shopify/AppSubscription/…` |
| `name` | `String` | `name`, the name of the plan |
| `status` | `SubscriptionStatus` | `status`: `Pending`, `Accepted`, `Active`, `Declined`, `Expired`, `Frozen`, `Cancelled`, or `Other(String)` for one OpenShop does not know |
| `test` | `bool` | `test`: a subscription that charges nothing, such as those of development stores |
| `trial_days` | `u32` | `trialDays` |
| `created_at` | `DateTime<Utc>` | `createdAt` |
| `current_period_end` | `Option<DateTime<Utc>>` | `currentPeriodEnd` |
| `line_items` | `Vec<SubscriptionLineItem>` | `lineItems`: `id`, and `pricing`, `SubscriptionPricing::Recurring` (`price`, `interval`, `plan_handle`, `discount`), `::Usage` (`terms`, `interval`, `capped_amount`, `balance_used`), or `::Other` |

`subscription.is_active()` and `subscription.plan_handle()`, the handle of the
plan of its recurring line, help to read it. An amount is a `Money`, whose
`amount` is the decimal written as text (`"9.99"`) and `currency_code` its
currency.

## Guard an admin page

`billing::require_plan(&ctx, &plans)` guards a loader or an action: it returns
the active subscription to one of `plans`, or to any plan when `plans` is
empty. Without one, it fails with an `AdminPublicError` that the page receives:

```rust,ignore
use openshop::billing;
use openshop::prelude::*;

pub struct ReportsPage;

#[admin_page("/reports", nav = "Reports")]
impl ReportsPage {
    #[loader]
    async fn report(ctx: AdminContext) -> Result<Value> {
        let subscription = billing::require_plan(&ctx, &["Pro", "Plus"]).await?;
        Ok(json!({ "plan": subscription.name }))
    }

    /// Any plan.
    #[action(authorize = billing::any_plan)]
    async fn export(ctx: AdminContext) -> Result<Value> {
        Ok(json!({ "shop": ctx.shop }))
    }
}
```

`billing::any_plan` is `require_plan(&ctx, &[])` as an `authorize` rule of a
loader or an action. Do not make it an `#[access]` rule: a page whose access
rule fails is hidden, and the merchant is not told why.

The response of a call without the plan:

```json
{
  "error": "This feature needs the Pro or Plus plan",
  "code": "PLAN_REQUIRED",
  "requestId": "…",
  "details": {
    "pricingUrl": "https://admin.shopify.com/store/demo/charges/my-app/pricing_plans",
    "pricingLink": "shopify:admin/charges/my-app/pricing_plans"
  }
}
```

with status `402`. Without plans, the message is
`Choose a plan to use this feature`. `billing::plan_required(&ctx, &plans)`
builds the same error, for a check of your own, and `billing::PLAN_REQUIRED` is
its code. A request to Shopify that fails is not a missing plan: it is an
error of the server (`500`), so a paying merchant is not sent to the pricing
page because Shopify did not answer.

In a view, the error of `use_loader` or `use_action` carries the details:
`error.code.as_deref() == Some("PLAN_REQUIRED")`, and
`error.detail("pricingLink")` (the view names the code as text:
`openshop::billing` is not compiled into the bundle). Open the link in the top
window of the admin, as an App Bridge link: `window.open(link, "_top")`, or an
`<a target="_top">` to it.

## The pricing page and the pages of the admin

| Function | Returns |
| --- | --- |
| `billing::pricing_url(&ctx)` | The plan selection page of the app, for the shop of the context: `https://admin.shopify.com/store/{store}/charges/{app_handle}/pricing_plans`. With an `admin_origin` ([Shopify endpoints](../guides/custom-shopify-endpoints.md)), `{admin_origin}/admin/charges/{app_handle}/pricing_plans?shop={shop}`. |
| `billing::pricing_link(&ctx)` | `shopify:admin/charges/{app_handle}/pricing_plans`, the App Bridge link of the same page, for a view of an embedded page. |
| `ctx.admin_url("settings/payments")` | Any page of the admin of the shop: `https://admin.shopify.com/store/{store}/settings/payments`, or `{admin_origin}/admin/settings/payments?shop={shop}`. The path may carry a query string. |
| `ctx.pricing_url(shop)`, `ctx.admin_url(shop, path)`, `ctx.app_handle(shop)` | The same in a server route, for the shop it names, installed or not: see [Server routes](server-routes.md#links-to-the-shopify-admin). |
| `ShopifyEndpoints::admin_url(shop, path)`, `::pricing_plans_url(shop, app_handle)` | The same without a context: `app.shopify_endpoints().admin_url(…)`. |

`store` is the shop without `.myshopify.com`. `ctx` is any context that
belongs to a shop: `pricing_url`, `pricing_link`, `app_handle` and
`admin_url` come from the `ShopScope` trait, in the prelude. A server route
does not belong to a shop: `RouteContext` has them as methods that take the
shop.

### The handle of the app

The URLs of the admin name the app by its handle on Shopify,
`ctx.app_handle()`: the `handle` of its `shopify.app.toml`, which Shopify CLI
writes when it links the app. It is not `ctx.shopify_app`, the name OpenShop
gives the app in its configuration (`default` for the app of the environment),
which keeps its meaning.

| Configuration | `app_handle()` |
| --- | --- |
| The app of the environment (`SHOPIFY_API_KEY`) | The top-level `handle` of the `shopify.app*.toml` whose `client_id` is `SHOPIFY_API_KEY`; without `SHOPIFY_API_KEY`, of `shopify.app.toml`. |
| `ShopifyAppConfig::Toml` | The `handle` of its file. |
| `ShopifyAppConfig::Keys` | The `handle` of the `shopify.app*.toml` whose `client_id` is its `api_key`. |

Without a `handle` in the file, it is the API key of the app, then its
OpenShop name. Keep the `shopify.app*.toml` files next to the binary in
production, as the generated `Dockerfile` does.

## The `app_subscriptions/update` webhook

Shopify sends `app_subscriptions/update` when a subscription of the shop
changes: a plan chosen, approved, cancelled, frozen… `AppSubscriptionsUpdate`
is its payload (API 2026-10):

```rust,ignore
use openshop::billing::{AppSubscriptionsUpdate, SubscriptionStatus};
use openshop::prelude::*;

#[webhook("app_subscriptions/update")]
pub async fn subscription_updated(ctx: WebhookContext, update: AppSubscriptionsUpdate) -> Result<()> {
    let subscription = update.app_subscription;
    if subscription.status == SubscriptionStatus::Cancelled {
        ctx.logger.warn(format!("{} cancelled {}", ctx.shop, subscription.name));
    }
    Ok(())
}
```

| `UpdatedAppSubscription` field | Type | Payload field |
| --- | --- | --- |
| `admin_graphql_api_id` | `String` | The `id` of the subscription in the Admin API |
| `name` | `String` | The name of the plan |
| `status` | `SubscriptionStatus` | Read in any case: `ACTIVE`, `active` |
| `admin_graphql_api_shop_id` | `Option<String>` | `gid://shopify/Shop/…` |
| `created_at`, `updated_at` | `Option<DateTime<Utc>>` | |
| `currency` | `Option<String>` | `USD` |
| `price`, `capped_amount` | `Option<String>` | Decimals as text, `"10.00"`; a number is read as text too |
| `interval` | `Option<String>` | `every_30_days` or `annual` |
| `plan_handle` | `Option<String>` | The handle of the plan |

Subscribe to it in `shopify.app.toml`, with the `uri` of the other topics
(`/webhooks`). The payload says what changed; read
`active_subscriptions()` for what the shop has now.
