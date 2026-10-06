# openshop-demo-ui

The admin UI bundle of the demo app: the built-in OpenShop screens plus the custom `/reviews`
page. The page's loader and action are registered in `apps/demo/src/admin/reviews.rs`; its view is
`ReviewsPage` in [`src/main.rs`](src/main.rs).

This crate is its own cargo workspace, built for `wasm32-unknown-unknown` with
[trunk](https://trunkrs.dev):

```bash
cd apps/demo/ui
trunk build --release        # writes dist/
```

Then, from the repository root, run the demo with this bundle instead of the embedded one:

```bash
OPENSHOP_UI_DIR=apps/demo/ui/dist cargo run -p openshop-demo -- dev
```

See `crates/openshop-ui/README.md` for the custom page API.
