# Charge with managed pricing

With Shopify's managed pricing (Shopify App Pricing), the plans of an app are
declared in the Partner Dashboard, and Shopify hosts the page where a merchant
picks one and approves its charge. This guide makes an admin page that needs a
plan: a shop without one is sent to that page, and the app reads the plan from
the Partner API, as Shopify App Pricing asks. The
[billing reference](../reference/billing.md) has every type and function.

## 1. Declare the plans in Shopify

In the Partner Dashboard, open the listing of the app, then **Pricing
content**, **Settings**, and choose **Shopify App Pricing** as the pricing
method. Add the plans, and give each a handle, such as `pro` and `plus`.
OpenShop declares none: the names you use in the code are the handles, or the
names, of these plans.

Give the app its handle in `shopify.app.toml`. Shopify CLI writes it when it
links the app (`shopify app config link`):

```toml
client_id = "…"
name = "Hide PayPal"
handle = "hide-paypal"
```

The handle names the app in the URL of its plan selection page,
`https://admin.shopify.com/store/{store}/charges/hide-paypal/pricing_plans`.
`ctx.app_handle()?` reads it. Without it, the pricing page is an error that
says to add it: Shopify does not take the client ID in that URL.

## 2. Connect the Partner API

Shopify App Pricing keeps the plan of each shop in the Partner API. For the
apps enrolled since April 2026, and for every app after April 28, 2026,
Shopify's
[migration guide](https://shopify.dev/docs/apps/launch/billing/shopify-app-pricing/migrating-to-shopify-app-pricing)
says to "replace GraphQL Admin API checks, such as billing.check() and
currentAppInstallation, with Partner API queries". Their subscriptions may not
be in the Admin API at all.

1. In the Dev Dashboard, open **Organization settings**, **Users**, then
   **Security**. In **Partner API clients**, click **Add Partner API client**,
   give it the **Manage apps** permission, and copy its access token: Shopify
   shows it once.
2. Note the Partner ID: the number in the URL of the Partner Dashboard,
   `https://partners.shopify.com/1234567/…`, not the organization ID in the
   URL of the Dev Dashboard.
3. Give both to the app, in `.env` or the secrets of your host:

   ```bash
   SHOPIFY_PARTNER_ID=1234567
   SHOPIFY_PARTNER_API_TOKEN=prtapi_…
   ```

   Or name the Partner ID in the code, and keep only the token in the
   environment:

   ```rust,ignore
   static APP: App = app! {
       admin_pages: [ReportsPage],
       partner_api: PartnerApi::new().partner_id("1234567"),
   };
   ```

The application then logs where it reads the plans, without the token:

```text
[openshop] Billing reads the plans from the Partner API: https://partners.shopify.com/1234567/api/2026-10/graphql.json (SHOPIFY_PARTNER_ID, token from SHOPIFY_PARTNER_API_TOKEN), each plan kept 300s per shop
```

Without a Partner ID and a token, billing reads the Admin API
(`currentAppInstallation.activeSubscriptions`), as an app on manual pricing,
whose charges the Billing API creates, needs: `activeSubscription` does not
cover the Billing API. A Partner ID without a token, or the reverse, stops the
app at startup. See [Where the plan is read](../reference/billing.md#where-the-plan-is-read).

The Partner API takes four requests a second per Partner API client, for the
whole organization. OpenShop keeps the plan of each shop for five minutes, and
asks again sooner when a check fails or the merchant comes back from the plan
selection page: see [The cache](../reference/billing.md#the-cache) for the
trade-off, and `PartnerApi::plan_ttl` to change it.

## 3. Guard the page

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
        let plan = billing::require_plan(&ctx, &["pro", "plus"]).await?;
        ctx.logger.info(format!("Report for a {} shop", plan.name));
        Ok(json!({ "plan": plan.handle }))
    }

    #[action(authorize = billing::any_plan)]
    async fn export(ctx: AdminContext) -> Result<Value> {
        Ok(json!({ "shop": ctx.shop }))
    }
}
```

Name the plans by their handles: the Admin API and the Partner API both give
them. A shop with neither plan gets `402` with the code `PLAN_REQUIRED`, and
the plan selection page in the `details` of the error: `pricingUrl`, the page
itself, and `pricingLink`, its App Bridge link
(`shopify:admin/charges/hide-paypal/pricing_plans`).

## 4. Send the merchant to the plans

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

A page can show its own "no plan" state instead of an error. Its loader reads
the plan with `ctx.shopify.plan_among(&[…])`, which is the check of
`require_plan` without the `402`: `Some(plan)`, or `None` without one of the
plans. It asks the Partner API again as `require_plan` does, so a merchant
back from the plan selection page sees the new plan at once:

```rust,ignore
#[loader]
async fn report(ctx: AdminContext) -> Result<Value> {
    Ok(match ctx.shopify.plan_among(&["pro", "plus"]).await? {
        Some(plan) => json!({ "plan": plan.handle }),
        None => json!({ "plan": null, "pricingLink": billing::pricing_link(&ctx)? }),
    })
}
```

The view shows the banner when `plan` is `null`, with the link of the loader:

```rust,ignore
let plans = move || {
    report
        .data
        .get()
        .filter(|report| report["plan"].is_null())
        .and_then(|report| report["pricingLink"].as_str().map(str::to_string))
};
```

On the server, outside a view, `billing::pricing_url(&ctx)?` is the same page,
for a redirect or an email, and `ctx.admin_url("settings/payments")` any other
page of the admin of the shop. A server route gives them for the shop it names,
installed or not: `ctx.pricing_url(&shop)?` and `ctx.admin_url(&shop,
&format!("apps/{}", ctx.app_handle(&shop)?))?` (see
[Server routes](../reference/server-routes.md#links-to-the-shopify-admin)).

## 5. The welcome link and the changes

After the merchant approves a plan, Shopify sends them to the welcome link of
the plan, with `plan_handle` in its query. Shopify App Pricing sends no webhook
for a change of plan after April 28, 2026: the welcome link and the Partner API
are how the app hears of it.

- A welcome link that is a path of the embedded app opens in the admin, which
  signs its launch. OpenShop forgets the plan it kept for the shop when a
  verified launch has `plan_handle` (or `charge_id`), so the next check reads
  the new plan.
- A welcome link served by a server route of the app reads the plan again
  with `refresh_plans()`, for a launch Shopify signed only, and as a best
  effort, as below.
- A plan that ends outside the app, a cancellation or a frozen shop, is seen
  within five minutes, when the kept plan expires.

The admin opens the welcome link in the app's frame, with a query Shopify
signs. But anyone can call a route, and the Partner API takes four requests a
second for the whole organization: an open route must not spend that rate
limit. So the route reads the plan again only for a launch Shopify signed,
`ctx.shopify_launch()`, that carries `plan_handle` (or `charge_id`). A call
without the signature asks the Partner API nothing: the app's next plan check
reads the plan anyway, because the kept "no plan" answer is from when the
merchant was sent to the plans, more than ten seconds earlier.

Read it as a best effort. Do not use `?` on `for_shop` or `refresh_plans()`
there: when the Partner API fails, a merchant who just paid would land on a
`500`. The route logs the failure and sends the merchant into the app. A call
without the signature names its shop in the query: `ctx.request.query_shop()`
checks it, and the route answers `400` when it is missing or not a shop
domain.

```rust,ignore
// src/routes/welcome.rs
use openshop::prelude::*;
use openshop::response::{self, IntoResponse, Response, StatusCode, header};

/// The welcome link of a plan, which the admin opens in the app's frame.
#[route(GET, "/welcome")]
pub async fn welcome(ctx: RouteContext) -> Result<Response> {
    let Some(launch) = ctx.shopify_launch() else {
        // Not signed by Shopify: the app's page in the admin, and no Partner API call.
        let Some(shop) = ctx.request.query_shop() else {
            let error = json!({ "error": "Missing or invalid shop" });
            return Ok(response::json_with_status(StatusCode::BAD_REQUEST, error));
        };
        let location = ctx.admin_url(&shop, &format!("apps/{}", ctx.app_handle(&shop)?))?;
        return Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response());
    };
    // Only a launch Shopify signed reads the plan again: an open route must
    // not spend the organization's rate limit of the Partner API.
    if launch.is_plan_welcome() {
        let refreshed = async { ctx.for_shop(&launch).await?.shopify.refresh_plans().await };
        if let Err(error) = refreshed.await {
            ctx.logger.warn(format!("The plan of {} was not read again: {error:#}", launch.shop));
        }
    }
    // Still in the admin's frame: the app's home, with the signed query.
    let location = format!("/?{}", launch.query);
    Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
}
```

A signed launch stays in the admin's frame: the route sends it to the app's
home with the signed query, which the home verifies again. Shopify's admin
cannot be opened in a frame. The home keeps the plan the route just read: one
Partner API request per plan change. `ctx.logger` writes the warning with the route
(see [Server routes](../reference/server-routes.md#logging)).

A flow, a webhook or a route reads what the shop has now with
`ctx.shopify.active_plans().await?`. Not with `active_subscriptions()`: it
reads the Admin API, where a subscription of Shopify App Pricing may not be,
so a paying merchant would look like one without a plan. With the Partner API
set, its first call logs a warning that says where it was called.

An app enrolled in Shopify App Pricing before April 2026, and an app of the
Billing API, still get `app_subscriptions/update` until then. Its typed payload
is `AppSubscriptionsUpdate`: see
[The `app_subscriptions/update` webhook](../reference/billing.md#the-app_subscriptionsupdate-webhook).

## 6. Test it

A test gives the shop its plan by handle, without writing what Shopify
answers, and calls the loader of the page as its view does. A test reads no
environment variable: give it the Partner API in `TestOptions::partner_api`.
The Partner ID or the token it lacks is the test's own (`1234567`,
`prtapi_test`), and the requests go to the mock transport:

```rust,ignore
use std::time::Duration;

use my_app::APP;
use my_app::pages::reports::ReportsPage;
use openshop::billing::PLAN_REQUIRED;
use openshop::prelude::*;
use openshop::testing::{TestContext, TestOptions, app_pricing_subscription};

async fn context() -> TestContext {
    let options = TestOptions {
        // The Partner API, asked at each check: the test changes the plan.
        partner_api: PartnerApi::new().partner_id("1234567").plan_ttl(Duration::ZERO),
        ..TestOptions::installed()
    };
    TestContext::with_options(APP.config().unwrap(), options).await.unwrap()
}

#[openshop::test(context())]
async fn a_shop_without_a_plan_is_sent_to_the_plans(ctx: &TestContext) {
    ctx.shopify.active_subscription(None);
    let error = ctx.call_loader::<Value>(ReportsPage::PATH, "report", ()).await.unwrap_err();
    assert_eq!((error.code.as_str(), error.status), (PLAN_REQUIRED, 402));
    // The `handle` of the `shopify.app.toml` of the project.
    assert_eq!(error.details.unwrap()["pricingLink"], "shopify:admin/charges/my-app/pricing_plans");

    ctx.shopify.active_subscription(app_pricing_subscription("pro"));
    let report: Value = ctx.call_loader(ReportsPage::PATH, "report", ()).await.unwrap();
    assert_eq!(report["plan"], "pro");
}
```

An app that reads the Admin API answers with
`ctx.shopify.active_subscriptions([subscription("Pro")])` instead. See
[Answer the Admin API](../reference/testing.md#answer-the-admin-api) and
[Test admin functions](../reference/testing.md#test-admin-functions).

## 7. Try it locally

[local-storefront](https://github.com/0xtlt/local-storefront) (lsf) serves the
plan selection page in its admin, at
`{admin_origin}/admin/charges/{handle}/pricing_plans?shop={shop}`, and the
Partner API at Shopify's path on its own origin. Start it with the plans in a
file (lsf's `docs/admin.md`, "Managed pricing"):

```bash
lsf app dev --app . --app-url http://localhost:3000 --port 9393 --pricing-plans pricing.toml
```

It prints the Partner API it serves, its Partner ID and its token, and
`GET http://localhost:9393/__lsf/admin/partner` gives the same as JSON. Point
the app at it (see
[Use custom Shopify endpoints](custom-shopify-endpoints.md)):

```bash
SHOPIFY_ADMIN_ORIGIN=http://localhost:9393
SHOPIFY_PARTNER_ORIGIN=http://localhost:9393
SHOPIFY_PARTNER_ID=…          # partnerId of /__lsf/admin/partner
SHOPIFY_PARTNER_API_TOKEN=…   # token of /__lsf/admin/partner, prtapi_lsf_…
```

`pricingUrl` and `ctx.admin_url(…)` point to lsf's admin, the App Bridge link
opens in the admin it is clicked in, and the plans chosen there are what
`activeSubscription` answers. `POST /__lsf/admin/pricing-plans/{handle}/select`
and `POST /__lsf/admin/pricing-plans/cancel` choose and end a plan without the
page.
