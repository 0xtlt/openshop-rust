# Custom admin pages

To build a screen, see [Add a custom admin page](../guides/add-admin-page.md).

Custom admin pages let an OpenShop app add embedded screens without forking the
framework UI. The API is experimental.

A custom page is one `impl` block, marked `#[admin_page("/path")]`. It holds
what the server runs, the access rule, the loaders, and the actions of the
page, and what the browser shows: the view of the page, compiled to
WebAssembly into the admin UI bundle of the application.

```rust,ignore
// src/main.rs
mod admin;

use openshop::prelude::*;

use admin::reviews::{ReviewDetailPage, ReviewsPage};

static APP: App = app! {
    admin_pages: [ReviewsPage, ReviewDetailPage],
};

fn main() -> Result<()> {
    APP.main()
}
```

`admin_pages` lists the pages. A listed page is served: there is no other
switch.

## Navigation

`nav = "Label"` in the attribute gives a page an entry in the menu of the
admin: `#[admin_page("/reviews", nav = "Reviews")]`. Entries are appended after
the built-in links, in the order `admin_pages` lists the pages. A page without
`nav` is served without appearing in the menu. A page with a dynamic segment,
such as `/reviews/:id`, cannot have an entry: a `nav` on it is rejected when
the configuration is built.

The `admin_menu` setting of `app!` sets the menu by hand, for another order
or another label:

```rust,ignore
use openshop::AdminMenuItem;
use openshop::prelude::*;

static APP: App = app! {
    admin_pages: [ReviewsPage, ReviewDetailPage, ModerationPage],
    admin_menu: [
        AdminMenuItem::new("Moderation", ModerationPage::PATH),
        AdminMenuItem::new("All reviews", ReviewsPage::PATH),
    ],
};
```

It replaces the entries the pages declare: with `admin_menu`, every `nav` is
ignored, and `admin_menu: []` leaves the menu without any entry of the
application. Each path must be a static path of a listed page.

## The home of the admin

A page declared at `/` is what the admin opens on, in place of the built-in
dashboard:

```rust,ignore
pub struct HomePage;

#[admin_page("/")]
impl HomePage {
    #[loader]
    async fn summary(ctx: AdminContext) -> Result<Summary> {
        Ok(Summary { shop: ctx.shop })
    }

    #[view]
    fn view() -> impl IntoView {
        let summary = use_loader(Self::SUMMARY, || ());
        view! { <s-page heading="Welcome">{move || summary.data.get().map(|summary| summary.shop)}</s-page> }
    }
}
```

It is a page like the others, with its access rule, its loaders, its actions
and its view. Two things set it apart:

- it has no menu entry: the name of the app in the Shopify sidebar opens it.
  A `nav` on it, or an `admin_menu` entry for `/`, is rejected when the
  configuration is built;
- it is not a parent of the other pages: its access rule is its own.

An application that
[disables every built-in page](configuration.md#an-admin-made-of-the-pages-of-the-application)
declares one.

## Server functions

`#[admin_page(path)]` on the `impl` block of a type declares a page. The
functions of the block marked `#[access]`, `#[loader]`, or `#[action]` are the
page's access rule, loaders, and actions; the path is also the constant `PATH`
of the type. The function marked [`#[view]`](#views) is what the page shows.

```rust,ignore
// src/admin/reviews.rs
use openshop::AdminPublicError;
use openshop::prelude::*;

#[input]
#[derive(Debug)]
pub struct SaveReview {
    #[schemars(length(min = 1))]
    pub title: String,
}

pub struct ReviewsPage;

#[admin_page("/reviews", nav = "Reviews")]
impl ReviewsPage {
    #[access]
    async fn signed_in(ctx: AdminContext) -> Result<bool> {
        Ok(!ctx.actor.id.is_empty())
    }

    #[loader]
    async fn list_reviews(ctx: AdminContext) -> Result<Value> {
        // Always filter your own queries by the trusted shop and app identity.
        Ok(json!([{ "id": "1", "title": format!("First review of {}", ctx.shop) }]))
    }

    #[action]
    async fn save_review(ctx: AdminContext, input: SaveReview) -> Result<Value> {
        if input.title.len() > 120 {
            bail!(
                AdminPublicError::new("TITLE_TOO_LONG", "Title is too long")
                    .status(422)
                    .field_errors([("title", "At most 120 characters")])
            );
        }
        Ok(json!({ "id": "2", "title": input.title, "savedBy": ctx.actor.id }))
    }
}

pub struct ReviewDetailPage;

#[admin_page("/reviews/:id")]
impl ReviewDetailPage {
    #[loader]
    async fn review(ctx: AdminContext) -> Result<Value> {
        let id = ctx.params.get("id").cloned().unwrap_or_default();
        let title = format!("Review {id}");
        Ok(json!({ "id": id, "title": title }))
    }
}
```

Path rules:

- the path given to the attribute starts with `/`;
- segments contain letters, digits, `_`, and `-`. A dynamic segment is written
  `:name` (or `[name]`); its name starts with a letter and continues with
  letters, digits, and `_`;
- the first segment must not be `api`, `auth`, `webhooks`, `proxy`, `ext`,
  `routes`, `health`, `ops`, `flows`, `runs`, `providers`, `crons`,
  `functions`, or `mcp`;
- `/` alone is [the home of the admin](#the-home-of-the-admin);
- two pages cannot have the same shape, such as `/reviews/:id` and
  `/reviews/:slug`. A static page takes precedence over a dynamic sibling:
  `/reviews/new` is served by its own page, not by `/reviews/:id`.

Invalid paths and navigation entries are rejected when the configuration is
built.

## Loaders and actions

Loaders read and actions write. Both are functions of the page's `impl` block,
marked `#[loader]` or `#[action]`. Each has a name: the name of the function
in camel case (`list_reviews` is `listReviews`), or the name given to the
marker (`#[loader("list")]`). Two functions of a page cannot have the same
name. The view of the page calls them through a
[constant of the page](#views), not by that name.

| Function | Input |
| --- | --- |
| `#[loader] async fn(ctx: AdminContext) -> Result<T>` | None. Whatever the page sends is ignored. |
| `#[loader] async fn(ctx: AdminContext, input: I) -> Result<T>` | Validated against the JSON Schema of `I` and deserialized into it. |
| `#[action] async fn(ctx: AdminContext) -> Result<T>` | None. Any JSON value is accepted and ignored. |
| `#[action] async fn(ctx: AdminContext, input: I) -> Result<T>` | Validated against the JSON Schema of `I` and deserialized into it. |

`I` is a type marked `#[input]`. The attribute derives `Deserialize`,
`Serialize`, and `JsonSchema` for it, without `serde` or `schemars` among the
dependencies of the application, and names its fields in camel case outside
Rust: `product_id` is `productId` in what the page sends. A
`#[serde(rename_all = "…")]` on the type states another rule. The attributes
of `serde` and `schemars` work on the type and on its fields; put `#[input]`
above the other attributes of the type. A typed input needs the `schema` cargo
feature, which is enabled by default. `T` is any `Serialize` value.
`authorize = <rule>` in the marker adds an
[access rule](#authorization-and-errors) of its own to the function. A function
of the block without a marker is an ordinary helper.

The functions receive an `AdminContext`:

| `AdminContext` field | Type | Contract |
| --- | --- | --- |
| `shop` | `String` | Shop of the verified session token. |
| `shopify_app` | `String` | App handle of the verified session token. |
| `actor` | `AdminActor` | `id` is the token's `sub` (the staff user), `session_id` its `sid`. |
| `params` | `IndexMap<String, String>` | Dynamic route segments of the page, percent-decoded. |
| `db` | `PgPool` | The shared `sqlx` pool. |
| `shopify` | `ShopifyClient` | Admin GraphQL client for the shop. |
| `connectors` | `Connectors` | Provider connectors for the shop. |
| `request_id` | `String` | Identifier also present in logs and error responses. |
| `idempotency_key` | `Option<String>` | The `Idempotency-Key` request header, when the client sent one. |
| `app` | `OpenShop` | The running application. |

Nothing is read when the context is built: the access token of the shop is
read at the first Shopify request, and the provider configuration at the first
provider call.

`AdminContext` belongs to a shop, so what needs one takes the context itself:
`Review::of(&ctx)` gives the rows of the shop of the session token, and
`sync_orders::dispatch(&ctx, SyncOrdersInput { limit: 10 }).await?` queues a
run of a flow for it.

SQL written with `sqlx` on `ctx.db` is not tenant-scoped: every such query
must filter by the trusted shop, and by the app handle where applicable.

Custom actions are synchronous and use the hosting platform's request limits. Use
an ordinary server route or an OpenShop flow for long-running or retryable work.

Loaders and actions are called over an internal
`POST /api/pages/custom/<page path>/_rpc/<loader|action>/<name>` transport
(`/api/pages/custom/_rpc/…` for the home)
authenticated with a fresh Shopify App Bridge session token. Endpoint URLs are
not a public API; use `use_loader` and `use_action` in the view of the page.

## Views

The view of a page is the function of its block marked `#[view]`, in the same
file as the loaders and the actions it calls. It is written with
[Leptos](https://leptos.dev) and Shopify's Polaris web components:

```rust,ignore
// src/admin/reviews.rs
use leptos::prelude::*;
use leptos::task::spawn_local;
use openshop::prelude::*;
use openshop::ui::{AdminActionOptions, RouteParams, shopify, use_action, use_loader};
use serde::{Deserialize, Serialize};

#[input]
pub struct SaveReview {
    pub title: String,
}

// What a loader or an action returns is written by the server and read by
// the view.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub title: String,
}

pub struct ReviewsPage;

#[admin_page("/reviews", nav = "Reviews")]
impl ReviewsPage {
    #[loader]
    async fn list_reviews(ctx: AdminContext) -> Result<Vec<Review>> {
        Ok(vec![Review { id: "1".to_string(), title: format!("First review of {}", ctx.shop) }])
    }

    #[action]
    async fn save_review(_ctx: AdminContext, input: SaveReview) -> Result<Review> {
        Ok(Review { id: "2".to_string(), title: input.title })
    }

    #[view]
    fn view() -> impl IntoView {
        let reviews = use_loader(Self::LIST_REVIEWS, || ());
        let save = use_action(Self::SAVE_REVIEW);
        let add = move |_| {
            spawn_local(async move {
                let input = SaveReview { title: "New review".to_string() };
                let options = AdminActionOptions::new().revalidate(Self::LIST_REVIEWS);
                if let Ok(Some(review)) = save.invoke(input, options).await {
                    shopify::toast(&format!("Saved {}", review.title));
                }
            });
        };

        view! {
            <s-page heading="Reviews">
                {move || reviews.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
                {move || {
                    reviews
                        .data
                        .get()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|review| view! { <s-link href=format!("/reviews/{}", review.id)>{review.title}</s-link> })
                        .collect_view()
                }}
                <s-button variant="primary" loading=move || save.pending.get() on:click=add>
                    "Add review"
                </s-button>
            </s-page>
        }
    }
}

pub struct ReviewDetailPage;

#[admin_page("/reviews/:id")]
impl ReviewDetailPage {
    #[loader]
    async fn review(ctx: AdminContext) -> Result<Review> {
        let id = ctx.params.get("id").cloned().unwrap_or_default();
        let title = format!("Review {id}");
        Ok(Review { id, title })
    }

    // The view of a page with dynamic segments takes the route params.
    #[view]
    fn view(params: RouteParams) -> impl IntoView {
        let id = params.get("id").unwrap_or_default().to_string();
        let review = use_loader(Self::REVIEW, || ());
        view! {
            <s-page heading=move || review.data.get().map_or_else(|| format!("Review {id}"), |review| review.title)></s-page>
        }
    }
}
```

A page has at most one view: `fn() -> impl IntoView`, or
`fn(params: RouteParams) -> impl IntoView`. Components it is made of are
ordinary Leptos `#[component]` functions, written outside the block.

Polaris components are plain custom elements. Pass their props as attributes
(`listSlot="primary"`, `disabled=move || busy.get()`), except the `value` and
`checked` of form fields, which only follow the property: use `prop:value` /
`prop:checked` together with `on:input` / `on:change`. Wrap attribute values
that contain `>` in braces.

With a view, the block gains a constant per loader and action, named after the
function: `list_reviews` is `ReviewsPage::LIST_REVIEWS`. The constant is an
`AdminFunctionRef<Input, Output>`: it carries the kind and the name of the
function, the type it takes, and the type it returns. The function states
what it returns as `Result<Output>`, and `Output` implements `Serialize`, for
the server, and `Deserialize`, for the view.

| Item | Contract |
| --- | --- |
| `Page::FUNCTION` | The loader or the action `function` of the page: an `AdminFunctionRef<I, O>`, where `I` is the input sent as JSON, `()` for a function without input, and `O` the output. |
| `use_loader(reference, input)` | Calls the loader when the page mounts, and again whenever the input closure yields a different JSON value or the loader is revalidated. Exposes the signals `data`, `error`, and `loading`, and `revalidate()`. Responses of superseded calls are discarded. |
| `use_action(reference)` | Exposes the signals `data`, `error`, and `pending`, `invoke(input, options)`, and `reset()`. Never retries on its own. |
| `AdminActionOptions` | `.revalidate(loader)` reloads mounted loaders after a success, `.idempotency_key(key)` sets the `Idempotency-Key` header, `.concurrency(ActionConcurrency::Allow)` permits overlapping calls. By default a call made while another one is in flight is skipped and `invoke` returns `Ok(None)`. |
| `AdminRpcError` | Failure of a call: `message`, and the server's `code`, `request_id`, and `field_errors` when it answered with a structured error. |
| `RouteParams::get(name)` | The percent-decoded value of a dynamic segment. |
| `openshop::ui::navigate(path)` | Switches to another embedded page without reloading. |
| `openshop::ui::shopify` | `toast`, `toast_error`, and `toast_with` for App Bridge toasts; `id_token` for a session token. |

The UI asks the server before showing a custom page. A page whose access rule
rejects the staff member shows "This page is disabled." A page without a view,
a page the server knows but the bundle being served does not contain, and a
view that renders an `Err`, show a "Page unavailable" fallback.

### Set up the application

Views need the `ui` feature of `openshop`, and `leptos`, in `Cargo.toml`:

```toml
[dependencies]
openshop = { git = "https://github.com/0xtlt/openshop-rust", tag = "v<version>", features = ["ui"] }
leptos = "0.8"
serde = { version = "1", features = ["derive"] }
```

`openshop init-ui` makes that change and writes an example page; see
[CLI commands](cli.md#scaffold-a-custom-admin-page). Nothing else is added to
the application: there is no second crate, and no file of the bundle to keep.

### What the bundle is made of

`openshop dev` and `openshop build` build the admin UI bundle of an
application whose pages have a view. They compile to WebAssembly, on their
own, the modules of the crate root that declare such pages: with pages in
`src/admin/`, the bundle is the module `admin`. The rest of the application is
the server, and is not compiled into the bundle: `src/main.rs`, flows,
providers, and models are written as in an application without a view.

From a module of pages, the bundle keeps:

- the `#[view]` of each page, and the constants of its loaders and actions.
  The access rule, the loaders, the actions, and the unmarked functions of the
  block are the server's, with whatever their bodies name;
- everything else the module declares: the inputs and the outputs of the
  pages, components, other modules of views.

So, outside the functions the server runs, a module of pages names itself
(`super::components`, `crate::admin::…`) and crates, and nothing else of the
application. What only the server functions need is imported inside them:

```rust,ignore
#[loader]
async fn list_reviews(ctx: AdminContext) -> Result<Vec<Review>> {
    use crate::models::review::ReviewRow;

    let rows = ReviewRow::of(&ctx).all().await?;
    Ok(rows.into_iter().map(Review::from).collect())
}
```

What a view receives is a type of the module, here `Review`, not a model of
the application.

The bundle is compiled with `openshop`, `leptos`, and the dependencies of the
application its modules of pages name outside the server functions, at the
versions of the application's `Cargo.lock`. A crate that only the server
functions use is not compiled for the browser.

The view is also compiled with the server, where nothing calls it:
`cargo check` and your editor report its errors. An error that only the build
of the bundle reports names something the bundle does not have, such as an
import of a server module at the top of the file of a page.

The crate of the bundle is written under the `target` directory of the
project, in `openshop-ui/<package>/`. It is rewritten from the application at
every build: do not edit it.

## Serve the bundle

`openshop build` builds the bundle into `dist/`, optimized for size, then the
application; `openshop build-ui` builds the bundle alone. Tell the server
where the bundle is. During development, `openshop dev` rebuilds it when the
pages change and serves `dist` for you. `dev` and `start` pick the first of:

1. the directory named by the `OPENSHOP_UI_DIR` environment variable;
2. the bundle given to the `admin_ui` setting of `app!`;
3. the bundle built into the framework, which has no custom pages.

| `UiSource` | Serves |
| --- | --- |
| `UiSource::Dir(path)` | The files of a directory on disk, such as `dist`. |
| `UiSource::Assets(lookup)` | A bundle compiled into your binary. `lookup` is a `fn(&str) -> Option<Cow<'static, [u8]>>` that receives an asset path such as `index.html`. |
| `UiSource::Embedded` | The bundle built into the framework. |
| `UiSource::None` | No UI: the server is API-only. |

```rust,ignore
use std::borrow::Cow;

use openshop::UiSource;
use openshop::prelude::*;

#[derive(rust_embed::RustEmbed)]
#[folder = "dist/"]
struct AdminUi;

fn admin_ui(path: &str) -> Option<Cow<'static, [u8]>> {
    AdminUi::get(path).map(|file| file.data)
}

static APP: App = app! {
    admin_ui: UiSource::Assets(admin_ui),
};
```

Every path that is not an API route or a file is answered with the
`index.html` of the bundle, after the server has injected the Shopify API key
into `<meta name="shopify-api-key">` and the App Bridge `<script>` tag.

Assets whose file name carries a content hash are served with
`Cache-Control: public, max-age=31536000, immutable`, other files with
`no-cache`, and the shell with `no-store`. Every asset has a strong `ETag`, a
hash of its bytes (weak, and with `Vary: accept-encoding`, when the response may
be compressed; a `304` carries the same), and a request whose
`If-None-Match` names it is answered `304`. Assets and shell carry
`X-Content-Type-Options: nosniff`. An asset compiled into the binary is sent
from the binary without being copied, and hashed once.

## Authorization and errors

The function marked `#[access]` filters navigation and guards direct page
visits; a page has at most one. Add `authorize = <rule>` to the marker of each
loader or action that needs a narrower policy:
`#[action(authorize = Self::signed_in)]`. A rule is an
`async fn(ctx: AdminContext) -> Result<bool>`, in the block or outside
it. OpenShop evaluates the page's rule, the rule of every listed parent page
(`/reviews` for `/reviews/:id`), and the function's own rule on the server, for
every request. All of them must allow the request.

Return an `AdminPublicError` for failures that are safe to show.
`AdminPublicError::new(code, message)` has status `400`; `.status(…)` and
`.field_errors(…)` change the status and attach per-field messages. Any other
error is logged with a request ID and returned without details.

| Status | `code` | Condition |
| --- | --- | --- |
| `200` | — | The handler's JSON output is the response body. |
| `403` | `FORBIDDEN` | A page or function rule rejected the request. |
| `404` | `NOT_FOUND` | The page, function, or kind does not exist. |
| `422` | `INVALID_INPUT` | The input does not satisfy the function's schema. |
| Your status | Your code | The handler returned an `AdminPublicError`. |
| `500` | `INTERNAL_ERROR` | Any other handler error, a panic, or an access rule that failed. |

Error bodies have the form
`{ "error": "<message>", "code": "<code>", "requestId": "<id>" }`, plus
`fieldErrors` when the error carries them. Unexpected errors always use the
message `Internal server error`.

Each call writes one structured line to the runtime logger with the request
ID, shop, app handle, actor ID, page ID, function name, kind, duration, and
status, but not request or response payloads.

## Testing

`openshop::testing` exports `admin_context` and
`invoke_admin_function` for handler tests. Keep HTTP integration tests for
session token validation, policies, route params, and tenant isolation. See
[Testing](testing.md#test-admin-functions).

## Without the attribute

`#[admin_page]` is built on `AdminPage`, which can be used directly when the
functions of a page are only known at run time:

```rust,ignore
use openshop::prelude::*;
use openshop::builder::{AdminPage, InputSchema};

pub fn reviews_page() -> AdminPage {
    AdminPage::new("/reviews")
        .nav("Reviews")
        .access(|ctx| async move { Ok(!ctx.actor.id.is_empty()) })
        .loader("listReviews", |ctx, _input| async move {
            Ok(json!([{ "id": "1", "title": format!("First review of {}", ctx.shop) }]))
        })
        .action("deleteReview", InputSchema::any(), |_ctx, input| async move {
            Ok(json!({ "deleted": input["id"] }))
        })
}
```

Register it with `.admin_page(reviews_page())` on the builder:
`APP.builder()?` is the builder of the declaration, to add to. A page made by
hand has no view: only an `#[admin_page]` block has one.

`AdminPage::new(path)` starts a page. `.nav(label)` gives it its entry in the
menu, and `.access(rule)` its access rule.

Loaders and actions are `AdminFunction` values registered under a name on
their page:

| Constructor | Kind | Input |
| --- | --- | --- |
| `page.loader(name, handler)` / `AdminFunction::loader(handler)` | Loader | Passed through as a `serde_json::Value` (`null` when absent) unless `.input(schema)` is added. |
| `AdminFunction::typed_loader(handler)` | Loader | Validated against the JSON Schema of `T` and deserialized into it. |
| `page.action(name, schema, handler)` / `AdminFunction::action(schema, handler)` | Action | Validated by the given `InputSchema`. `InputSchema::any()` accepts any JSON value. |
| `AdminFunction::typed_action(handler)` | Action | Validated against the JSON Schema of `T` and deserialized into it. |

`page.function(name, function)` registers an `AdminFunction`. The typed
constructors need the `schema` cargo feature. An action always has an input
schema. `AdminFunction` also has the builder methods `.input(schema)` and
`.authorize(rule)`.

A rule, for `.access(…)` and `.authorize(…)`, is an async function or a
closure that takes the `AdminContext` and returns `Result<bool>`. It is passed
as it is: `.authorize(|ctx| async move { Ok(ctx.actor.id == "1") })`.

Handlers receive the `AdminContext` and the input, and return a JSON
value, or any `Serialize` value for the typed constructors.
