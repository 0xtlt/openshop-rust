# Contributing

## Setup

- Rust 1.94 or newer (`rustup`), plus the `wasm32-unknown-unknown` target and
  [trunk](https://trunkrs.dev) if you work on the admin UI.
- PostgreSQL 17. `docker compose up -d` starts one on `localhost:5432` with the
  `openshop` role and database.

```bash
export DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop
cargo test --workspace
```

The tests use `TEST_DATABASE_URL`, or `DATABASE_URL` when it points at this
machine; they refuse a `DATABASE_URL` of another host, so a shell with a real
database in it never runs them. They fail within seconds, saying so, when
PostgreSQL does not answer.

Integration tests create one PostgreSQL schema per test and drop it afterwards,
so they run in parallel against a single database. They never touch the
`public` schema.

The integration tests of `openshop` are one binary, `crates/openshop/tests/it`,
linked once rather than once per file: a new file is a module of its
`main.rs`, and `cargo test -p openshop --test it engine_runner::` runs one
module. Its tests share the process: one that reads the process logger does it
with `common::LogCapture`, which takes turns with the others. The Sentry tests,
which bind a client for the whole process, and the `trybuild` tests of the
macros are binaries of their own.

## Checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

CI builds with `--locked`: `Cargo.lock` is committed, and a change to a
dependency commits the lockfile with it. It also checks that the workspace
builds with the oldest Rust it claims (`rust-version` in `Cargo.toml`), and runs
[cargo-deny](https://embarkstudios.github.io/cargo-deny/) on the dependencies,
as `deny.toml` configures it (`cargo install --locked cargo-deny`, then
`cargo deny check`). Every action of the workflows is pinned to a commit;
Dependabot proposes the updates. Three jobs check what no test runs: `docker`
makes a project of the `minimal` and of the `custom` template and builds its
image, `docs` builds the guide (`mise run docs:build`), and `windows` checks
that `openshop` and the CLI compile there.

The errors of the macros are checked with `trybuild` in
`crates/openshop/tests/macros.rs`: each file of `tests/macros/fail` is refused
with the error of its `.stderr`, those of `tests/macros/pass` compile and run.
The `.stderr` files are the output of the compiler pinned in `mise.toml`; after
a change of a macro or of the compiler, `TRYBUILD=overwrite cargo test -p
openshop --test macros` rewrites them, and the diff is what to review.

The admin UI is its own workspace:

```bash
cd crates/openshop-ui
cargo fmt --check
cargo clippy --target wasm32-unknown-unknown --locked -- -D warnings
cargo test --locked
```

`crates/openshop/ui-dist` is committed so that applications depending on the
framework get the admin UI without installing trunk. Rebuild and commit it when
you change `crates/openshop-ui` (or `openshop-shared`, or update the
dependencies of the admin UI):

```bash
mise run ui:build          # crates/openshop-ui/build-dist.sh
```

The script builds on a copy of the sources, in a fixed place, with the paths of
your machine rewritten, so that the bundle does not carry them and a checkout
anywhere builds the same bytes. It also writes `crates/openshop-ui/ui-dist.sha256`,
the hash of the sources the bundle is built from. CI computes it again
(`crates/openshop-ui/sources-hash.sh`) and fails when the sources moved without
a new bundle. It does not compare the bytes of a bundle it builds itself: they
depend on the platform (the compiler orders the functions differently on macOS
and on Linux, on x86_64 and on aarch64), so a bundle built on a Mac is never the
one built by CI.

The custom and public templates have admin pages with a view, so each has an
admin UI bundle of its own, compiled from its `admin` module. Build them into
`templates/custom/dist` and `templates/public/dist`, which are not committed:

```bash
mise run templates:ui      # `openshop build-ui` in each of them
```

## The templates

`openshop init` makes a project from one of the crates in `templates/`, which
are members of the workspace: they are compiled, linted and tested with it.
The CLI embeds them when it is built (`crates/openshop-cli/build.rs`). A
project gets `src/`, `tests/`, `migrations/`, `README.md`, `shopify.app.toml`
and its `Cargo.toml` with `workspace = true` replaced by the source of each
dependency, plus the files of `templates/shared`: a dotfile is stored there as
`_name`, and the lines between `# if views`, `# else` and `# end` are kept
or dropped depending on whether the admin pages of the template have views,
as in the `Dockerfile`, which builds their bundle. `templates/shared/rustfmt.toml`
is the repository's, so that a project is formatted as its template is. The rest, `examples/` among it, serves the repository only.

| Template | What a project starts with |
| --- | --- |
| `minimal` | One provider and one flow, with the built-in admin |
| `custom` | A custom app: every building block, the built-in admin and a page of its own |
| `public` | A public app: an admin made of its own pages, the operator dashboard for whoever runs it |

The migrations of a template are committed, and its tests check that they
cover the framework schema and its models: after a change to either, run
`cargo run -- migrate generate` in the template. To make a project from the
templates of your checkout, depending on it:

```bash
mise run init -- ../my-app --template=public
```

The custom and public templates show the two ways an app is distributed. A
change to the framework is tried on both:

| | `templates/custom` | `templates/public` |
| --- | --- | --- |
| The app | Built for the merchant who runs it | Installed by many shops |
| What a merchant sees | The built-in admin (flows, runs, logs, providers, crons, functions, MCP) and a page of the application | The pages of the application and nothing else: its home and its settings |
| Who reads the runs and the logs | The merchant, in Shopify admin | Whoever runs the app, on the operator dashboard at `/ops` |
| Made of | Every building block | A flow run for every shop each hour, a model, two admin pages, a webhook |
| Preview | `http://localhost:3999` | `http://localhost:3998` |

`apps/preview` is what serves either of them without a store.

## Running a template

To work on the framework and look at the result in a browser, without a store:

```bash
mise run dev:custom        # or dev:public
```

It needs the PostgreSQL of the tests: `mise run db:up`, or `DATABASE_URL`.
It serves the admin UI of the template on `http://localhost:3999/`
(`http://localhost:3998/` for the public one), as the staff of a fake shop,
and the operator dashboard at `/ops` (password `preview`). It is the preview
described below, as a shop and with the template's own bundle, run by
`openshop dev`: the template is started again when a crate of the workspace
changes, trunk builds its admin UI bundle again, and the page reloads by
itself. The pages of `/ops` run no script: reload them by hand.

To run a template in a development store, with the same loop:

```bash
cp .env.example templates/custom/.env     # add SHOPIFY_API_KEY and SHOPIFY_API_SECRET
mise run store:custom                     # `openshop dev` in templates/custom
```

`templates/public` and `mise run store:public` do the same for the public one.

The admin UI is an embedded Shopify app: it only starts inside the Shopify
admin iframe. To exercise the backend without a store, call the API with an
HS256 session token signed with `SHOPIFY_API_SECRET` (`aud` = `SHOPIFY_API_KEY`,
`iss` / `dest` = `https://<shop>.myshopify.com`), as the test harness does.

To look at an application without a store, run its preview. It serves the
template on `http://127.0.0.1:3999` (`3998` for the public one), in a throwaway
schema of the database above, and only accepts loopback connections. It
answers as in production: without a shop, the admin is the page that says
where the app opens from, and the
[operator dashboard](docs/src/reference/operator-dashboard.md) is at `/ops`
(password `preview`):

```bash
cargo run -p openshop-template-custom --example preview-custom    # or openshop-template-public, preview-public
```

To look at the admin UI itself, set `PREVIEW_AS_SHOP=1`: the preview then puts
a stub in place of Shopify App Bridge, with a session for a fixed test shop:

```bash
PREVIEW_AS_SHOP=1 cargo run -p openshop-template-custom --example preview-custom
```

Set `PREVIEW_UI_DIR=templates/custom/dist` to preview the template's own bundle
after building it with `mise run templates:ui`. The public template needs its own, as
its admin is made of its pages. The stub does not
reproduce what Shopify admin renders itself (navigation menu, title bar,
modals), so check those in a development store.

## Publishing to crates.io

Nothing is published yet: the CLI is installed from Git, and `openshop-cli`
cannot be published as it is. What stands in the way, for whoever decides to:

- The CLI embeds files outside its package. `build.rs` reads `templates/` at the
  root of the repository, and `init.rs` includes the `Cargo.toml` of the
  workspace (`include_str!("../../../Cargo.toml")`) for the `workspace = true`
  of the templates. `cargo package` only takes what is under `crates/openshop-cli`,
  so the tarball does not build. The templates, `templates/shared` and what
  `init.rs` reads of the workspace manifest would have to be copied under the
  package, or generated by a script that runs before `cargo publish`.
- `openshop init` has no source that exists on crates.io. A project depends on
  `openshop` by Git (`rev`, `tag`, `branch`) or by `path`, and a CLI built
  without git falls back to the tag `v<version>`. A published CLI would write
  `openshop = "<version>"`, and `bundle.rs`, which builds the admin UI bundle
  of a project, would have to accept that source too.
- The crates depend on each other by `path` with a `version`, so they publish in
  order: `openshop-shared`, `openshop-macros`,
  `openshop-ui`, `openshop`, then `openshop-cli`. `openshop-ui` is its own
  workspace, built for wasm32, with its own lockfile, and has to be published
  too since the `ui` feature of `openshop` depends on it. `openshop` embeds
  `ui-dist`, which is committed, so rebuild it first.

None of this changes the install command: `cargo install --locked --git
https://github.com/0xtlt/openshop-rust openshop-cli`.

## Conventions

- The TypeScript implementation is the reference for behavior. When the Rust
  version deliberately differs, document it in `docs/src/reference/differences.md`.
- Database rows and encrypted values must stay readable by both runtimes.
- Keep `openshop-shared` free of server-only dependencies: it compiles to WebAssembly.
