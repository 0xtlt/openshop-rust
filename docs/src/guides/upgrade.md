# Upgrade OpenShop

OpenShop is a v0 in development. Read the [repository changelog](https://github.com/0xtlt/openshop-rust/blob/main/CHANGELOG.md) and test an upgrade in
a non-production environment before changing production.

## Upgrade procedure

1. Record the current framework version and create a database backup. The first
   line of the output shows the version and the exact source revision:

   ```bash
   cargo tree -i openshop
   ```

2. Update to an explicit version first. The generated `Cargo.toml` pins the
   release that created the project with a Git tag, `v` followed by the
   version. Change the tag on both `openshop` entries:

   ```toml
   [dependencies]
   openshop = { git = "https://github.com/0xtlt/openshop-rust", tag = "v<version>" }

   [dev-dependencies]
   openshop = { git = "https://github.com/0xtlt/openshop-rust", tag = "v<version>", features = ["testing"] }
   ```

   Then update the lockfile for that package only. Avoid an unreviewed
   `cargo update` of every dependency in the same deployment.

   ```bash
   cargo update -p openshop
   ```

   If the project has its own admin UI bundle, set the same tag on its
   `openshop-ui` dependency and rebuild the bundle.

3. Compare the current project with
   `crates/openshop-cli/templates/minimal/` in the matching release. Generated
   projects are not rewritten automatically. Review `Cargo.toml`, Shopify
   TOML, `Dockerfile`, `Procfile`, `docker-compose.yml`, and `.env.example`.

4. Regenerate application-owned artifacts and check the app. The test command
   needs PostgreSQL; see [Test an app](test-app.md).

   ```bash
   cargo run -- migrate generate
   cargo run -- migrate check
   cargo check
   cargo test -- --include-ignored
   cargo build --release
   ```

5. Review and commit generated SQL. Apply it once before deploying the new web
   and worker processes. Locally, the command is `cargo run -- migrate`; with a
   deployed binary:

   ```bash
   app migrate
   ```

6. Deploy web and worker from the same build. Run a smoke flow, inspect its
   checkpoints, and verify the embedded admin.

## Rollback boundary

Application processes can be rolled back only while the database schema remains
compatible with the older version. Once a destructive migration is applied,
restore a backup or use a reviewed forward fix. Prefer additive migrations
during rolling upgrades.

## Template drift

`openshop init` creates a new project from the template bundled in the installed
`openshop` CLI. It does not update existing projects. Keep local template changes
explicit so later upgrades can distinguish application choices from generated
defaults.

The CLI is installed separately from your project's dependency. Reinstall it
when you upgrade so that `openshop init` and `openshop dev` come from the same
release:

```bash
cargo install --git https://github.com/0xtlt/openshop-rust openshop-cli
```

For migration steps, see [Manage migrations](manage-migrations.md). For the
package compatibility policy, see [Versioning](../reference/versioning.md).
