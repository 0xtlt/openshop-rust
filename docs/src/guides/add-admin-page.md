# Add a custom admin page

Use this guide to add a **Store** page to an existing app. It displays the trusted
shop identity returned by a server loader. You need a running app installed on a
development store.

A custom page has two halves: a loader registered on the server, and a view
compiled into your own admin UI bundle. The bundle is a small WebAssembly crate
built with [trunk](https://trunkrs.dev). Install the toolchain once:

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
```

> **Caution: experimental API.**
> Custom admin pages are enabled with `.custom_pages(..)` on the builder. Check the
> [custom admin page reference](../reference/custom-admin-pages.md) when upgrading.

## 1. Create the server loader

Create `src/admin/store.rs`:

```rust,ignore
use openshop::{AdminPage, json};

pub fn page() -> AdminPage {
    AdminPage::new("/store").loader("storeSummary", |ctx, _input| async move {
        Ok(json!({ "shop": ctx.shop, "shopifyApp": ctx.shopify_app }))
    })
}
```

Declare it in a new `src/admin/mod.rs` (`pub mod store;`) and add `mod admin;`
next to the other `mod` lines of `src/main.rs`.

The loader receives the identity verified by OpenShop. Keep database access,
provider credentials, and other server-only work in loaders and actions: their
context also carries `ctx.db`, `ctx.shopify`, and `ctx.connectors`.

## 2. Create the page

Create a second crate in a `ui/` directory of the project. It depends on
`openshop-ui`, which contains the built-in screens. `ui/Cargo.toml`:

```toml
[package]
name = "my-app-ui"
version = "0.1.0"
edition = "2024"
publish = false

# Built with trunk for wasm32; kept out of the server crate.
[workspace]

[dependencies]
openshop-ui = { git = "https://github.com/0xtlt/openshop-rust", tag = "<tag>" }
leptos = "0.8"
serde = { version = "1", features = ["derive"] }
```

Replace `<tag>` with the tag of the `openshop` dependency in the project's own
`Cargo.toml`, so that both halves come from the same release.

Copy `index.html` and `Trunk.toml` from `crates/openshop-ui` in the OpenShop
repository into `ui/`, and set the output directory in `ui/Trunk.toml`:

```toml
[build]
target = "index.html"
dist = "dist"
public_url = "/"
# The server rewrites head tags of index.html at serve time: keep them byte-for-byte.
minify = "never"
no_sri = true
```

Do not edit the `shopify-api-key` meta tag or the App Bridge script tag of
`index.html`: the server injects the Shopify API key into them when it serves
the page.

Create `ui/src/main.rs`:

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

// Kind, name, input and output of the loader registered on the server.
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

Build the bundle:

```bash
cd ui
trunk build --release
```

This writes `ui/dist/`. Rebuild it after every change to the page:
`openshop dev` rebuilds and restarts the server when `src/` changes, but it does
not build the UI crate.

## 3. Enable navigation

In `src/main.rs`, register the page, its sidebar entry, and the bundle to serve,
keeping your flows and other settings:

```rust,ignore
use openshop::{CustomAdminNavigationItem, UiSource};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        // ... providers, flows and other settings ...
        .custom_pages([CustomAdminNavigationItem {
            label: "Store".to_string(),
            path: "/store".to_string(),
        }])
        .admin_page(admin::store::page())
        .admin_ui(UiSource::Dir("ui/dist".into()))
        .build()
}
```

`build()` fails when a navigation path does not match a registered page.
`.admin_ui(..)` replaces the built-in bundle with yours; the directory is
resolved from the working directory of the process. The `OPENSHOP_UI_DIR`
environment variable overrides it.

Start development again if it is not running. Open the app from Shopify admin
and select **Store**. You should see the current shop domain. A
**Page unavailable** message means the server knows the page but the bundle
being served does not contain it: rebuild the bundle and check the directory.

Run `cargo check` in the project root and `trunk build --release` in `ui/`
before deployment.

## Ship the bundle

With `UiSource::Dir`, deploy `ui/dist` next to the binary. The generated
`Dockerfile` does not build or copy a UI bundle; add those steps to it.

To keep a single binary, compile the bundle into it instead. Add the
`rust-embed` crate (`cargo add rust-embed`) and serve the embedded files:

```rust,ignore
use openshop::UiSource;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "ui/dist/"]
struct AdminUi;

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        // ... providers, flows, pages and other settings ...
        .admin_ui(UiSource::assets(|path| AdminUi::get(path).map(|file| file.data)))
        .build()
}
```

`ui/dist` must then exist when the server crate is compiled, so build the
bundle first.

## Restrict access when adding business data

This example is available to authenticated app users. For narrower access, add
a page rule with `.access(..)` and per-function rules with `.authorize(..)`:

```rust,ignore
use openshop::{AdminPage, define_admin_page_access, json};

const ALLOWED_STAFF: [&str; 1] = ["1234567890"];

pub fn page() -> AdminPage {
    let staff_only =
        define_admin_page_access(|ctx| async move { Ok(ALLOWED_STAFF.contains(&ctx.actor.id.as_str())) });

    AdminPage::new("/store")
        .access(staff_only)
        .loader("storeSummary", |ctx, _input| async move {
            Ok(json!({ "shop": ctx.shop, "shopifyApp": ctx.shopify_app }))
        })
}
```

`ctx.actor.id` is the Shopify staff user of the session token. The rules are
described in the
[authorization reference](../reference/custom-admin-pages.md#authorization-and-errors).
When querying app-owned tables, filter by the trusted shop and app identity;
`sqlx` does not add those filters automatically.
