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

trunk build --release --dist target/ui-dist   # to look at it
./build-dist.sh                                # the committed bundle
```

The bundle (`index.html` and a hashed `.js` / `.wasm` pair) committed in
`../openshop/ui-dist` is built by `build-dist.sh` (`mise run ui:build`), which keeps the paths of
your machine out of it and writes `ui-dist.sha256`, the hash of its sources that CI checks. This
crate is its own cargo workspace: run `cargo` and `trunk` from this directory.

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
its pages. See
[Custom admin pages](https://github.com/0xtlt/openshop-rust/blob/main/docs/src/reference/custom-admin-pages.md)
in the OpenShop documentation, and `templates/custom` for a complete page, or
`templates/public` for an admin made of such pages only.
