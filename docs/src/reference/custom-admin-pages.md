# Custom admin pages

To build a screen, see [Add a custom admin page](../guides/add-admin-page.md).

Custom admin pages let an OpenShop app add embedded screens next to the pages
of the framework. The API is experimental.

A custom page is one `impl` block, marked `#[admin_page("/path")]`. It holds
the access rule, the loaders and the actions of the page, and its view: a
function of what the loaders return, which the server renders into HTML made
of Shopify's Polaris web components. There is no bundle to build and no
WebAssembly: the admin is pages the server renders, and one small script.

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
    fn view(summary: Summary) -> Markup {
        html! { s-page heading="Welcome" { (summary.shop) } }
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
    async fn save_review(ctx: AdminContext, input: SaveReview) -> Result<Outcome> {
        if input.title.len() > 120 {
            return Ok(Outcome::invalid([("title", "At most 120 characters")]));
        }
        ctx.logger.info_with("Review saved", json!({ "title": input.title }));
        Ok(Outcome::toast("Review saved"))
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
| `#[loader] async fn(ctx: AdminContext) -> Result<T>` | None. |
| `#[loader] async fn(ctx: AdminContext, input: I) -> Result<T>` | The query string of the page URL (`?status=active&page=2`), each parameter converted to the type the JSON Schema of `I` gives it, then validated against that schema and deserialized into `I`. |
| `#[action] async fn(ctx: AdminContext) -> Result<T>` | None. The fields of the form are ignored. |
| `#[action] async fn(ctx: AdminContext, input: I) -> Result<T>` | The fields of the form, converted and validated the same way. |

`I` is a type marked `#[input]`. The attribute derives `Deserialize`,
`Serialize`, and `JsonSchema` for it, without `serde` or `schemars` among the
dependencies of the application, and names its fields in camel case outside
Rust: `product_id` is `productId` in what the page sends. A
`#[serde(rename_all = "…")]` on the type states another rule. The attributes
of `serde` and `schemars` work on the type and on its fields; put `#[input]`
above the other attributes of the type. `T` is any `Serialize` value; an
action returns an [`Outcome`](#forms-and-actions) to say what the page does
next.
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
| `logger` | `ContextLogger` | Logs to the runtime logger with the shop, the app and the request ID; see [Logging](logging.md#handler-logger). |
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

## Views

The view of a page is the function of its block marked `#[view]`, in the same
file as its loaders and its actions. The server renders it: it runs the access
rules, then the loaders the view names, then the view, a function of what they
return, written with [maud](https://maud.lambda.xyz)'s `html!` and Shopify's
Polaris web components (`s-page`, `s-section`, `s-table`, `s-text-field`…):

```rust,ignore
// src/admin/reviews.rs
use maud::html;
use openshop::prelude::*;
use serde::{Deserialize, Serialize};

#[input]
pub struct SaveReview {
    #[schemars(length(min = 1, max = 120))]
    pub title: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub title: String,
}

#[input]
pub struct Filter {
    pub search: Option<String>,
}

pub struct ReviewsPage;

#[admin_page("/reviews", nav = "Reviews")]
impl ReviewsPage {
    #[loader]
    async fn list_reviews(ctx: AdminContext, input: Filter) -> Result<Vec<Review>> {
        let title = format!("First review of {}", ctx.shop);
        let matches = input.search.is_none_or(|search| title.contains(&search));
        Ok(matches.then(|| Review { id: "1".to_string(), title }).into_iter().collect())
    }

    #[action]
    async fn save_review(_ctx: AdminContext, input: SaveReview) -> Result<Outcome> {
        Ok(Outcome::redirect("/reviews/2").with_toast(format!("Saved {}", input.title)))
    }

    #[view]
    fn view(list_reviews: Vec<Review>, form: FormState) -> Markup {
        html! {
            s-page heading="Reviews" {
                s-section {
                    form method="get" {
                        s-search-field name="search" label="Search" {}
                    }
                    @for review in &list_reviews {
                        s-link href=(format!("/reviews/{}", review.id)) { (review.title) }
                    }
                }
                s-section heading="New review" {
                    form method="post" {
                        input type="hidden" name="_action" value=(Self::SAVE_REVIEW);
                        s-text-field name="title" label="Title"
                            value=(form.value_or("title", "")) error=[form.error("title")] {}
                        s-button type="submit" variant="primary" { "Add review" }
                    }
                }
            }
        }
    }
}

pub struct ReviewDetailPage;

#[admin_page("/reviews/:id")]
impl ReviewDetailPage {
    #[loader]
    async fn review(ctx: AdminContext) -> Result<Review> {
        // The dynamic segments of the path are in `ctx.params`.
        let id = ctx.params.get("id").cloned().unwrap_or_default();
        let title = format!("Review {id}");
        Ok(Review { id, title })
    }

    #[view]
    fn view(review: Review) -> Markup {
        html! { s-page heading=(review.title) {} }
    }
}
```

A page has at most one view: `fn(…) -> Markup`, not `async`. Each parameter
is named after a loader of the page and receives what it returns:
`fn view(list_reviews: Vec<Review>, stats: Stats)`, by value or by reference.
A parameter that names no loader is refused at compile time. A loader with an
input reads it from the query string of the page; a value the schema refuses
answers `400 INVALID_INPUT`. A loader with its own `authorize` rule runs after
it. The one other parameter a view may take is `form` (see
[Form errors](#form-errors)).

A loader that fails fails the page: an `AdminPublicError` is shown in a
critical banner with its status, any other error is logged and the page shows
"This page could not be displayed." (`500`). A page whose access rule says no
shows "This page is disabled." (`404`). There is no loading state: the server
renders the page once it has the data.

The view, like the loaders and the actions, is compiled with the server, and
its HTML is escaped: `(review.title)` writes text, never markup. Polaris
components are plain custom elements; their props are attributes
(`listSlot="primary"`, `tone=(tone)`). Components the view is made of are
ordinary functions that return `Markup`. Markup is written with
`maud::html!`, which names the `maud` crate: the application depends on it
(see [Set up the application](#set-up-the-application)).

### Forms and actions

An action is a `<form method="post">` of the page. Without an `action`
attribute it posts to the URL of the page, query string included, so the page
is rendered again with the same filters. The name of the action travels in the
field `_action`, written with the constant the block gains for each function,
named after it: `save_review` is `ReviewsPage::SAVE_REVIEW`. Polaris fields
(`s-text-field`, `s-number-field`, `s-select`, `s-checkbox`…) take part in the
form with their `name`.

The fields are converted to the JSON the input schema of the action describes,
then go through the access rules, the action's `authorize`, the schema and the
action. A boolean property without a field is `false` (an unchecked checkbox
sends nothing); a property that is an array collects every field of its name;
an empty field is `null` for an `Option`, and missing for a number; fields
that start with `_` are the page's.

An action returns any value, or an `Outcome`, which says what the page does
next:

| `Outcome` | What the merchant sees |
| --- | --- |
| `Outcome::toast("Saved")` | The page rendered again, and a toast. |
| `Outcome::error("…")` | The page rendered again, and an error toast. |
| `Outcome::redirect("/reviews/42")`, `.with_toast(…)` | That page of the admin, and the toast. |
| `Outcome::invalid([("title", "Already taken")])` | The form refused: see [Form errors](#form-errors). |
| any other value | The page rendered again. |
| `Err(AdminPublicError)` | The page as it was, and an error toast with its message. |
| any other error | The page as it was, an error toast "Internal server error"; the error is logged and reported to Sentry. |

A `<form method="get">` is a navigation: its fields become the query string
of the URL, which the loaders read. Every list, filter and page is a URL. A
link to another page of the admin is a plain path: `a href="/reviews/42"`,
`s-link href=…`, `s-button href=…`.

### Form errors

A view that declares a parameter `form: FormState` (or `&FormState`) shows a
refused form itself. The form is refused field by field when the input schema
refuses it (`Required`, `Must be at least 3 characters`, `Must be a whole
number`), when the action answers `Outcome::invalid(…)`, and when it fails
with an `AdminPublicError` that has `field_errors`. The page is then rendered
again in place, with the status `422` (or the error's), and `form` holds:

| `FormState` | Contract |
| --- | --- |
| `form.value_or("title", saved)` | The value sent for the field after a refusal, else `saved`: the `value` of a field. |
| `form.error("title")` | The error of the field: `error=[form.error("title")]` gives the Polaris field its `error` attribute only when there is one. |
| `form.of(Self::SAVE_REVIEW)` | The state of that action's form, empty when another form of the page was refused. |
| `form.values("tags")` | Every value sent for a field. |
| `form.message` | What the action said of the form as a whole: the message of its `AdminPublicError`. |
| `form.is_refused()` | Whether the page is rendered again for a refused form. |

A view without `form` keeps the page as it was, with an error toast that names
the first field.

### What the browser does

The document around every page has App Bridge, Polaris, the menu of the admin
(the built-in pages, then the pages of the application whose access rule lets
the staff member in), and `openshop.js`, a script of 2 KB that only presents
what the server rendered:

- a form is sent in the background with App Bridge's `fetch`, which adds the
  session token; its submit button shows `loading`, and the admin's loading bar
  runs. A form with `data-openshop-native` is left to the browser;
- a link to a page of the app, the menu, and back and forward fetch the page
  and replace its content, without loading the document again;
- the toasts the action asked for are shown with `shopify.toast`.

It also does what only a browser can, when the page asks with an attribute:

| Attribute | Effect |
| --- | --- |
| `<time datetime="…">` | The date written in the language and the time zone of the browser. |
| `data-openshop-refresh="5000"` | The page is fetched again after that many milliseconds, unless a field has the focus or a modal is open: what runs is followed without a reload. |
| `data-openshop-open` on an `s-modal` | The modal opens once the page is shown: a modal whose form was refused. |
| `data-openshop-copy="text"` on a button | Copies the text, with a toast. |
| `data-openshop-download="/api/…"` and `data-openshop-filename="name.csv"` | Fetches the URL with the session token and saves it as a file. |

Without the script, a form posts and the server answers with a redirect to the
page, and a link loads the page. Inside Shopify admin, where App Bridge
already needs JavaScript, that costs a reload per click, and the toast of a
plain post is lost.

### Set up the application

Views need `maud` in `Cargo.toml`, for `html!`:

```toml
[dependencies]
openshop = { version = "<version>" }
maud = "0.27"
serde = { version = "1", features = ["derive"] }
```

`openshop init-ui` adds it and writes an example page; see
[CLI commands](cli.md#scaffold-a-custom-admin-page). There is nothing to
build besides the application: `openshop dev` restarts it when a page changes,
and the page reloads.

## How pages are served

Every path that is not an API route, a webhook, a proxy or a route of the
application is a page of the admin. A request for a page is authenticated with
a Shopify session token: the `Authorization` header that App Bridge's `fetch`
adds to the requests of the script, else the `id_token` of the URL Shopify
opens the app with. A launch Shopify signed first checks that the shop is
installed, and sends it to `/auth` when it is not.

A page loaded without a valid token, inside the admin, goes through
`/auth/session-token`, the bounce page of Shopify's libraries: App Bridge gets
a fresh token there and loads the page again with it. A request of the script
gets `401` with `X-Shopify-Retry-Invalid-Session-Request`, and App Bridge asks
again; outside Shopify admin the page says to open the app from there. With
several Shopify apps on one server, a page loaded without any token bounces
with the key of the first app; the `aud` of an expired token picks the right
one, which covers the reload of a page.

Pages are sent with `Cache-Control: no-store`,
`X-Content-Type-Options: nosniff`, and a `Content-Security-Policy` whose
`frame-ancestors` allows Shopify admin only (and `localhost` in development).
The title of the document is the name of the app: the `name` of the
`shopify.app*.toml` of the app (the one whose `client_id` is its API key), else
the key of the app in `shopify.apps`, else `OpenShop`. App Bridge titles a page
with it when its `s-page` has no `heading`.

`openshop.js` is served at `/__openshop/openshop.js?v=<hash>`, cached for a
year under the URL of its version.

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
`.field_errors(…)` change the status and attach per-field messages (shown
under the fields of [a view that reads the form](#form-errors)), and
`.detail(name, value)` adds a text the error carries. Any other error is logged
with a request ID and shown as "Internal server error". An access rule or a
rule of the function may return one too: the request is answered with it.
`billing::require_plan` and `billing::any_plan` answer `402 PLAN_REQUIRED` this
way, with the pricing page of the app in the details; see
[Billing](billing.md#guard-an-admin-page).

| Status | `code` | Condition |
| --- | --- | --- |
| `200` | — | The page, rendered again after an action. |
| `402` | `PLAN_REQUIRED` | `billing::require_plan` or `billing::any_plan` found no active plan; `details` has `pricingUrl` and `pricingLink`. |
| `403` | `FORBIDDEN` | A rule of the function rejected the request. |
| `404` | `NOT_FOUND` | The page or the action does not exist, or the page's access rule said no. |
| `422` | `INVALID_INPUT` | The form does not satisfy the action's schema. |
| Your status | Your code | The handler returned an `AdminPublicError`. |
| `500` | `INTERNAL_ERROR` | Any other handler error, a panic, or an access rule that failed. |

Each action writes one structured line to the runtime logger with the request
ID, shop, app handle, actor ID, page ID, function name, kind, duration, and
`"form": true`, but not the fields of the form or what the action returns.

## Testing

`ctx.call_loader(path, name, input)` and `ctx.call_action(path, name, input)`
call a function of a page without HTTP: the access rules, the `authorize`
rule, the input schema and the errors above, typed output (an action that
returns an `Outcome` gives back an `Outcome`; `call_loader_as` and
`call_action_as` for another shop or staff user). `ctx.page(path)` asks for
the page as the admin does and gives the HTML of the whole document, the page
in `<main id="openshop-page">`, and `ctx.submit(path, &[("_action",
Page::SAVE), …])` sends a form of it: the page rendered again, its toast in
`x-openshop-toast` or `x-openshop-toast-error`, percent-encoded, which
`openshop::testing::toast(&response)` and `toast_error(&response)` read
decoded, `422` and the errors under the fields when the form is refused.
`admin_context` and `invoke_admin_function` call a
function, or code that takes an `AdminContext`, directly. See
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
`APP.builder()?` is the builder of the declaration, to add to.

`.view(|request: PageRequest| async move { … })` gives it a view: the closure
runs the loaders it needs with `request.load(loader)` (an input read from
the query string with `request.load_with(loader)`) and returns the `Markup` of
the page; `request.form` is the form the page refused. `.view_with_form(…)`
is the same for a view that shows a refused form itself. What `#[view]`
declares is one of these.

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
value, or any `Serialize` value for the typed constructors. An action made by
hand answers an `Outcome` with `openshop::admin::output(Outcome::toast(…))`.
