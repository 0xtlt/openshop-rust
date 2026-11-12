# Migrate to the server-rendered admin

Use this guide to move an application whose admin pages have a Leptos view
(`#[view] fn view() -> impl IntoView`) to views the server renders
(`#[view] fn view(…) -> Markup`). The admin is now rendered by the server:
the WebAssembly bundle, trunk, the `ui` feature and `openshop::ui` are gone,
and a Leptos view no longer compiles.

A page keeps its access rule, its loaders and its actions. Three things
change in it: the view takes what the loaders return and gives back HTML, an
action that showed a toast returns an `Outcome`, and a button that ran an
action is a form. Count about 30 minutes per page.

## 1. The project

1. Install the `openshop` CLI of the release you move to: the one you have
   runs trunk, and serves a bundle the server no longer has. From crates.io:

   ```bash
   cargo install --locked --force openshop-cli --version <version>
   ```

   or from a checkout of the repository at that release,
   `cargo install --locked --path crates/openshop-cli --force`.
   Then check which one runs:

   ```bash
   openshop --version   # openshop <version> (<commit>)
   ```

   It prints the version, and, for a CLI built from the repository, the first
   12 characters of the commit it was built from. A CLI that answers `Unknown command: --version` is the old
   one: `which openshop` says where it is, and the install did not replace
   it. Run in a project that still has the Leptos admin, `openshop dev`
   lists what is left of it (`features = ["ui"]`, `leptos`,
   `UiSource::Dir`…) before it starts.
2. In `Cargo.toml`, remove `features = ["ui"]` from `openshop` and remove
   `leptos`; add `maud`, whose `html!` the views are written with:

   ```toml
   [dependencies]
   openshop = { version = "<version>" }
   maud = "0.27"
   ```

3. Remove the `admin_ui: UiSource::Dir(…)` or `UiSource::Assets(…)` setting of
   `app!`, the `rust-embed` struct that served `dist/`, and `dist/` itself.
   `UiSource::None` (an API-only server) stays.
4. Remove `OPENSHOP_UI_DIR` from the environment of your deployments, and the
   `wasm32-unknown-unknown` target and trunk from your toolchain
   (`mise.toml`, CI).
5. In the `Dockerfile`, remove what built the bundle: the WebAssembly target,
   trunk, the `openshop` CLI, `openshop build`, the copy of `dist` and
   `ENV OPENSHOP_UI_DIR`. The image builds the application with
   `cargo build --release`, as the `Dockerfile` of a new project does.

`openshop build-ui` no longer exists; `openshop build` builds the
application. `openshop dev` restarts the application when a page changes, and
the page reloads.

## 2. A page

An app's `src/admin/home.rs`, before:

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
                        shopify::toast(if hidden { "Banner hidden" } else { "Banner shown" });
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
                                    "The banner is hidden from the checkout page"
                                } else {
                                    "The banner shows on the checkout page"
                                }
                            >
                                <s-button
                                    variant="primary"
                                    tone=if hidden { "critical" } else { "auto" }
                                    loading=move || set_hidden.pending.get()
                                    on:click=move |_| toggle(!hidden)
                                >
                                    {if hidden { "Show the banner" } else { "Hide the banner" }}
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
    async fn set_hidden(ctx: AdminContext, input: SetHidden) -> Result<Outcome> {
        // … unchanged …
        Ok(Outcome::toast(if input.hidden { "Banner hidden" } else { "Banner shown" }))
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
                                "The banner is hidden from the checkout page"
                            } else {
                                "The banner shows on the checkout page"
                            })
                        {
                            form method="post" {
                                input type="hidden" name="_action" value=(Self::SET_HIDDEN);
                                input type="hidden" name="hidden" value=(!hidden);
                                s-button type="submit" variant="primary" tone=(if hidden { "critical" } else { "auto" }) {
                                    (if hidden { "Show the banner" } else { "Hide the banner" })
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

What went away: the loading and error states (the server renders the page
once it has the data; a loader that fails shows its error in place of the
page), `revalidate` (the page is rendered again after the action), `pending`
(the submit button shows `loading` while the form is sent), and `spawn_local`.
The loader does not change. The action changes only what it returns.

`Serialize` stays on what a loader returns, and `Deserialize` where a test
reads it with `ctx.call_loader`.

## 3. What each piece of the Leptos API becomes

| Leptos view | Server-rendered view |
| --- | --- |
| `use_loader(Self::LIST, || ())` | A parameter of the view named after the loader: `fn view(list: List) -> Markup`. |
| `use_loader(Self::LIST, move || Filter { … })` | A loader with an `#[input]`, read from the query string of the page; a `<form method="get">` or a link changes it. |
| `use_action(Self::SAVE)` and `invoke(input, options)` | A `<form method="post">` with `input type="hidden" name="_action" value=(Self::SAVE)` and the fields of the input, each with its `name`. |
| `.revalidate(Self::LIST)` | Nothing: the page is rendered again after the action. |
| `pending`, `loading` | Nothing: the submit button shows `loading`. |
| `shopify::toast("Saved")` after an action | The action returns `Outcome::toast("Saved")`. |
| `shopify::toast_error(…)`, `error.message` | The action fails with an `AdminPublicError` (an error toast with its message), or returns `Outcome::error(…)`. |
| `error.field_errors` | A `form: FormState` parameter: `value=(form.value_or("title", saved)) error=[form.error("title")]`. The input schema's refusals and `Outcome::invalid([("title", "…")])` fill it. |
| `navigate("/reviews/42")` after an action | `Outcome::redirect("/reviews/42").with_toast(…)`. |
| `s-link href=…` and `navigate` elsewhere | A link with the path: `a href`, `s-link href`, `s-button href`. |
| `fn view(params: RouteParams)` | The loader reads `ctx.params.get("id")`. |
| `prop:value` with `on:input` and a signal | A field with `name` and `value=(…)`: what it holds is sent with the form. |
| A polling timer (`every`) | `data-openshop-refresh="5000"` on an element of the page while something runs. |
| Copying to the clipboard | `s-button data-openshop-copy=(text)`. |
| Components (`#[component]`) | Functions that return `Markup`. |

See [Custom admin pages](../reference/custom-admin-pages.md#views) for the
whole contract.

## 4. The tests

The RPC transport of the Leptos views (`/api/pages/custom/…/_rpc/…`) is gone.
A test that posted to it calls the function, or the page:

- `ctx.call_loader::<Home>("/", "home", ())` and
  `ctx.call_action::<Outcome>("/", "setHidden", json!({ "hidden": true }))`
  run the access rules, the schema and the function, as before, and give back
  what it returns;
- `ctx.page("/")` asks for the page as the admin does: the HTML of the whole
  document, the menu and the page in `<main id="openshop-page">`;
- `ctx.submit("/", &[("_action", HomePage::SET_HIDDEN), ("hidden", "true")])`
  sends a form of it: the page rendered again, the toast in the header
  `x-openshop-toast` (an error toast in `x-openshop-toast-error`),
  percent-encoded, `422` with the error under each field when the form is
  refused. `openshop::testing::toast` and `toast_error` read the toasts
  decoded.

```rust,ignore
use openshop::testing::{TestContext, toast};

#[openshop::test(APP)]
async fn hides_the_banner(ctx: &TestContext) {
    let hidden = ctx
        .submit("/", &[("_action", HomePage::SET_HIDDEN), ("hidden", "true")])
        .send()
        .await
        .unwrap();
    assert_eq!(hidden.status(), 200);
    assert_eq!(toast(&hidden).as_deref(), Some("Banner hidden"));
    let page = hidden.text().await.unwrap();
    assert!(page.contains("Show the banner"), "{page}");
}
```

A helper of the tests names what the context sends and what it answers as
`openshop::testing::RequestBuilder` and `openshop::testing::Response`: the
tests need no `reqwest` dependency.

## 5. Check

1. `openshop --version` prints the version you moved to.
2. `cargo build` and `cargo test`, without a warning from `openshop`.
3. `openshop dev`, then open each page from Shopify admin: what it shows, each
   form, each link.
4. Deploy: the image has no bundle, and `OPENSHOP_UI_DIR` is not set.
