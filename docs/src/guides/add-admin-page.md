# Add a custom admin page

Use this guide to add a **Store** page to an existing app. It displays the trusted
shop identity returned by a server loader. You need a running app installed on a
development store.

A custom page has two halves: a loader declared on the server, and a view
compiled into your own admin UI bundle. The bundle is a small WebAssembly crate
built with [trunk](https://trunkrs.dev). Install the toolchain once:

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
```

> **Caution: experimental API.**
> Custom admin pages are experimental. Check the
> [custom admin page reference](../reference/custom-admin-pages.md) when upgrading.

## 1. Create the server loader

Create `src/admin/store.rs`:

```rust,ignore
use openshop::prelude::*;

pub struct StorePage;

#[admin_page("/store", nav = "Store")]
impl StorePage {
    #[loader]
    async fn store_summary(ctx: AdminContext) -> Result<Value> {
        Ok(json!({ "shop": ctx.shop, "shopifyApp": ctx.shopify_app }))
    }
}
```

Declare the module in a new `src/admin/mod.rs` (`pub mod store;`) and add
`mod admin;` next to the other `mod` lines of `src/main.rs`.

`#[admin_page("/store")]` makes the `impl` block of `StorePage` the server side
of the page, and `nav = "Store"` gives the page an entry in the menu of the
admin. `#[loader]` makes `store_summary` a loader of that page, called
`storeSummary`: the name of the function in camel case.

The loader receives the identity verified by OpenShop. Keep database access,
provider credentials, and other server-only work in loaders and actions: their
context also carries `ctx.db`, `ctx.shopify`, and `ctx.connectors`.

A loader or an action can take what the page sends as a second argument. Mark
the type of that argument with `#[input]`: the input is validated against it
before the function runs, and its fields are named in camel case outside Rust
(`display_name` is `displayName`). The
[custom admin page reference](../reference/custom-admin-pages.md#loaders-and-actions)
shows an action with its input.

## 2. Create the page

Scaffold the UI crate from the project root:

```bash
openshop init-ui
```

This writes a `ui/` directory: a `Cargo.toml` that depends on `openshop-ui`
(the crate containing the built-in screens, pinned to the same release as the
CLI), `Trunk.toml`, `index.html`, and `src/main.rs` with an example page. The
crate is its own cargo workspace, because it is compiled for
`wasm32-unknown-unknown` and not with the server.

Do not edit the `shopify-api-key` meta tag or the App Bridge script tag of
`ui/index.html`: the server injects the Shopify API key into them when it
serves the page.

Replace `ui/src/main.rs` with the **Store** page:

```rust,ignore
use leptos::prelude::*;
use openshop_ui::{
    App,
    admin::{AdminFunctionRef, use_loader},
};
use serde::Deserialize;

#[derive(Clone, Deserialize)]
struct StoreSummary {
    shop: String,
}

// Kind, name, input and output of the loader declared on the server.
const STORE_SUMMARY: AdminFunctionRef<(), StoreSummary> = AdminFunctionRef::loader("storeSummary");

#[component]
fn Store() -> impl IntoView {
    let summary = use_loader(STORE_SUMMARY, || ());

    view! {
        <s-page heading="Store">
            {move || summary.loading.get().then(|| view! { <s-spinner accessibilityLabel="Loading store" /> })}
            {move || summary.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
            {move || summary.data.get().map(|summary| view! { <s-paragraph>{summary.shop}</s-paragraph> })}
        </s-page>
    }
}

fn main() {
    App::new().page("/store", Store).mount();
}
```

`App::new()` contains every built-in screen; `.page(..)` adds yours. The loader
is identified by its name, so `"storeSummary"` must match the server side. The
browser calls the loader with a fresh Shopify session token.

`openshop dev` builds the bundle for you: when the project has a
`ui/Trunk.toml`, it runs `trunk watch` in `ui/` and serves `ui/dist`. To build
it yourself, for example before a deployment:

```bash
cd ui
trunk build --release
```

## 3. List the page and serve the bundle

In `src/main.rs`, where the application is declared, list the page and add the
bundle to serve, keeping your flows and other settings:

```rust,ignore
use openshop::UiSource;
use openshop::prelude::*;

use admin::store::StorePage;

static APP: App = app! {
    // ... providers, flows and other settings ...
    admin_pages: [StorePage],
    admin_ui: UiSource::Dir("ui/dist".into()),
};
```

Listing the page in `admin_pages` is enough to serve it, and its `nav` label
is its entry in the menu. `admin_ui` replaces the built-in bundle with yours;
the directory is resolved from the working directory of the process. The
`OPENSHOP_UI_DIR` environment variable overrides it.

To set the menu by hand, for another order or another label, add the
`custom_pages` setting:
`custom_pages: [openshop::CustomAdminNavigationItem::new("Store", StorePage::PATH)]`.
It replaces the entries the pages declare. `StorePage::PATH` is the path given
to `#[admin_page]`; the configuration is refused, and the app does not start,
when a path of that list does not match a listed page.

Restart `openshop dev` so that it picks up the new `ui/` directory. Open the
app from Shopify admin and select **Store**. You should see the current shop
domain. A **Page unavailable** message means the server knows the page but the
bundle being served does not contain it: check the `trunk` output in the
terminal and the directory being served.

`cargo run -- dev` does not run `trunk`. Without the `openshop` CLI, run
`trunk watch` in `ui/` in a second terminal.

Run `cargo check` in the project root and `trunk build --release` in `ui/`
before deployment.

## Ship the bundle

With `UiSource::Dir`, deploy `ui/dist` next to the binary. The generated
`Dockerfile` does not build or copy a UI bundle; add those steps to it.

To keep a single binary, compile the bundle into it instead. Add the
`rust-embed` crate (`cargo add rust-embed`) and serve the embedded files:

```rust,ignore
use openshop::UiSource;
use openshop::prelude::*;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "ui/dist/"]
struct AdminUi;

static APP: App = app! {
    // ... providers, flows, pages and other settings ...
    admin_ui: UiSource::assets(|path| AdminUi::get(path).map(|file| file.data)),
};
```

`ui/dist` must then exist when the server crate is compiled, so build the
bundle first.

## Restrict access when adding business data

This example is available to authenticated app users. For narrower access, add
a page rule with `#[access]` and per-function rules with `authorize = ..` on
`#[loader]` and `#[action]`:

```rust,ignore
use openshop::prelude::*;

const ALLOWED_STAFF: [&str; 1] = ["1234567890"];

pub struct StorePage;

#[admin_page("/store", nav = "Store")]
impl StorePage {
    #[access]
    async fn staff_only(ctx: AdminContext) -> Result<bool> {
        Ok(ALLOWED_STAFF.contains(&ctx.actor.id.as_str()))
    }

    #[loader]
    async fn store_summary(ctx: AdminContext) -> Result<Value> {
        Ok(json!({ "shop": ctx.shop, "shopifyApp": ctx.shopify_app }))
    }
}
```

`ctx.actor.id` is the Shopify staff user of the session token. The rules are
described in the
[authorization reference](../reference/custom-admin-pages.md#authorization-and-errors).
When querying app-owned tables, read a model through the rows of the shop of
the context, as in `Review::of(&ctx)`, and filter by the app identity where it
matters; a query written with `sqlx` has no such filter.
