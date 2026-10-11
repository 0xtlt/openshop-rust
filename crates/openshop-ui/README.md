# openshop-ui

The embedded Shopify admin UI of OpenShop, written in Rust with [Leptos](https://leptos.dev) and
compiled to WebAssembly. It renders Shopify's Polaris web components (`s-page`, `s-table`, …) and
talks to the Admin API of the OpenShop server with App Bridge session tokens.

The crate is a library plus a default binary. The binary is the stock UI; the library is what
the views of custom admin pages are written with, re-exported as `openshop::ui`.

## Build

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk --locked

trunk build --release
```

The bundle (`index.html` and a hashed `.js` / `.wasm` pair) is written to
`../openshop/ui-dist`. This crate is its own cargo workspace: run `cargo` and `trunk` from this
directory.

```bash
cargo test                                               # pure logic, on the host
cargo clippy --target wasm32-unknown-unknown -- -D warnings
```

## How the server finds the assets

- By default the `openshop` crate embeds `crates/openshop/ui-dist` in the binary (`embedded-ui`
  feature), so rebuild the server after rebuilding the UI.
- `OPENSHOP_UI_DIR=<directory>` serves a bundle from disk instead. That is how an application
  ships a UI with custom pages.

Every path that is not an API route or a file is answered with `index.html`, after the server has
injected the Shopify API key into `<meta name="shopify-api-key">` and the App Bridge `<script>`
tag. Keep those two tags, and App Bridge as the first script, exactly as they are in
[`index.html`](index.html). HTML minification is off in [`Trunk.toml`](Trunk.toml) for that
reason.

## Custom admin pages

An application writes a custom page in one place: an `impl` block marked
`#[admin_page("/reviews", nav = "Reviews")]`, with its loaders, its actions, and its view, a
function marked `#[view]`. The `ui` feature of `openshop` re-exports this library as
`openshop::ui` for that, and the `openshop` command builds the bundle of the application from
its pages. See "Custom admin pages" in the OpenShop documentation, and `apps/demo-custom` for a
complete page, or `apps/demo-public` for an admin made of such pages only.

The library can also be used on its own, for a bundle built by hand in a binary crate that
targets `wasm32-unknown-unknown`:

```toml
[dependencies]
openshop-ui = "0.0.12-beta.0" # or a `path` / `git` dependency
leptos = "0.8"
serde = { version = "1", features = ["derive"] }
```

Copy `index.html` and `Trunk.toml` from this crate (set `dist = "dist"`), then register the pages
in `src/main.rs`:

```rust,no_run
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

Build it with `trunk build --release` and start the server with `OPENSHOP_UI_DIR` pointing at its
`dist` directory.

- **Routes** use the server's patterns: static segments and `:name` segments. Built-in prefixes
  (`/flows`, `/runs`, `/api`, `/ops`, …) are reserved. A page at `/` is the home of the admin, in
  place of the built-in dashboard. Navigation entries and access rules come from the
  server; the UI asks it before showing a page. A page has an entry in the menu when its
  `#[admin_page]` gives it a `nav` label.
- **Functions** are called by name: the name of the server function in camel case (`list_reviews`
  is `listReviews`), or the name given to `#[loader]` or `#[action]`. An input the server declares
  with `#[input]` reads its fields in camel case too (`product_id` is `productId`): give the type
  the UI sends `#[serde(rename_all = "camelCase")]` when a field has several words. A `#[view]`
  needs neither: the page gives it a typed constant per function, and the view sends the
  `#[input]` type itself.
- **`use_loader(reference, input)`** calls a loader when the page mounts and again whenever the
  input closure yields a different JSON value or the loader is revalidated. It exposes `data`,
  `error` and `loading` signals and `revalidate()`, and discards responses of superseded calls.
- **`use_action(reference)`** exposes `data`, `error` and `pending` signals, `invoke(input,
  options)` and `reset()`. A call made while another one is in flight is skipped (`Ok(None)`)
  unless `ActionConcurrency::Allow` is set; options also carry an idempotency key and the loaders
  to revalidate on success. Failures are `AdminRpcError`s with the server's `code`, `request_id`
  and `field_errors`.
- **`openshop_ui::navigate(path)`** switches page without reloading, and `openshop_ui::shopify`
  wraps App Bridge toasts and session tokens.
- A view may render a `Result`: an `Err` shows the "Page unavailable" fallback instead of the page.
  A page the server knows but the bundle does not contain shows the same fallback.

Polaris components are plain custom elements. Pass their props as attributes (`listSlot="primary"`,
`disabled=move || busy.get()`), except the `value` and `checked` of form fields, which only follow
the property: use `prop:value` / `prop:checked` together with `on:input` / `on:change`. Wrap
attribute values that contain `>` in braces.
