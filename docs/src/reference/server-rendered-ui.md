# Server-rendered admin (design)

> **Status: implemented.** The admin of the framework and of the
> applications is rendered by the server; the Leptos UI (`crates/openshop-ui`,
> built with trunk), its bundle and the `ui` feature are removed. This page is
> the record of the design and of the migration. How to write a page is in
> [Custom admin pages](custom-admin-pages.md); how to move an application is in
> [Migrate to the server-rendered admin](../guides/migrate-to-server-rendered-ui.md).

## Why

| | Leptos UI (before) | Server-rendered |
| --- | --- | --- |
| What the browser downloads for the app, `openshop dev` | 3.2 MB compressed (a 45.5 MB debug `.wasm`) | 5 to 7 KB: the page, 0.8 to 3.1 KB, and `openshop.js`, 4.1 KB, cached for a year |
| Same, release build | 451 KB compressed (a 1.2 MB `.wasm`) | the same 5 to 7 KB |
| Launch to rendered page, `openshop dev`, cold browser cache | 1.5 s for the first load of a build, 0.5 s after | 87 to 115 ms, lsf's admin included |
| Same, release bundle | 285 to 321 ms | the same, from a debug server |
| Navigation to another page | 82 ms | 54 to 57 ms (163 ms for the first) |
| An edit of a page to the page updated in `openshop dev` | 5.3 s (cargo and trunk) | 2.7 to 2.8 s (cargo) |
| Time the server takes for a page | — | 3 to 11 ms |
| Toolchain | `wasm32-unknown-unknown`, trunk, wasm-bindgen | cargo |
| Committed bundle | `ui-dist/`, 1.2 MB | none |
| Crates in `Cargo.lock` | 480, and 1906 lines of a second lockfile | 397 |

Measured in lsf's admin (`lsf app dev --polaris offline`) with Playwright
1.64.1 and Chromium, on projects made from the `minimal`, `custom` and `public`
templates. The Leptos numbers are those of the proof of concept, on the same
machine, with the compression cache of #57. Polaris and App Bridge (268 KB, the
same for both) are not counted.

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
fn view(home: Home, form: FormState) -> Markup {
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
- A parameter named `form` receives the form the page refused, its values and
  the error of each field (see below). A view without it keeps the page as it
  was when a form is refused, with an error toast.
- Markup is written with `maud::html!`. The application depends on `maud`
  (`html!` names the crate); `openshop init-ui` adds it. There is no wrapper
  macro.
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

`AdminPage::view(…)` is the same thing by hand, and `AdminPage::view_with_form(…)`
for a view that reads the form. A `#[view]` that returns anything else is
refused at compile time.

## Actions and the form protocol

An action is a `<form method="post">` of the page. Without an `action`
attribute it posts to the URL of the page, query string included, so the page
is rendered again with the same filters. The name of the action travels in
the field `_action`:

```rust,ignore
form method="post" {
    input type="hidden" name="_action" value=(Self::SET_HIDDEN);
    input type="hidden" name="hidden" value=(!hidden);
    s-button type="submit" variant="primary" { "Hide the banner" }
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
| `Outcome::invalid([("title", "…")])` | `422`, the page rendered with the form (see below) | the same |
| `Err(AdminPublicError)` | its status, `X-OpenShop-Toast-Error: <message>`, no body: the page stays | the page rendered with the message in a critical banner, with its status |
| any other error | `500`, `X-OpenShop-Toast-Error: Internal server error`, reported to Sentry | the same page, `500` |

Header values are percent-encoded. Each action writes one log line, with
`"form": true`, and its failure is reported to Sentry as `admin <route>
<action>`, with the `request_id` tag.

### Form errors

A form refused field by field is rendered again in place (`422`, or the
status of the error), with what was sent and the error of each field, in the
`form: FormState` of a view that declares it: when the input schema refuses
the form (each property it can tell: `Required`, `Must be at least 3
characters`, `Must be a whole number`), when the action answers
`Outcome::invalid(…)`, and when it fails with an `AdminPublicError` that has
`field_errors`. The view shows them with `error=[form.error("title")]` on a
Polaris field and `value=(form.value_or("title", saved))`; `form.of(ACTION)`
keeps the errors on the form that was sent. A modal whose form was refused is
rendered with `data-openshop-open`, which opens it again.

The toast of a plain post, without the script, is lost: there is no session
cookie the admin's frame keeps reliably, and nothing runs inside Shopify
admin without JavaScript, App Bridge first. A refused form is not lost: it is
the page, rendered with its errors.

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
  one: with several Shopify apps on one server, a page loaded without any
  token, such as a pasted link, bounces with the key of the first app. That is
  known, and left as it is;
- a request of the script gets `401` with
  `X-Shopify-Retry-Invalid-Session-Request: 1`, and App Bridge's `fetch` asks
  again with a fresh token;
- a plain post gets a `303` to its page, which bounces;
- anything else is the "Open this app from Shopify admin" page, `401`.

The launch URL keeps its parameters: a reload of the frame sends an expired
token, which bounces. The bounce costs one round trip and App Bridge's start:
84 ms in lsf for a page loaded without any token. A page is framed by the
admin of the shop of its session only: `frame-ancestors https://<shop>
https://admin.shopify.com` (any shop and `localhost` in development).

## `openshop.js`

231 lines, 4.1 KB with Brotli, served at `/__openshop/openshop.js?v=<hash>`
with `immutable`. Its whole job:

1. **Forms.** `submit` of a same-origin form is sent with `fetch` (App
   Bridge's, which adds the token), `X-OpenShop-Fetch: 1`, the body as
   `application/x-www-form-urlencoded`. A `GET` form puts its fields in the
   query and is a navigation. The submit `s-button` has `loading` while the
   request runs, and the admin's loading bar is on (`shopify.loading`). A form
   with `data-openshop-native` is left to the browser. Enter in a Polaris
   field submits its form, as in a native field (the field's input is in a
   shadow root, which the browser does not submit from). A `GET` form with
   `data-openshop-autosubmit` is sent when one of its fields changes.
2. **Answers.** `X-OpenShop-Toast` and `X-OpenShop-Toast-Error` go to
   `shopify.toast.show`. `X-OpenShop-Location` is fetched next. An HTML body,
   `ok` or not (a refused form, a page that is not there), replaces the
   content of `#openshop-page`, the title, and the `ui-nav-menu` when it
   changed. Anything else is a generic error toast.
3. **Navigation.** A click on a same-origin `<a>`, a `shopify:navigate` event
   (Polaris's `s-link`, `s-button href`; App Bridge clicks the `<a>` of the
   menu), and back and forward are fetched the same way, then `pushState`. The
   menu of the admin follows the URL.
4. **What only a browser knows**, when the page asks with an attribute:
   `<time datetime>` written in the language and the time zone of the browser;
   `data-openshop-open` on a modal, opened once the page is shown;
   `data-openshop-refresh="3000"`, the page fetched again after that delay
   while something runs, unless a field has the focus or was changed, or a
   modal is open (a running flow, the dashboard); `data-openshop-copy` and
   `data-openshop-copy-from`, a copy to the clipboard; `data-openshop-download`
   and `data-openshop-download-from`, a file saved (the export of the logs of
   a run, the output of a step).

Without it, every form posts and every link loads: the server answers with
redirects and full pages. Inside Shopify admin a full load costs a bounce, and
a toast needs the script. The navigation by the script stays: it saves a
bounce per click.

## The framework pages

Every page of the framework is server-rendered, in
`crates/openshop/src/server/pages/<page>.rs`: a function of the request
(`BuiltinPage`: the session, the path, the query, the refused form) that reads
what the JSON API reads (the listing, the save, the check of a provider, the
runs a list asks for, the logs of a run, the instances of a function, the
tokens of MCP…, each a function the handler of the API calls too) and returns
`Markup`, and a `submit` for its forms. The home, the providers, the flows and
their runs, the page of a run with its steps, its output and its logs, the
Shopify Functions and their instances, MCP and its tokens, and the crons keep
every feature of their Leptos page; a list, a filter, a selection, a tab is a
URL. What runs is followed by fetching the URL again
(`data-openshop-refresh`): every 3 s for a run in progress, 5 s for the runs of
a flow while one is active, 10 s for the dashboard. A secret made for the
merchant, the token of MCP, is given to the page by its action
(`BuiltinPage::reveal`) and rendered once, never put in a URL. A form posts to
`/mcp` as a form; MCP clients post JSON there.

The document around every page (`shell.rs`) has App Bridge first (the
endpoints of `ShopifyEndpoints`), Polaris, `openshop.js` with `defer`, the
`ui-nav-menu` of the built-in pages that are visible and of the pages of the
app that let the staff member in, and `#openshop-page`. In development the
reload script of `openshop dev` is added: the page reloads when the server
starts again.

## What was removed

| Before | After |
| --- | --- |
| `crates/openshop-ui` (Leptos, wasm32, its own workspace and lockfile) | removed |
| `crates/openshop/ui-dist/` (1.2 MB committed), `build-dist.sh`, `sources-hash.sh`, `ui-dist.sha256`, the `ui`, `ui-dist` and `templates-ui` CI jobs | removed |
| feature `embedded-ui` | removed; the admin needs no feature |
| feature `ui` and `openshop::ui` | removed at once: nothing was released, so no release with a feature that warns |
| `UiSource::{Embedded, Dir, Assets}` | removed: `UiSource::Server` is the default and the only admin, `None` stays (API only) |
| `openshop dev`: trunk watch, `OPENSHOP_UI_DIR`, the bundle crate under `target/openshop-ui/` | `cargo run -- dev` alone; the reload script stays |
| `openshop build-ui`, `init-ui` writing a Leptos page | `build-ui` removed; `init-ui` writes a `Markup` page and adds `maud` |
| trunk and the wasm target in `mise.toml`, CI, the `Dockerfile` of the templates (`# if views`) | removed |
| `/api/pages/custom/_rpc/…`, `use_loader`, `use_action`, `AdminFunctionRef` | removed; `ctx.call_loader` and `ctx.call_action` stay for tests, with `ctx.page` (the whole document) and `ctx.submit`, whose toasts `openshop::testing::toast` and `toast_error` read decoded |
| `/api/pages`, `/api/pages/custom`, `/api/crons`, … (the JSON the Leptos UI read) | kept: scripts, MCP and the downloads of the pages read them |

Loader outputs keep `Serialize`: `ctx.call_loader` reads them as JSON in
tests.

## Migrating an application

The `openshop` CLI is installed again first (`cargo install --locked --force
openshop-cli --version <version>`, or `--path crates/openshop-cli` from a
checkout), and `openshop --version` says
which one runs: the old one runs trunk. `openshop dev` lists what a project
keeps of the Leptos admin before it starts it.

`Cargo.toml`: `features = ["ui"]` and `leptos` go, `maud = "0.27"` comes. A
page changes in three places: the view takes the loader's output, an action
that toasts returns `Outcome`, and a button that ran an action is a form. See
[Migrate to the server-rendered admin](../guides/migrate-to-server-rendered-ui.md),
with an app's `home.rs` before and after. About 30 minutes per page.

## Order of the full migration

| Step | What | State |
| --- | --- | --- |
| 1 | The proof: `UiSource::Server`, `#[view] -> Markup`, forms, `Outcome`, the bounce, `openshop.js`, `/crons` | done (#58) |
| 2 | Form errors: `form: FormState`, `Outcome::invalid`, the `422` of the schema field by field, rendered in place | done |
| 3 | The framework pages: home, providers, flows, the page of a run, functions, MCP | done |
| 4 | `UiSource::Server` is the default | done; the Leptos UI did not stay one more release |
| 5 | Templates (`custom`, `public`), `init-ui`, the guides and the references | done |
| 6 | Remove `crates/openshop-ui`, `ui-dist/`, trunk, the `ui` and `embedded-ui` features, the RPC transport and `openshop::ui` | done |
| 7 | Applications, about 30 minutes per page | per app |

## Decisions

1. **Navigation by the script**: kept. Links and the menu are fetched and
   swapped.
2. **Form errors**: a `form` parameter of the view, filled when the schema
   refuses the form and when the action returns field errors
   (`Outcome::invalid`, or an `AdminPublicError` with `field_errors`); `error=`
   on the Polaris fields. A view without `form` works as before.
3. **The toast of a plain post** without the script is lost: accepted.
4. **Several Shopify apps on one server**: a page loaded without any token
   bounces with the first app's key. Known, not fixed.
5. **`maud` in the application**: the application depends on it; `init-ui`
   adds it. No wrapper macro.
