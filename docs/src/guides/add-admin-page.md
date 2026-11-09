# Add a custom admin page

Use this guide to add a **Store** page to an existing app. It displays the trusted
shop identity returned by a server loader, and saves a note with an action.
You need a running app installed on a development store.

A custom page is one file: a loader, which reads what the page shows, and a
view, a function of what the loader returns, which the server renders into
HTML made of Shopify's Polaris web components. There is nothing to build
besides your application.

> **Caution: experimental API.**
> Custom admin pages are experimental. Check the
> [custom admin page reference](../reference/custom-admin-pages.md) when upgrading.

## 1. Prepare the application

From the project root:

```bash
openshop init-ui
```

In `Cargo.toml`, this adds `maud`, the crate whose `html!` the views are
written with. It also writes an example page, `src/admin/hello.rs`, declared
in `src/admin/mod.rs`.

## 2. Write the page

Create `src/admin/store.rs`:

```rust,ignore
use maud::html;
use openshop::prelude::*;
use serde::{Deserialize, Serialize};

// What the loader returns, and the view reads.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreSummary {
    pub shop: String,
}

#[input]
pub struct SaveNote {
    #[schemars(length(min = 1, max = 200))]
    pub note: String,
}

pub struct StorePage;

#[admin_page("/store", nav = "Store")]
impl StorePage {
    #[loader]
    async fn store_summary(ctx: AdminContext) -> Result<StoreSummary> {
        Ok(StoreSummary { shop: ctx.shop })
    }

    #[action]
    async fn save_note(ctx: AdminContext, input: SaveNote) -> Result<Outcome> {
        ctx.logger.info_with("Note saved", json!({ "note": input.note }));
        Ok(Outcome::toast("Note saved"))
    }

    #[view]
    fn view(store_summary: StoreSummary, form: FormState) -> Markup {
        html! {
            s-page heading="Store" {
                s-section {
                    s-paragraph { (store_summary.shop) }
                }
                s-section heading="Note" {
                    form method="post" {
                        input type="hidden" name="_action" value=(Self::SAVE_NOTE);
                        s-stack gap="base" {
                            s-text-field name="note" label="Note"
                                value=(form.value_or("note", "")) error=[form.error("note")] {}
                            s-button type="submit" variant="primary" { "Save" }
                        }
                    }
                }
            }
        }
    }
}
```

Declare the module in `src/admin/mod.rs`: `pub mod store;`.

`#[admin_page("/store")]` makes the `impl` block of `StorePage` a page, and
`nav = "Store"` gives it an entry in the menu of the admin. `#[loader]` makes
`store_summary` a loader of that page, `#[action]` makes `save_note` an
action, and `#[view]` makes `view` what the page shows.

The view's parameter `store_summary` is named after the loader: the server
runs the loader and gives the view what it returns, a `StoreSummary`. The
form posts to the page, the name of the action in `_action`
(`Self::SAVE_NOTE`, a constant the block gains for each function); the action
answers `Outcome::toast(…)`, and the page is rendered again with the toast. A
note the input schema refuses (empty, or too long) renders the page again
with the error under the field: that is what the `form` parameter carries.

The loader and the action receive the identity verified by OpenShop, from the
Shopify session token of the request. Keep database access, provider
credentials, and other server-only work in loaders and actions: their context
also carries `ctx.db`, `ctx.shopify`, and `ctx.connectors`.

## 3. List the page

In `src/main.rs`, declare `admin` next to the other modules, and list the
page where the application is declared, keeping your flows and other settings:

```rust,ignore
mod admin;

use openshop::prelude::*;

use admin::store::StorePage;

static APP: App = app! {
    // ... providers, flows and other settings ...
    admin_pages: [StorePage],
};
```

Listing the page in `admin_pages` is enough to serve it, and its `nav` label
is its entry in the menu.

To set the menu by hand, for another order or another label, add the
`admin_menu` setting:
`admin_menu: [openshop::AdminMenuItem::new("Store", StorePage::PATH)]`.
It replaces the entries the pages declare. `StorePage::PATH` is the path given
to `#[admin_page]`; the configuration is refused, and the app does not start,
when a path of that list does not match a listed page.

## 4. Open the page

`openshop dev` restarts the application when a file changes, and the page
reloads. Open the app from Shopify admin and select **Store**. You should see
the current shop domain. Save a note: a toast says so. Save an empty one: the
field says it is required.

`cargo check` reports the errors of the page, view included: it is compiled
with the server, as the rest of the application.

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

A page whose rule says no shows "This page is disabled." and has no entry in
the menu. `ctx.actor.id` is the Shopify staff user of the session token. The rules are
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
