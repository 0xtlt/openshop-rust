# Versioning

OpenShop is a v0 in development. Until `1.0`, any update can change public
Rust APIs, configuration, generated template files, database migrations, CLI
behavior, and documented workflows, without a transition period.

The crates `openshop`, `openshop-shared`, `openshop-macros` and `openshop-cli`
share one version number and require Rust 1.94 or newer.

## Pinning policy

- Applications should pin an exact OpenShop version while evaluating an update.
- Commit `Cargo.lock` and generated migrations.
- Deploy web and worker from the same binary.
- Do not assume `openshop init` updates an existing project.

`openshop init` pins the `openshop` dependency of a new project to the Git
commit the CLI that generated it was built from, which the CLI records when it
is built from a checkout of the repository, as `cargo install --git` does:

```toml
[dependencies]
openshop = { git = "https://github.com/0xtlt/openshop-rust", rev = "<commit>" }

[dev-dependencies]
openshop = { git = "https://github.com/0xtlt/openshop-rust", rev = "<commit>", features = ["testing"] }
```

A CLI built without git, from a package say, pins the tag `v<version>` of its
own version instead, and says so: the tag has to exist in the repository.
`--openshop-tag=<tag>`, `--openshop-rev=<commit>` and `--openshop-branch=<name>`
name another source; see the [CLI reference](cli.md#scaffold-a-project).

Change both entries together when you upgrade. A generated project is
app-owned after creation. Updating the dependency does not rewrite its
`main.rs`, configuration, or deployment files.

## Compatibility surfaces

| Surface | Compatibility check |
| --- | --- |
| Rust API | Run `cargo check` and the application tests. |
| Generated template | Compare `Cargo.toml`, `src/main.rs`, Shopify TOML, Docker, and `Procfile` files with a freshly generated project. |
| Configuration | Build the configuration, for example with `APP.config()` in a test, so validation checks registered names, paths, schedules, and positive numeric options. |
| Database | Run `migrate generate`, review the SQL, and run `migrate check`; test forward migration and application rollback compatibility. |
| Custom admin pages | Open each page and send each form: their views and `openshop.js` change with `openshop`. |
| Web and worker | Deploy the exact same binary and configuration. |

This implementation follows the behavior of OpenShop for TypeScript
`0.0.12-beta.0` and uses the same database tables and encryption formats. See
[Differences from the TypeScript version](differences.md).

See [Upgrade OpenShop](../guides/upgrade.md) for the step-by-step procedure.
