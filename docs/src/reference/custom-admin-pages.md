# Custom admin pages

To build a screen, see [Add a custom admin page](../guides/add-admin-page.md).

Custom admin pages let an OpenShop app add embedded screens without forking the
framework UI. The API is experimental and must be enabled explicitly.

A custom page has two halves:

- a **server half**, an `AdminPage` registered on the configuration builder,
  which holds the page's access rule, loaders, and actions;
- a **client half**, a Rust view compiled into your own WebAssembly admin UI
  bundle with the `openshop-ui` crate.

```rust,ignore
// src/main.rs
use openshop::{CustomAdminNavigationItem, OpenShopConfig, Result};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .custom_pages([CustomAdminNavigationItem {
            label: "Reviews".to_string(),
            path: "/reviews".to_string(),
        }])
        .admin_page(admin::reviews::list_page())
        .admin_page(admin::reviews::detail_page())
        .build()
}
```

`.custom_pages(navigation)` enables the feature; pass an empty list to enable
it without sidebar entries. Navigation entries are appended after built-in
links. Each path must be a static path of a registered page; dynamic detail
pages remain routable without appearing in navigation. Without
`.custom_pages(…)`, registered pages are not served.

## Server pages

`AdminPage::new(path)` declares the server half of a page.

```rust,ignore
// src/admin/reviews.rs
use openshop::{
    AdminPage, AdminPublicError, bail, define_admin_page_access, define_typed_admin_action, json,
};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
pub struct SaveReview {
    #[schemars(length(min = 1))]
    pub title: String,
}

pub fn list_page() -> AdminPage {
    AdminPage::new("/reviews")
        .access(define_admin_page_access(|ctx| async move { Ok(!ctx.actor.id.is_empty()) }))
        .loader("listReviews", |ctx, _input| async move {
            // Always filter your own queries by the trusted shop and app identity.
            Ok(json!([{ "id": "1", "title": format!("First review of {}", ctx.shop) }]))
        })
        .function(
            "saveReview",
            define_typed_admin_action(|ctx, input: SaveReview| async move {
                if input.title.len() > 120 {
                    bail!(
                        AdminPublicError::new("TITLE_TOO_LONG", "Title is too long")
                            .status(422)
                            .field_errors([("title", "At most 120 characters")])
                    );
                }
                Ok(json!({ "id": "2", "title": input.title, "savedBy": ctx.actor.id }))
            }),
        )
}

pub fn detail_page() -> AdminPage {
    AdminPage::new("/reviews/:id").loader("review", |ctx, _input| async move {
        let id = ctx.params.get("id").cloned().unwrap_or_default();
        let title = format!("Review {id}");
        Ok(json!({ "id": id, "title": title }))
    })
}
```

Path rules:

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

Loaders read and actions write. Both are `AdminFunction` values registered
under a name on their page.

| Constructor | Kind | Input |
| --- | --- | --- |
| `page.loader(name, handler)` / `define_admin_loader(handler)` | Loader | Passed through as a `serde_json::Value` (`null` when absent) unless `.input(schema)` is added. |
| `define_typed_admin_loader(handler)` | Loader | Validated against the JSON Schema of `T` and deserialized into it. |
| `page.action(name, schema, handler)` / `define_admin_action(schema, handler)` | Action | Validated by the given `InputSchema`. `InputSchema::any()` accepts any JSON value. |
| `define_typed_admin_action(handler)` | Action | Validated against the JSON Schema of `T` and deserialized into it. |

The typed constructors need the `schema` cargo feature, which is enabled by
default. An action always has an input schema. `AdminFunction` also has the
builder methods `.input(schema)`, `.output(validate)`, and
`.authorize(access)`.

Handlers receive an `AdminServerContext` and the input, and return a JSON
value, or any `Serialize` value for the typed constructors.

| `AdminServerContext` field | Type | Contract |
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

Database access is not automatically tenant-scoped: every app query must filter
by the trusted shop and app handle where applicable.

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

// Kind, name, input and output of the functions registered on the server.
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
2. the bundle set with `.admin_ui(…)` on the configuration builder;
3. the bundle built into the framework, which has no custom pages.

| `UiSource` | Serves |
| --- | --- |
| `UiSource::Dir(path)` | The files of a directory on disk, such as the UI crate's `dist`. |
| `UiSource::assets(lookup)` | A bundle compiled into your binary. `lookup` is a `fn(&str) -> Option<Cow<'static, [u8]>>` that receives an asset path such as `index.html`. |
| `UiSource::Embedded` | The bundle built into the framework. |
| `UiSource::None` | No UI: the server is API-only. |

```rust,ignore
use std::borrow::Cow;

use openshop::{OpenShopConfig, Result, UiSource};

#[derive(rust_embed::RustEmbed)]
#[folder = "ui/dist/"]
struct AdminUi;

fn admin_ui(path: &str) -> Option<Cow<'static, [u8]>> {
    AdminUi::get(path).map(|file| file.data)
}

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .admin_ui(UiSource::assets(admin_ui))
        .build()
}
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

`.access(rule)` on a page filters navigation and guards direct page visits.
Add `.authorize(rule)` to each loader or action that needs a narrower policy.
Rules are created with `define_admin_page_access(|ctx| async move { Ok(bool) })`.
OpenShop evaluates the page's rule, the rule of every registered parent page
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
| `404` | `NOT_FOUND` | Custom pages are not enabled, or the page, function, or kind does not exist. |
| `422` | `INVALID_INPUT` | The input does not satisfy the function's schema. |
| Your status | Your code | The handler returned an `AdminPublicError`. |
| `500` | `INTERNAL_ERROR` | Any other handler error, a panic, an access rule that failed, or an output rejected by `.output(…)`. |

Error bodies have the form
`{ "error": "<message>", "code": "<code>", "requestId": "<id>" }`, plus
`fieldErrors` when the error carries them. Unexpected errors always use the
message `Internal server error`.

Each call writes one structured line to the runtime logger with the request
ID, shop, app handle, actor ID, page ID, function name, kind, duration, and
status, but not request or response payloads.

## Testing

`openshop::testing` exports `create_admin_function_test_context` and
`invoke_admin_function` for handler tests. Keep HTTP integration tests for
session token validation, policies, route params, and tenant isolation. See
[Testing](testing.md#test-admin-functions).
