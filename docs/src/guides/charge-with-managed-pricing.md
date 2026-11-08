# Charge with managed pricing

With Shopify's managed pricing (Shopify App Pricing), the plans of an app are
declared in the Partner Dashboard, and Shopify hosts the page where a merchant
picks one and approves its charge. This guide makes an admin page that needs a
plan: a shop without one is sent to that page, and the app hears when a
subscription changes. The [billing reference](../reference/billing.md) has
every type and function.

## 1. Declare the plans in Shopify

In the Partner Dashboard, open the listing of the app, then **Pricing
content**, **Settings**, and choose **Shopify App Pricing** as the pricing
method. Add the plans, such as `Pro` and `Plus`. OpenShop declares none: the
names you use in the code are the names, or the handles, of these plans.

Give the app its handle in `shopify.app.toml`. Shopify CLI writes it when it
links the app (`shopify app config link`):

```toml
client_id = "…"
name = "Hide PayPal"
handle = "hide-paypal"
```

The handle names the app in the URL of its plan selection page,
`https://admin.shopify.com/store/{store}/charges/hide-paypal/pricing_plans`.
`ctx.app_handle()` reads it; without it, the page is named by the API key of
the app, which Shopify may not take.

## 2. Guard the page

Call `billing::require_plan` in a loader or an action, with the plans it needs,
or use `billing::any_plan` as the `authorize` rule of one that needs any plan:

```rust,ignore
// src/pages/reports.rs
use openshop::billing;
use openshop::prelude::*;

pub struct ReportsPage;

#[admin_page("/reports", nav = "Reports")]
impl ReportsPage {
    #[loader]
    async fn report(ctx: AdminContext) -> Result<Value> {
        let subscription = billing::require_plan(&ctx, &["Pro", "Plus"]).await?;
        ctx.logger.info(format!("Report for a {} shop", subscription.name));
        Ok(json!({ "plan": subscription.name }))
    }

    #[action(authorize = billing::any_plan)]
    async fn export(ctx: AdminContext) -> Result<Value> {
        Ok(json!({ "shop": ctx.shop }))
    }
}
```

A shop with neither plan gets `402` with the code `PLAN_REQUIRED`, and the
plan selection page in the `details` of the error: `pricingUrl`, the page
itself, and `pricingLink`, its App Bridge link
(`shopify:admin/charges/hide-paypal/pricing_plans`).

## 3. Send the merchant to the plans

In the view of the page, open `pricingLink` in the top window of the admin when
the loader answers `PLAN_REQUIRED`:

```rust,ignore
#[view]
fn view() -> impl IntoView {
    let report = use_loader(Self::REPORT, || ());
    let plans = move || {
        report
            .error
            .get()
            // `openshop::billing` is the server's: the view names the code.
            .filter(|error| error.code.as_deref() == Some("PLAN_REQUIRED"))
            .and_then(|error| error.detail("pricingLink").map(str::to_string))
    };
    view! {
        <s-page heading="Reports">
            {move || plans().map(|link| view! {
                <s-banner heading="Choose a plan" tone="warning">
                    <a href=link target="_top">"See the plans"</a>
                </s-banner>
            })}
        </s-page>
    }
}
```

On the server, outside a view, `billing::pricing_url(&ctx)` is the same page,
for a redirect or an email, and `ctx.admin_url("settings/payments")` any other
page of the admin of the shop. A server route, such as the welcome link of the
plans, gives them for the shop it names, installed or not:
`ctx.pricing_url(&shop)?` and `ctx.admin_url(&shop, &format!("apps/{}",
ctx.app_handle(&shop)?))?` (see [Server routes](../reference/server-routes.md#links-to-the-shopify-admin)).

## 4. Hear of the changes

Subscribe to `app_subscriptions/update` in `shopify.app.toml`, and handle it
with its typed payload:

```toml
[[webhooks.subscriptions]]
topics = ["app_subscriptions/update"]
uri = "/webhooks"
```

```rust,ignore
use openshop::billing::{AppSubscriptionsUpdate, SubscriptionStatus};
use openshop::prelude::*;

#[webhook("app_subscriptions/update")]
pub async fn subscription_updated(ctx: WebhookContext, update: AppSubscriptionsUpdate) -> Result<()> {
    let subscription = update.app_subscription;
    ctx.logger.info(format!("{} is {}", subscription.name, subscription.status));
    Ok(())
}
```

List it in the `webhooks` of `app!`. A flow, a webhook or a route reads what
the shop has now with `ctx.shopify.active_subscriptions().await?`.

## 5. Try it locally

[local-storefront](https://github.com/0xtlt/local-storefront) serves the plan
selection page in its admin, at
`{admin_origin}/admin/charges/{handle}/pricing_plans?shop={shop}`. With
`SHOPIFY_ADMIN_ORIGIN` set (see
[Use custom Shopify endpoints](custom-shopify-endpoints.md)), `pricingUrl` and
`ctx.admin_url(…)` point there; the App Bridge link opens in the admin it is
clicked in. Development stores get test subscriptions (`subscription.test`),
which `require_plan` counts as plans.
