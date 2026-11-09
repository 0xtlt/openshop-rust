# Server-rendered admin (design)

> **Status: proof of concept, behind `UiSource::Server`.** This page is the
> design of the migration from the WebAssembly admin UI (`crates/openshop-ui`,
> Leptos built with trunk) to pages the server renders. Nothing else changes
> until it is accepted: the Leptos UI stays the default.

## Why

| | Leptos UI (today) | Server-rendered |
| --- | --- | --- |
| What the browser downloads for the app, `openshop dev` | 3.2 MB compressed (a 45.5 MB debug `.wasm`) | 3 KB (the page, 1 KB, and `openshop.js`, 2 KB) |
| Same, release build | 451 KB compressed (a 1.2 MB `.wasm`) | 3 KB |
| Launch to rendered page, `openshop dev` (cold cache) | 4.4 to 6.8 s | 70 ms |
| Same, release bundle | 285 to 321 ms | 70 ms (debug server) |
| An edit of a page to the page updated in `openshop dev` | 13.1 s (cargo, then trunk, the frame blank meanwhile) | 3.6 s (cargo) |
| Toolchain | `wasm32-unknown-unknown`, trunk, wasm-bindgen | cargo |

Measured in lsf's admin (`lsf serve --polaris offline`) with Playwright 1.64.1
and Chromium, on the same app twice: the `minimal` template, the home page of
shop-test, the `/reviews` page of the `custom` template and the `/hello` page
of `openshop init-ui`. Polaris and App Bridge (268 KB, the same for both) are
not counted. The numbers are in the pull request that adds this page.

## The model

The admin is made the way MyMCPs makes its pages:

1. **The server does the work; the script only presents it.** A page is HTML
   that the server renders from the data of its loaders, made of Shopify's
   Polaris web components (`<s-page>`, `<s-banner>`, `<s-button>`, …), which
   are plain custom elements loaded from Shopify's CDN, as today.
2. **Every list, filter and page is a URL the server renders.** The input of a
   loader is read from the query string of the page.
3. **A form sent in the background also answers a plain post.** A plain post
   is answered with a redirect: the URL is the state.

There is no JavaScript build and no component library: one hand-written
script, `openshop.js`, embedded in the binary.

## Views

`#[view]` in an `#[admin_page]` block becomes a function of the server: it
takes what the loaders return and gives back `maud::Markup`. No signal, no
`use_loader`, no `spawn_local`.

```rust,ignore
#[view]
fn view(home: Home) -> Markup {
    html! { s-page { … } }
}
```

- **Each parameter is a loader of the page, by the name of its function**, and
  receives what it returns: `fn view(list_reviews: ReviewList, stats: Stats)`.
  A parameter that names no loader is refused at compile time, with the rule
  in the message. The types are checked by rustc as for any call.
- A loader that takes an input reads it from the query string of the page:
  `?status=active&page=2` for `#[input] struct Filter { status: String, page: u32 }`,
  each field converted to the type its JSON Schema gives it, then validated by
  that schema (`400 INVALID_INPUT` otherwise).
- The access rule of the page and of its parents runs first; a loader's own
  `authorize` rule runs before it. A refusal renders "This page is disabled."
- Markup is written with `maud::html!`. The application depends on `maud`
  (`html!` names the crate), as it depends on `leptos` today.
- The view, like the loaders and the actions, is compiled with the server only.

### What the macro does

`#[admin_page]` tells the two kinds of view apart by what the function
returns: `-> Markup` is rendered by the server, anything else stays a Leptos
view. For a server view it writes, on the server only:

```rust,ignore
// Registered with the page:
.view(|request: openshop::admin::PageRequest| async move {
    let home = request.load(Self::home).await?;          // a loader without input
    request.authorize(Self::staff).await?;               // `#[loader(authorize = …)]`
    let filter = request.load_with(Self::filter).await?; // input from the query string
    Ok(Self::view(home, filter))
})

// The name of every function, for the forms of the view:
impl HomePage {
    pub const SET_HIDDEN: &'static str = "setHidden";
}
```

`AdminPage::view(…)` is the same thing by hand. `openshop dev` and `openshop
build` build no bundle for a view that returns `Markup`.

## Actions and the form protocol

An action is a `<form method="post">` of the page. Without an `action`
attribute it posts to the URL of the page, query string included, so the page
is rendered again with the same filters. The name of the action travels in
the field `_action`:

```rust,ignore
form method="post" {
    input type="hidden" name="_action" value=(Self::SET_HIDDEN);
    input type="hidden" name="hidden" value=(!hidden);
    s-button type="submit" variant="primary" { "Hide PayPal Express" }
}
```

The fields are converted to the JSON the input schema of the action
describes, then go through what the RPC transport runs today: the access
rules, the action's `authorize`, the schema (`422`), the action. A boolean
property without a field is `false` (an unchecked checkbox sends nothing); a
property that is an array collects every field of its name; fields that start
with `_` are the page's.

An action returns any value as before, or an `Outcome`, which says what the
page does next:

| `Outcome` | Script request | Plain post |
| --- | --- | --- |
| `Outcome::toast("Saved")` | `200`, the page rendered again, `X-OpenShop-Toast: Saved` | `303` to the page |
| `Outcome::error("…")` | the same, with `X-OpenShop-Toast-Error` | `303` to the page |
| `Outcome::redirect("/reviews/42")` (`.with_toast(…)`) | `204`, `X-OpenShop-Location: /reviews/42` (and the toast) | `303` to `/reviews/42` |
| any other value | `200`, the page rendered again | `303` to the page |
| `Err(AdminPublicError)` | its status, `X-OpenShop-Toast-Error: <message>`, no body: the page stays | the page rendered with the message in a critical banner, with its status |
| any other error | `500`, `X-OpenShop-Toast-Error: Internal server error`, reported to Sentry | the same page, `500` |

Header values are percent-encoded. Each action writes the log line of an RPC
call, with `"form": true`.

## Authentication of document requests

A request for a page is authenticated with a Shopify session token:

1. the `Authorization: Bearer` header, which App Bridge's `fetch` adds to the
   requests of the script;
2. else the `id_token` of the URL, which Shopify launches the app with.

A launch Shopify signed (`hmac`) first checks that the shop is installed, as
the Leptos shell does: `is_installed_for_launch`, then `/auth` when it is not.
A valid token is checked as `/api` checks it (one function, `session_of`,
now serves both), the shop given a token by the exchange when it has none.

Without a valid token, Shopify's own pattern for server-rendered embedded apps
(the session token bounce of `@shopify/shopify-app-remix`):

- a document request inside the admin (`shop` or `embedded` in the query, or
  `Sec-Fetch-Dest: iframe`) is redirected to
  `/auth/session-token?<its query>&shopify-reload=<its URL>`, without the
  expired `id_token`. That page is App Bridge alone: App Bridge sees
  `shopify-reload` and loads that URL again with a fresh `id_token`. The app
  whose key the page carries is the `aud` of the expired token, else the first
  one;
- a request of the script gets `401` with
  `X-Shopify-Retry-Invalid-Session-Request: 1`, and App Bridge's `fetch` asks
  again with a fresh token;
- a plain post gets a `303` to its page, which bounces;
- anything else is the "Open this app from Shopify admin" page, `401`.

The launch URL keeps its parameters: a reload of the frame sends an expired
token, which bounces. The bounce costs one round trip and App Bridge's start:
43 ms in lsf for a page loaded without any token.

## `openshop.js`

126 lines, 106 of code, 2.3 KB gzipped, served at
`/__openshop/openshop.js?v=<hash>` with `immutable`. Its whole job:

1. **Forms.** `submit` of a same-origin form is sent with `fetch` (App
   Bridge's, which adds the token), `X-OpenShop-Fetch: 1`, the body as
   `application/x-www-form-urlencoded`. A `GET` form puts its fields in the
   query and is a navigation. The submit `s-button` has `loading` while the
   request runs, and the admin's loading bar is on (`shopify.loading`). A form
   with `data-openshop-native` is left to the browser.
2. **Answers.** `X-OpenShop-Toast` and `X-OpenShop-Toast-Error` go to
   `shopify.toast.show`. `X-OpenShop-Location` is fetched next. An HTML body
   replaces the content of `#openshop-page`, the title, and the `ui-nav-menu`
   when it changed. Anything else is a generic error toast.
3. **Navigation.** A click on a same-origin `<a>`, a `shopify:navigate` event
   (Polaris's `s-link`, `s-button href`; App Bridge clicks the `<a>` of the
   menu), and back and forward are fetched the same way, then `pushState`. The
   menu of the admin follows the URL.

Without it, every form posts and every link loads: the server answers with
redirects and full pages. Inside Shopify admin a full load costs a bounce, and
a toast needs the script.

## The framework pages

Built-in pages are server-rendered the same way, in
`crates/openshop/src/server/pages/<page>.rs`: a function of the session that
reads what the JSON API reads today (`cron_items`, …) and returns `Markup`,
and a `submit` for its forms. This proof ports `/crons`: the table, the input
of a cron in an `s-modal` opened by `commandFor`, and the form of each row,
which says "Cron enabled" or "Cron disabled" in a toast.

The document around every page (`shell.rs`) has App Bridge first (the
endpoints of `ShopifyEndpoints`, as today), Polaris, `openshop.js` with
`defer`, the `ui-nav-menu` of the built-in pages that are visible and of the
pages of the app that let the staff member in, and `#openshop-page`. In
development the reload script of `openshop dev` is added: the page reloads
when the server starts again.

## Features, `ui-dist/`, trunk

| Today | After the migration |
| --- | --- |
| `crates/openshop-ui` (Leptos, wasm32, its own workspace and lockfile) | removed |
| `crates/openshop/ui-dist/` (1.2 MB committed), `build-dist.sh`, `sources-hash.sh`, `ui-dist.sha256`, the `ui-dist` CI job | removed |
| feature `embedded-ui` (embeds `ui-dist/`) | removed; the server-rendered admin needs no feature (`maud` is a small dependency) |
| feature `ui` (re-exports `openshop-ui` as `openshop::ui`) | removed after one release where it is a no-op that warns |
| `UiSource::{Embedded, Dir, Assets}` | `UiSource::Server` becomes the default and the only admin; `Dir`/`Assets` go, `None` stays (API only) |
| `openshop dev`: trunk watch, `OPENSHOP_UI_DIR`, the bundle crate under `target/openshop-ui/` | `cargo run -- dev` alone; the reload script stays |
| `openshop build-ui`, `init-ui` writing a Leptos page | `build-ui` removed; `init-ui` writes a `Markup` page and adds `maud` |
| trunk in `mise.toml`, CI, the `Dockerfile` of the templates (`# if views`) | removed |
| `/api/pages/custom/_rpc/…`, `use_loader`, `use_action`, `AdminFunctionRef` | removed with the Leptos views; `ctx.call_loader` and `ctx.call_action` stay for tests |
| `/api/pages`, `/api/pages/custom`, `/api/crons`, … (the JSON the Leptos UI reads) | kept while something else reads them (MCP has its own functions), then removed page by page |

## Migrating an application

`Cargo.toml`: `features = ["ui"]` and `leptos` go, `maud = "0.27"` comes.
A page changes in three places: the view takes the loader's output, an action
that toasts returns `Outcome`, and a button that ran an action is a form.
shop-test's `src/admin/home.rs`, before:

```rust,ignore
use leptos::prelude::*;
use leptos::task::spawn_local;
use openshop::prelude::*;
use openshop::ui::{AdminActionOptions, shopify, use_action, use_loader};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Home {
    NoPlan { pricing_plans: String },
    Plan { hidden: bool },
}

#[input]
pub struct SetHidden {
    pub hidden: bool,
}

pub struct HomePage;

#[admin_page("/")]
impl HomePage {
    #[loader]
    async fn home(ctx: AdminContext) -> Result<Home> { /* unchanged */ }

    #[action(authorize = openshop::billing::any_plan)]
    async fn set_hidden(ctx: AdminContext, input: SetHidden) -> Result<Home> {
        // … unchanged …
        Ok(Home::Plan { hidden: input.hidden })
    }

    #[view]
    fn view() -> impl IntoView {
        let home = use_loader(Self::HOME, || ());
        let set_hidden = use_action(Self::SET_HIDDEN);
        let toggle = move |hidden: bool| {
            spawn_local(async move {
                let options = AdminActionOptions::new().revalidate(Self::HOME);
                match set_hidden.invoke(SetHidden { hidden }, options).await {
                    Ok(Some(_)) => {
                        shopify::toast(if hidden { "Paypal Express hidden" } else { "Paypal Express shown" });
                    }
                    Err(error) => {
                        shopify::toast_error(&error.message);
                    }
                    Ok(None) => {}
                }
            });
        };

        view! {
            <s-page>
                {move || home.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
                {move || home.loading.get().then(|| view! { <s-spinner accessibilityLabel="Loading" /> })}
                {move || {
                    home.data.get().map(|data| match data {
                        Home::NoPlan { pricing_plans } => view! {
                            <s-banner tone="warning" heading="You don't have a subscription plan yet.">
                                <s-button variant="primary" href=pricing_plans target="_top">"Take a plan"</s-button>
                            </s-banner>
                        }
                        .into_any(),
                        Home::Plan { hidden } => view! {
                            <s-banner
                                tone=if hidden { "success" } else { "warning" }
                                heading=if hidden {
                                    "Paypal Express is successfully hidden from checkout page"
                                } else {
                                    "Paypal Express is not hidden from checkout page"
                                }
                            >
                                <s-button
                                    variant="primary"
                                    tone=if hidden { "critical" } else { "auto" }
                                    loading=move || set_hidden.pending.get()
                                    on:click=move |_| toggle(!hidden)
                                >
                                    {if hidden { "Show Paypal Express" } else { "Hide Paypal Express" }}
                                </s-button>
                            </s-banner>
                        }
                        .into_any(),
                    })
                }}
            </s-page>
        }
    }
}
```

After:

```rust,ignore
use maud::html;
use openshop::prelude::*;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Home {
    NoPlan { pricing_plans: String },
    Plan { hidden: bool },
}

#[input]
pub struct SetHidden {
    pub hidden: bool,
}

pub struct HomePage;

#[admin_page("/")]
impl HomePage {
    #[loader]
    async fn home(ctx: AdminContext) -> Result<Home> { /* unchanged */ }

    #[action(authorize = openshop::billing::any_plan)]
    async fn set_hidden(ctx: AdminContext, input: SetHidden) -> Result<Outcome> {
        // … unchanged …
        Ok(Outcome::toast(if input.hidden { "Paypal Express hidden" } else { "Paypal Express shown" }))
    }

    #[view]
    fn view(home: Home) -> Markup {
        html! {
            s-page {
                @match home {
                    Home::NoPlan { pricing_plans } => {
                        s-banner tone="warning" heading="You don't have a subscription plan yet." {
                            s-button variant="primary" href=(pricing_plans) target="_top" { "Take a plan" }
                        }
                    }
                    Home::Plan { hidden } => {
                        s-banner
                            tone=(if hidden { "success" } else { "warning" })
                            heading=(if hidden {
                                "Paypal Express is successfully hidden from checkout page"
                            } else {
                                "Paypal Express is not hidden from checkout page"
                            })
                        {
                            form method="post" {
                                input type="hidden" name="_action" value=(Self::SET_HIDDEN);
                                input type="hidden" name="hidden" value=(!hidden);
                                s-button type="submit" variant="primary" tone=(if hidden { "critical" } else { "auto" }) {
                                    (if hidden { "Show Paypal Express" } else { "Hide Paypal Express" })
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
```

What goes away: the loading and error states (the server renders once it has
the data; a loader error is the page's error), `revalidate` (the page is
rendered again after the action), `pending` (the script sets `loading`), and
`Deserialize` on `Home`. `Serialize` stays while the loaders also answer the
RPC transport of the Leptos views. The loader and the action do not change.

## Order of the full migration

Estimates are for one person who knows the code, with the tests of each step.

| Step | What | Estimate |
| --- | --- | --- |
| 1 | This proof, after review: `UiSource::Server`, `#[view] -> Markup`, forms, `Outcome`, the bounce, `openshop.js`, `/crons` | done, plus 0.5 day of review fixes |
| 2 | Form errors: the view receives the action's field errors and the submitted values (`error=` on `s-text-field`), `422` rendered in place | 1 day |
| 3 | Framework pages, simplest first: home (0.5 d), providers with their config forms and the check button (0.5 d), flows and the run modal (1 d), functions (1 d), flow run with its logs, polling as a refresh of the URL (1.5 d), MCP (1.5 d) | 6 days |
| 4 | Switch the default to `UiSource::Server`; the Leptos UI stays reachable with `admin_ui: UiSource::Embedded` for one release | 0.5 day |
| 5 | Templates (`custom`, `public`), `init-ui`, the guides and the references (`custom-admin-pages.md`, `add-admin-page.md`, `cli.md`, `project-structure.md`), `tests/it/admin_views.rs` | 1 day |
| 6 | Remove `crates/openshop-ui`, `ui-dist/`, trunk in the CLI, CI, `mise.toml` and the `Dockerfile`, the `ui` and `embedded-ui` features, the RPC transport and `openshop::ui`; drop `Serialize` from loader outputs | 1 day |
| 7 | Applications: shop-test and the others, about 30 minutes per page | per app |

About 10 days in all, then the applications.

## Open questions

1. **Navigation by the script.** Links and the menu are fetched and swapped,
   which avoids a bounce per click (47 ms instead of a full load and a
   bounce). The alternative is full loads only, a smaller script, and a bounce
   for every page. Keep it?
2. **Form errors.** The view needs the field errors and the submitted values
   of an action that failed. A second kind of parameter
   (`form: &FormState`), or an `Outcome` that carries them?
3. **The toast of a plain post** is lost: a session cookie is not reliable in
   the admin's iframe. Acceptable, given that nothing runs inside Shopify
   admin without JavaScript (App Bridge is a script)?
4. **Several Shopify apps on one server.** A page loaded without any token
   bounces with the key of the first app. The `aud` of an expired token picks
   the right one, which covers reloads; a pasted link does not have one.
5. **`maud` in the application.** `html!` names the `maud` crate, so the
   application depends on it. A wrapper macro of `openshop` would hide it at
   the cost of maintaining one.
