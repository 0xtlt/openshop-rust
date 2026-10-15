# Add a custom admin page

Use this guide to add a **Store** page to an existing app. It displays the trusted
shop identity returned by a server loader. You need a running app installed on a
development store.

A custom page is one file: a loader, which the server runs, and a view, which
the browser shows. The view is compiled to WebAssembly into the admin UI
bundle of your application, built with [trunk](https://trunkrs.dev). Install
the toolchain once:

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
```

> **Caution: experimental API.**
> Custom admin pages are experimental. Check the
> [custom admin page reference](../reference/custom-admin-pages.md) when upgrading.

## 1. Prepare the application

From the project root:

```bash
openshop init-ui
```

In `Cargo.toml`, this enables the `ui` feature of `openshop` and adds
`leptos`, the library views are written with. It also writes an example page,
`src/admin/hello.rs`, declared in `src/admin/mod.rs`.

## 2. Write the page

Create `src/admin/store.rs`:

```rust,ignore
use leptos::prelude::*;
use openshop::prelude::*;
use openshop::ui::use_loader;
use serde::{Deserialize, Serialize};

// What the loader returns, and the view reads.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreSummary {
    pub shop: String,
}

pub struct StorePage;

#[admin_page("/store", nav = "Store")]
impl StorePage {
    #[loader]
    async fn store_summary(ctx: AdminContext) -> Result<StoreSummary> {
        Ok(StoreSummary { shop: ctx.shop })
    }

    #[view]
    fn view() -> impl IntoView {
        let summary = use_loader(Self::STORE_SUMMARY, || ());

        view! {
            <s-page heading="Store">
                {move || summary.loading.get().then(|| view! { <s-spinner accessibilityLabel="Loading store" /> })}
                {move || summary.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
                {move || summary.data.get().map(|summary| view! { <s-paragraph>{summary.shop}</s-paragraph> })}
            </s-page>
        }
    }
}
```

Declare the module in `src/admin/mod.rs`: `pub mod store;`.

`#[admin_page("/store")]` makes the `impl` block of `StorePage` a page, and
`nav = "Store"` gives it an entry in the menu of the admin. `#[loader]` makes
`store_summary` a loader of that page, and `#[view]` makes `view` what the
page shows.

The view calls the loader through `Self::STORE_SUMMARY`, a constant the block
gains for the loader: it carries the type the loader returns, so
`summary.data` is a `StoreSummary`. The browser calls the loader with a fresh
Shopify session token.

The loader receives the identity verified by OpenShop. Keep database access,
provider credentials, and other server-only work in loaders and actions: their
context also carries `ctx.db`, `ctx.shopify`, and `ctx.connectors`.

A loader or an action can take what the page sends as a second argument. Mark
the type of that argument with `#[input]`: the input is validated against it
before the function runs, and its fields are named in camel case outside Rust
(`display_name` is `displayName`). The
[custom admin page reference](../reference/custom-admin-pages.md#views)
shows an action with its input.

## 3. List the page

In `src/main.rs`, declare `admin` next to the other modules, and list the
page where the application is declared, keeping your flows and other settings:

```rust,ignore
mod admin;

use openshop::UiSource;
use openshop::prelude::*;

use admin::store::StorePage;

static APP: App = app! {
    // ... providers, flows and other settings ...
    admin_pages: [StorePage],
    admin_ui: UiSource::Dir("dist".into()),
};
```

Listing the page in `admin_pages` is enough to serve it, and its `nav` label
is its entry in the menu. `admin_ui` replaces the built-in bundle with yours;
the directory is resolved from the working directory of the process. The
`OPENSHOP_UI_DIR` environment variable overrides it.

To set the menu by hand, for another order or another label, add the
`admin_menu` setting:
`admin_menu: [openshop::AdminMenuItem::new("Store", StorePage::PATH)]`.
It replaces the entries the pages declare. `StorePage::PATH` is the path given
to `#[admin_page]`; the configuration is refused, and the app does not start,
when a path of that list does not match a listed page.

## 4. Open the page

Restart `openshop dev`. When a page has a view, it builds the admin UI bundle
of the application from the module of the pages, here `admin`, rebuilds it
when a page changes, and serves it. Open the app from Shopify admin and select
**Store**. You should see the current shop domain. From then on, the page
reloads by itself when you change it. A **Page unavailable**
message means the server knows the page but the bundle being served does not
contain it: check the `trunk` output in the terminal and the directory being
served.

`cargo run -- dev` does not build the bundle: use `openshop dev`, or run
`openshop build-ui` after a change.

`cargo check` reports the errors of the page, view included. The bundle is
compiled from the module of the pages on its own, so an error that only its
build reports names something it does not have: import what only a loader or
an action needs inside that function. See
[What the bundle is made of](../reference/custom-admin-pages.md#what-the-bundle-is-made-of).

## Ship the bundle

`openshop build` builds the bundle into `dist/`, optimized for size, then the
application. With `UiSource::Dir`, or `OPENSHOP_UI_DIR`, deploy `dist` next
to the binary.

The `Dockerfile` of a project made from the `custom` or `public` template
builds the bundle too: it installs the WebAssembly target, trunk and the
`openshop` CLI of the release the project depends on, runs `openshop build`,
copies `dist` into the image and names it with `OPENSHOP_UI_DIR`. The
`Dockerfile` of the `minimal` template does not; after `openshop init-ui`, take
those steps from the `Dockerfile.views` of `templates/shared` in the OpenShop
repository.

To keep a single binary, compile the bundle into it instead. Add the
`rust-embed` crate (`cargo add rust-embed`) and serve the embedded files:

```rust,ignore
use openshop::UiSource;
use openshop::prelude::*;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "dist/"]
struct AdminUi;

static APP: App = app! {
    // ... providers, flows, pages and other settings ...
    admin_ui: UiSource::assets(|path| AdminUi::get(path).map(|file| file.data)),
};
```

`dist` must then exist when the server is compiled, so build the bundle
first: `openshop build` does.

## Restrict access when adding business data

This example is available to authenticated app users. For narrower access, add
a page rule with `#[access]` and per-function rules with `authorize = ..` on
`#[loader]` and `#[action]`:

```rust,ignore
const ALLOWED_STAFF: [&str; 1] = ["1234567890"];

#[admin_page("/store", nav = "Store")]
impl StorePage {
    #[access]
    async fn staff_only(ctx: AdminContext) -> Result<bool> {
        Ok(ALLOWED_STAFF.contains(&ctx.actor.id.as_str()))
    }

    #[loader]
    async fn store_summary(ctx: AdminContext) -> Result<StoreSummary> {
        Ok(StoreSummary { shop: ctx.shop })
    }

    // ... the view of the page ...
}
```

`ctx.actor.id` is the Shopify staff user of the session token. The rules are
described in the
[authorization reference](../reference/custom-admin-pages.md#authorization-and-errors).
When querying app-owned tables, read a model through the rows of the shop of
the context, as in `Review::of(&ctx)`, and filter by the app identity where it
matters; a query written with `sqlx` has no such filter.

## Make the page the home of the admin

Declare the page at `/` to have the admin open on it, in place of the
built-in dashboard. It has no `nav`: the name of the app in the Shopify
sidebar opens it.

```rust,ignore
#[admin_page("/")]
impl StorePage {
    // ... the loaders, the actions and the view of the page ...
}
```

To leave merchants nothing but your pages, also disable the built-in ones
with `builtin_pages: AdminPagesConfig::none()`. See
[An admin made of the pages of the application](../reference/configuration.md#an-admin-made-of-the-pages-of-the-application).
