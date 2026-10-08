# Custom admin pages

To build a screen, see [Add a custom admin page](../guides/add-admin-page.md).

Custom admin pages let an OpenShop app add embedded screens without forking the
framework UI. The API is experimental.

A custom page has two halves:

- a **server half**, the `impl` block of a type marked `#[admin_page("/path")]`,
  which holds the page's access rule, loaders, and actions;
- a **client half**, a Rust view compiled into your own WebAssembly admin UI
  bundle with the `openshop-ui` crate.

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

The `custom_pages` setting of `app!` sets the menu by hand, for another order
or another label:

```rust,ignore
use openshop::CustomAdminNavigationItem;
use openshop::prelude::*;

static APP: App = app! {
    admin_pages: [ReviewsPage, ReviewDetailPage, ModerationPage],
    custom_pages: [
        CustomAdminNavigationItem::new("Moderation", ModerationPage::PATH),
        CustomAdminNavigationItem::new("All reviews", ReviewsPage::PATH),
    ],
};
```

It replaces the entries the pages declare: with `custom_pages`, every `nav` is
ignored, and `custom_pages: []` leaves the menu without any entry of the
application. Each path must be a static path of a listed page.

## Server pages

`#[admin_page(path)]` on the `impl` block of a type declares the server half of
a page. The functions of the block marked `#[access]`, `#[loader]`, or
`#[action]` are the page's access rule, loaders, and actions; the path is also
the constant `PATH` of the type.

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
  `routes`, `health`, `flows`, `runs`, `providers`, `crons`, `functions`, or
  `mcp`, and a page cannot replace Home (`/`);
- two pages cannot have the same shape, such as `/reviews/:id` and
  `/reviews/:slug`. A static page takes precedence over a dynamic sibling:
  `/reviews/new` is served by its own page, not by `/reviews/:id`.

Invalid paths and navigation entries are rejected when the configuration is
built.

## Loaders and actions

Loaders read and actions write. Both are functions of the page's `impl` block,
marked `#[loader]` or `#[action]`. The page view calls them by name: the name
of the function in camel case (`list_reviews` is `listReviews`), or the name
given to the marker (`#[loader("list")]`). Two functions of a page cannot have
the same name.

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
authenticated with a fresh Shopify App Bridge session token. Endpoint URLs are
not a public API; use `use_loader` and `use_action` in the page view.

## Page views

Page views are written with [Leptos](https://leptos.dev) and Shopify's Polaris
web components, in a separate binary crate that targets
`wasm32-unknown-unknown` and is built with [trunk](https://trunkrs.dev). The
crate depends on `openshop-ui`, which provides the built-in screens and the
custom page API:

```toml
[dependencies]
openshop-ui = { git = "https://github.com/0xtlt/openshop-rust", tag = "v<version>" }
leptos = "0.8"
serde = { version = "1", features = ["derive"] }
```

`openshop init-ui` creates this crate in `ui/` with an example page; see
[CLI commands](cli.md#scaffold-a-custom-admin-ui). To set it up by hand, use
the same source and version as your `openshop` dependency, keep the UI crate
out of your server's cargo workspace (declare an empty `[workspace]` table in
its `Cargo.toml`), and copy `index.html` and `Trunk.toml` from the
`openshop-ui` crate with `dist = "dist"`. `apps/demo/ui` in the OpenShop
repository is a complete example.

```rust,ignore
// ui/src/main.rs
use leptos::{prelude::*, task::spawn_local};
use openshop_ui::{
    App, RouteParams,
    admin::{AdminActionOptions, AdminFunctionRef, use_action, use_loader},
    shopify,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Deserialize)]
struct Review {
    id: String,
    title: String,
}

#[derive(Serialize)]
struct SaveReview {
    title: String,
}

// Kind, name, input and output of the functions declared on the server.
const LIST_REVIEWS: AdminFunctionRef<(), Vec<Review>> = AdminFunctionRef::loader("listReviews");
const REVIEW: AdminFunctionRef<(), Review> = AdminFunctionRef::loader("review");
const SAVE_REVIEW: AdminFunctionRef<SaveReview, Review> = AdminFunctionRef::action("saveReview");

#[component]
fn Reviews() -> impl IntoView {
    let reviews = use_loader(LIST_REVIEWS, || ());
    let save = use_action(SAVE_REVIEW);
    let add = move |_| {
        spawn_local(async move {
            let input = SaveReview { title: "New review".to_string() };
            let options = AdminActionOptions::new().revalidate(LIST_REVIEWS);
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

// Pages with dynamic segments take the route params.
fn review_details(params: RouteParams) -> impl IntoView {
    let id = params.get("id").unwrap_or_default().to_string();
    let review = use_loader(REVIEW, || ());
    view! {
        <s-page heading=move || review.data.get().map_or_else(|| format!("Review {id}"), |review| review.title)></s-page>
    }
}

fn main() {
    App::new()
        .page("/reviews", Reviews)
        .page("/reviews/:id", review_details)
        .mount();
}
```

| Item | Contract |
| --- | --- |
| `App::new().page(pattern, view)` | Registers a view for a route pattern. `view` is a function without arguments, such as a `#[component]` without props, or a function taking `RouteParams`. |
| `App::mount()` | Mounts the UI into `<div id="root">`. Panics when a route is reserved or malformed or when two routes have the same shape. |
| `RouteParams::get(name)` | The percent-decoded value of a dynamic segment. |
| `AdminFunctionRef::loader(name)` / `::action(name)` | Typed handle `AdminFunctionRef<I, O>` to a server function: `I` is the input sent as JSON, `O` the output. Use `()` for a function without input. |
| `use_loader(reference, input)` | Calls the loader when the page mounts, and again whenever the input closure yields a different JSON value or the loader is revalidated. Exposes the signals `data`, `error`, and `loading`, and `revalidate()`. Responses of superseded calls are discarded. |
| `use_action(reference)` | Exposes the signals `data`, `error`, and `pending`, `invoke(input, options)`, and `reset()`. Never retries on its own. |
| `AdminActionOptions` | `.revalidate(loader)` reloads mounted loaders after a success, `.idempotency_key(key)` sets the `Idempotency-Key` header, `.concurrency(ActionConcurrency::Allow)` permits overlapping calls. By default a call made while another one is in flight is skipped and `invoke` returns `Ok(None)`. |
| `AdminRpcError` | Failure of a call: `message`, and the server's `code`, `request_id`, and `field_errors` when it answered with a structured error. |
| `openshop_ui::navigate(path)` | Switches to another embedded page without reloading. |
| `openshop_ui::shopify` | `toast`, `toast_error`, and `toast_with` for App Bridge toasts; `id_token` for a session token. |

The UI asks the server before showing a custom page. A page whose access rule
rejects the staff member shows "This page is disabled." A page the server knows
but the bundle does not contain, and a view that renders an `Err`, show a "Page
unavailable" fallback.

## Serve the bundle

Build the UI crate with `trunk build --release`, then tell the server where the
bundle is. During development, `openshop dev` rebuilds a bundle in `ui/` with
`trunk watch` and serves `ui/dist` for you. `dev` and `start` pick the first
of:

1. the directory named by the `OPENSHOP_UI_DIR` environment variable;
2. the bundle given to the `admin_ui` setting of `app!`;
3. the bundle built into the framework, which has no custom pages.

| `UiSource` | Serves |
| --- | --- |
| `UiSource::Dir(path)` | The files of a directory on disk, such as the UI crate's `dist`. |
| `UiSource::assets(lookup)` | A bundle compiled into your binary. `lookup` is a `fn(&str) -> Option<Cow<'static, [u8]>>` that receives an asset path such as `index.html`. |
| `UiSource::Embedded` | The bundle built into the framework. |
| `UiSource::None` | No UI: the server is API-only. |

```rust,ignore
use std::borrow::Cow;

use openshop::UiSource;
use openshop::prelude::*;

#[derive(rust_embed::RustEmbed)]
#[folder = "ui/dist/"]
struct AdminUi;

fn admin_ui(path: &str) -> Option<Cow<'static, [u8]>> {
    AdminUi::get(path).map(|file| file.data)
}

static APP: App = app! {
    admin_ui: UiSource::assets(admin_ui),
};
```

Every path that is not an API route or a file is answered with `index.html`,
after the server has injected the Shopify API key into
`<meta name="shopify-api-key">` and the App Bridge `<script>` tag. Keep those
two tags, App Bridge as the first script, and `<div id="root">` as they are in
the `index.html` of `openshop-ui`, and keep HTML minification off in
`Trunk.toml`.

Assets whose file name carries a content hash are served with
`Cache-Control: public, max-age=31536000, immutable`, other files with
`no-cache`, and the shell with `no-store`.

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
| `500` | `INTERNAL_ERROR` | Any other handler error, a panic, an access rule that failed, or an output rejected by [`.output(…)`](#without-the-attribute). |

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
functions of a page are only known at run time, or need what the attribute
does not state, such as an output validator:

```rust,ignore
use openshop::prelude::*;
use openshop::{AdminPage, InputSchema};

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
`APP.builder()?` is the builder of the declaration, to add to.

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
schema. `AdminFunction` also has the builder methods `.input(schema)`,
`.output(validate)`, and `.authorize(rule)`.

A rule, for `.access(…)` and `.authorize(…)`, is an async function or a
closure that takes the `AdminContext` and returns `Result<bool>`. It is passed
as it is: `.authorize(|ctx| async move { Ok(ctx.actor.id == "1") })`.

Handlers receive the `AdminContext` and the input, and return a JSON
value, or any `Serialize` value for the typed constructors.
