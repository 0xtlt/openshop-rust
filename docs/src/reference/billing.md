# Billing

`openshop::billing` works with Shopify's managed pricing (Shopify App
Pricing): the plans are declared in the Partner Dashboard, and Shopify hosts
the page where a merchant picks one. OpenShop declares no plan of its own. An
application reads what the shop is subscribed to and sends the merchant to the
plan selection page. To set it up step by step, see
[Charge with managed pricing](../guides/charge-with-managed-pricing.md).

## Where the plan is read

The configuration says where the plans are read, never a guess.
`app.billing_source()` returns it, a `BillingSource`:

| `BillingSource` | When | What is read |
| --- | --- | --- |
| `AdminApi` | No [Partner API](#the-partner-api) is set: the default | `currentAppInstallation.activeSubscriptions` of the Admin API of the shop, at each check. |
| `PartnerApi` | A Partner ID and a token are set | `activeSubscription(appId:, shopId:)` of the [Partner API](https://shopify.dev/docs/api/partner/2026-07/active-subscription), kept per shop (see [The cache](#the-cache)). |

Which one an app needs:

- An app enrolled in Shopify App Pricing since April 2026, and every app on
  Shopify App Pricing after April 28, 2026: the Partner API. Shopify's
  [migration guide](https://shopify.dev/docs/apps/launch/billing/shopify-app-pricing/migrating-to-shopify-app-pricing)
  says: "Replace GraphQL Admin API checks, such as billing.check() and
  currentAppInstallation, with Partner API queries." Its subscriptions may have
  no Admin API subscription at all: `legacySubscriptionId` is null for a
  subscription created in Shopify App Pricing. With the Admin API, a paying
  merchant would get `402`.
- An app on manual pricing, whose charges the Billing API creates: the Admin
  API. `activeSubscription` covers the contracts of Shopify App Pricing only, and
  answers `null` for a subscription of the Billing API that was not migrated.
- An app enrolled before April 2026: either. Its subscriptions are in the Admin
  API, and in the Partner API with their `legacySubscriptionId`.

The source is the same for every Shopify app the instance serves.

## The Partner API

`PartnerApi` holds the credentials of the Partner API: the Partner ID, the
number in the URL of the Partner Dashboard (`partners.shopify.com/1234567/…`,
not the organization ID of the Dev Dashboard), and the access token of a
Partner API client of the organization that owns the app, with the "Manage
apps" permission. One token serves every app and every shop of the
organization. See [Partner API](https://shopify.dev/docs/api/partner).

Each field comes from the first place that sets it:

1. a call at startup, `APP.main_with(|app| app.partner_api(…))`;
2. the `partner_api` setting of `app!`;
3. the environment: `SHOPIFY_PARTNER_ID` and `SHOPIFY_PARTNER_API_TOKEN`.

```rust,ignore
static APP: App = app! {
    admin_pages: [ReportsPage],
    // The Partner ID in the code; the token from SHOPIFY_PARTNER_API_TOKEN.
    partner_api: PartnerApi::new().partner_id("1234567"),
};
```

| `PartnerApi` method | Field | Default |
| --- | --- | --- |
| `.partner_id(id)` | `partner_id` | `SHOPIFY_PARTNER_ID` |
| `.token(token)` | `token` | `SHOPIFY_PARTNER_API_TOKEN` |
| `.plan_ttl(duration)` | `plan_ttl` | Five minutes (`DEFAULT_PLAN_TTL`). `Duration::ZERO` keeps nothing. |

Set both, or neither. A Partner ID without a token, or a token without a
Partner ID, stops the application at startup, and so does a Partner ID that is
not a number. The token is a secret: `Debug` hides it, and no error or log line
shows it. An error names the setting or the variable, never the value:

```text
[openshop] Invalid config: the Partner API has a Partner ID (partner_api.partner_id) but no token: set SHOPIFY_PARTNER_API_TOKEN or partner_api.token, or remove the Partner ID to read the plans from the Admin API
```

With the Partner API, the process logs one line at startup:

```text
[openshop] Billing reads the plans from the Partner API: https://partners.shopify.com/1234567/api/2026-10/graphql.json (partner_api.partner_id, token from SHOPIFY_PARTNER_API_TOKEN), each plan kept 300s per shop
```

The requests go to
`POST https://partners.shopify.com/{partner_id}/api/2026-10/graphql.json`,
with the token in `X-Shopify-Access-Token`. The `partner_origin` of the
[Shopify endpoints](../guides/custom-shopify-endpoints.md) moves the origin,
for a simulator such as local-storefront.

`app.partner_api()` returns the client, a `PartnerApiClient`, for any other
query of the Partner API, such as `events`:
`app.partner_api().unwrap().graphql(query, variables).await?` returns its
`data`. A `429`, a `5xx` or a network failure is sent again as the
[`shopify_retry`](configuration.md) of the application says; a mutation is sent
again only after a `429`.

## The cache

The Partner API takes four requests a second per Partner API client, for
every shop and every process of the organization together. A plan read at each
request of each merchant would go over it. So each process keeps, per Shopify
app and shop:

- the GIDs the Partner API takes, which never change: the app's
  (`currentAppInstallation { app { id } }`) and the shop's (`shop { id }`),
  read once from the Admin API of the shop (the operation
  `OpenShopPartnerIds`);
- the last answer of `activeSubscription`, for `plan_ttl`: five minutes by
  default.

| Event | What happens |
| --- | --- |
| A check passes with the kept answer | It is used until it is `plan_ttl` old. |
| A check does not pass (no plan, or not the one asked) | The Partner API is asked again, unless the answer is younger than ten seconds (`billing::MIN_REFRESH`). A merchant who just chose a plan gets it at once; a merchant without one costs at most one request every ten seconds. |
| A verified launch of the app with `plan_handle` (or `charge_id`) | The answer is forgotten: it is the welcome link of a plan, which the admin opens signed. Not an answer younger than ten seconds that has the plan of `plan_handle`: the [route of the welcome link](#the-welcome-link-of-a-route) just read it, after the plan was chosen. A "no plan" answer of the same age is forgotten: it is the check that sent the merchant to the plans. |
| `app/uninstalled` | The answer is forgotten. |
| `ctx.shopify.refresh_plans()` | The Partner API is asked again, whatever the age of the answer: for the welcome link of a plan that a server route serves, when Shopify signed it (see [The welcome link of a route](#the-welcome-link-of-a-route)). Not for every request: the Partner API takes four requests a second for the whole organization. |
| Concurrent checks of a shop | One request; the others wait for it. |

The trade-off: a plan that ends outside the app (a cancellation by Shopify, a
frozen shop) is seen within `plan_ttl`, and the merchant keeps the plan's
features until then. Shopify App Pricing sends no webhook for these changes
after April 28, 2026, so nothing else could tell the app sooner. A plan chosen
or changed on the plan selection page is seen at once. With `plan_ttl` at five
minutes, the checks of about 1,200 shops in use at the same time fit in four
requests a second; a shorter `plan_ttl` sees an ended plan sooner, and spends
more of the rate limit. Each process keeps its own answers: give every process
the same `plan_ttl`.

The Admin API is asked at each check: its rate limit is per shop.

## The plans of a shop

`ctx.shopify.active_plans()` reads the plans of the shop from the source of
the application, through the client of any context (`ctx.shopify`):

```rust,ignore
let plans = ctx.shopify.active_plans().await?;
if let Some(plan) = plans.plan(&["pro", "plus"]) {
    ctx.logger.info(format!("On {} ({:?})", plan.name, plan.source()));
}
```

| `ActivePlans` | Returns |
| --- | --- |
| `plan(&["pro", "Plus"])` | `Option<&Plan>`: the plan among these the shop has. Each is the handle of a plan or its name. With `&[]`, any plan. |
| `has_plan(&[…])` | `bool`: whether `plan(&[…])` finds one. |
| `[index]`, `iter()`, `len()`… | The plans: `ActivePlans` dereferences to `[Plan]`. |
| `into_vec()` | `Vec<Plan>`. |

| `Plan` field | Type | Admin API | Partner API |
| --- | --- | --- | --- |
| `name` | `String` | The `name` of the subscription | The `description` of the item of the plan, else its handle |
| `handle` | `Option<String>` | The `planHandle` of its recurring line | The `handle` of the item of the plan |
| `subscription` | `PlanSubscription` | `AdminApi(AppSubscription)` | `PartnerApi(AppPricingSubscription)` |

`plan.source()` is its `BillingSource`, and `plan.is_named(name)` compares its
handle and its name. The item of the plan is the first item with a flat rate,
else the first item: the other items of a subscription of the Partner API are
the events of the App Events API. Prefer handles to names: both sources have
them, and the `description` of the Partner API may not be the name of the plan
in the Partner Dashboard. A `Plan` serializes as
`{ "name", "handle", "source": "adminApi" | "partnerApi", "subscription" }`.

On the Admin API, the plans are the subscriptions whose status is `ACTIVE`, a
trial included. A `PENDING` one, which the merchant has not approved, is not.
On the Partner API, the subscription is active by definition: Shopify answers
`null` without one.

### Admin API: `AppSubscription`

`ctx.shopify.active_subscriptions()` reads
`currentAppInstallation.activeSubscriptions` whatever the source: one request
to the Admin API, which needs no access scope. An app on manual pricing reads
its plans there. An app on Shopify App Pricing does not: its subscriptions may
have no Admin API subscription, and a paying merchant would look like one
without a plan. Call `active_plans()` there.

With the Partner API set, the first call of `active_subscriptions()` in the
process logs a warning that says where it was called. The call still reads the
Admin API and returns what it did before; the warning is not repeated:

```text
[openshop] active_subscriptions() at src/webhooks/orders.rs:12 reads the Admin API, but billing reads the plans from the Partner API. A subscription of Shopify App Pricing may not be in the Admin API, so a paying merchant can look like one without a plan. Call active_plans() instead, or keep active_subscriptions() only to read the Admin API on purpose. This warning is logged once per process.
```

An app enrolled before April 2026 that reads its legacy subscriptions on
purpose keeps the call, and gets this line once per process.

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

`ActiveSubscriptions` has `plan(&[…])`, `has_plan(&[…])` and `into_vec()`
too. An amount is a `Money`, whose `amount` is the decimal written as text
(`"9.99"`) and `currency_code` its currency. The request is the operation
`OpenShopActiveSubscriptions` (`billing::ACTIVE_SUBSCRIPTIONS_OPERATION`).

### Partner API: `AppPricingSubscription`

| `AppPricingSubscription` field | Type | Shopify field |
| --- | --- | --- |
| `billing_period` | `String` | `billingPeriod`: `EVERY_30_DAYS` or `ANNUAL` |
| `cancel_at_end_of_cycle` | `bool` | `cancelAtEndOfCycle` |
| `trial_ends_at` | `Option<DateTime<Utc>>` | `trialEndsAt`: `None` without a trial, or after it |
| `current_billing_cycle` | `Option<BillingCycle>` | `currentBillingCycle` (`start_time`, `end_time`): `None` during the trial |
| `items` | `Vec<AppPricingItem>` | `items`: `handle`, `description`, `price` (`AppPricingPrice::FlatRate` with `active`, `amount`, `currency`; `::Tiered` with `tiers_mode` and `tiers`; or `::Other`), `discount`, `usage` |
| `pending_update` | `Option<PendingUpdate>` | `pendingUpdate`: what changes at the next billing cycle |
| `legacy_subscription_id` | `Option<String>` | `legacySubscriptionId`: the Admin API's `AppSubscription` GID of a migrated subscription; `None` for one created in Shopify App Pricing |

`subscription.plan_item()` is the item of the plan. The request is the
operation `OpenShopPartnerSubscription`
(`billing::PARTNER_SUBSCRIPTION_OPERATION`). "Shop not found" and the other
errors of the Partner API fail the call.

## Guard an admin page

`billing::require_plan(&ctx, &plans)` guards a loader or an action: it returns
the `Plan` among `plans` the shop has, or any plan when `plans` is empty, read
from the source of the application. Without one, it fails with an
`AdminPublicError` that the page receives:

```rust,ignore
use openshop::billing;
use openshop::prelude::*;

pub struct ReportsPage;

#[admin_page("/reports", nav = "Reports")]
impl ReportsPage {
    #[loader]
    async fn report(ctx: AdminContext) -> Result<Value> {
        let plan = billing::require_plan(&ctx, &["pro", "plus"]).await?;
        Ok(json!({ "plan": plan.name }))
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

On the Partner API, a kept answer without the plan is asked again first,
unless it is younger than ten seconds (see [The cache](#the-cache)).

### A page with its own "no plan" state

`ctx.shopify.plan_among(&plans)` is the check of `require_plan` without the
error: `Result<Option<Plan>>`, the plan among `plans` the shop has, or `None`.
It reads the plan with the same rule: a kept answer without the plan is asked
again once it is ten seconds old. A loader that shows a "no plan" state,
instead of answering `402`, calls it:

```rust,ignore
#[loader]
async fn report(ctx: AdminContext) -> Result<Value> {
    Ok(match ctx.shopify.plan_among(&["pro", "plus"]).await? {
        Some(plan) => json!({ "plan": plan.handle }),
        None => json!({ "plan": null, "pricingLink": billing::pricing_link(&ctx)? }),
    })
}
```

It works in any context with a `ctx.shopify`: a flow, a webhook, the shop of a
route. A failure to ask Shopify is an error, not `None`. `active_plans()` reads
the plans too, but keeps any answer for `plan_ttl`: a "no plan" it kept is
not asked again sooner.

### The response of `require_plan`

The response of a call without the plan:

```json
{
  "error": "This feature needs the pro or plus plan",
  "code": "PLAN_REQUIRED",
  "requestId": "…",
  "details": {
    "pricingUrl": "https://admin.shopify.com/store/demo/charges/my-app/pricing_plans",
    "pricingLink": "shopify:admin/charges/my-app/pricing_plans"
  }
}
```

with status `402`. Without plans, the message is
`Choose a plan to use this feature`. `billing::plan_required(&ctx, &plans)?`
builds the same error, for a check of your own, and `billing::PLAN_REQUIRED` is
its code. A request to Shopify that fails is not a missing plan: it is an
error of the server (`500`), so a paying merchant is not sent to the pricing
page because Shopify did not answer. So is an app without a
[handle](#the-handle-of-the-app), whose pricing page cannot be named.

In a view, the error of `use_loader` or `use_action` carries the details:
`error.code.as_deref() == Some("PLAN_REQUIRED")`, and
`error.detail("pricingLink")` (the view names the code as text:
`openshop::billing` is not compiled into the bundle). Open the link in the top
window of the admin, as an App Bridge link: `window.open(link, "_top")`, or an
`<a target="_top">` to it.

## The welcome link of a route

After the merchant approves a plan, Shopify sends them to the welcome link of
the plan. The admin opens it in the app's frame, signed like any launch, with
`plan_handle` (and `charge_id` for the apps enrolled before April 2026) in its
query. When a page of the embedded app is the welcome link, its launch makes the
next check read the plan again (see [The cache](#the-cache)). When a server
route serves it, the route reads the plan with `refresh_plans()`:

```rust,ignore
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

- **Only for a launch Shopify signed.** Anyone can call a route. An open
  route must not spend the organization's rate limit: four requests a second
  for every shop and every process. A delay per shop would not stop that: one
  call per installed shop is enough. `ctx.shopify_launch()` is the launch when
  its `hmac` verifies (see
  [Server routes](server-routes.md#a-launch-shopify-signed)), and
  `is_plan_welcome()` says that it carries `plan_handle` or `charge_id`.
- **Without the signature, no Partner API call.** The app's next plan check
  reads the plan anyway: the kept "no plan" answer is from when the merchant
  was sent to the plans, more than ten seconds (`billing::MIN_REFRESH`)
  earlier.
- **A best effort.** Do not use `?` on `for_shop(…)` or `refresh_plans()`:
  when the Partner API fails, a merchant who just paid would land on a `500`.
  The route logs the failure and goes on.
- **Into the app, in the frame.** The admin runs the route in the app's
  frame, and Shopify's admin cannot be framed (`X-Frame-Options: DENY`). So a
  signed launch goes to the app's home, `/?{launch.query}`, which verifies the
  same query again within its 90 seconds. The home keeps the plan the route
  just read, since it has the plan of `plan_handle` and is younger than ten
  seconds (see [The cache](#the-cache)): one Partner API request per plan
  change. A call without the signature is not from the admin's frame: it goes
  to the app's page in the admin.
- **A bad shop is `400`.** Without the signature, the route reads `shop` from
  the query with `ctx.request.query_shop()`, which is `None` when it is
  missing or not a `*.myshopify.com` domain. `admin_url` would fail on it, and
  `?` would make that a `500`.

## The pricing page and the pages of the admin

| Function | Returns |
| --- | --- |
| `billing::pricing_url(&ctx)` | `Result<String>`: the plan selection page of the app, for the shop of the context: `https://admin.shopify.com/store/{store}/charges/{app_handle}/pricing_plans`. With an `admin_origin` ([Shopify endpoints](../guides/custom-shopify-endpoints.md)), `{admin_origin}/admin/charges/{app_handle}/pricing_plans?shop={shop}`. |
| `billing::pricing_link(&ctx)` | `Result<String>`: `shopify:admin/charges/{app_handle}/pricing_plans`, the App Bridge link of the same page, for a view of an embedded page. |
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
`ctx.app_handle()?`: the `handle` of its `shopify.app.toml`, which Shopify CLI
writes when it links the app. It is not `ctx.shopify_app`, the name OpenShop
gives the app in its configuration (`default` for the app of the environment),
which keeps its meaning.

| Configuration | `app_handle()` |
| --- | --- |
| The app of the environment (`SHOPIFY_API_KEY`) | The top-level `handle` of the `shopify.app*.toml` whose `client_id` is `SHOPIFY_API_KEY`; without `SHOPIFY_API_KEY`, of `shopify.app.toml`. |
| `ShopifyAppConfig::Toml` | The `handle` of its file. |
| `ShopifyAppConfig::Keys` | The `handle` of the `shopify.app*.toml` whose `client_id` is its `api_key`. |

Without a `handle` in the file, `app_handle()`, `pricing_url`, `pricing_link`,
`plan_required` and the `402` of `require_plan` fail with an error that says
to add it. The client ID does not take its place: Shopify's plan selection page
does not take it.

```text
[openshop] The Shopify app "default" has no handle: add `handle = "<your-app-handle>"` at the top of the shopify.app*.toml whose client_id is "…", next to client_id (`shopify app config link` writes it). The pages of the Shopify admin, such as the plan selection page, name the app by its handle, and Shopify does not take the client ID there.
```

The pages that open the app take its client ID: the redirect at the end of
OAuth goes to `/admin/apps/{client_id}`, with or without a handle. Keep the
`shopify.app*.toml` files next to the binary in production, as the generated
`Dockerfile` does.

## The `app_subscriptions/update` webhook

Shopify App Pricing no longer sends `app_subscriptions/update` after
April 28, 2026: read the plan from the Partner API instead. The Billing API,
and the apps enrolled in Shopify App Pricing before April 2026 until that
date, send it when a subscription of the shop changes: a plan chosen,
approved, cancelled, frozen… `AppSubscriptionsUpdate` is its payload (API
2026-10):

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
(`/webhooks`). The payload says what changed; read `active_plans()` for what
the shop has now.
