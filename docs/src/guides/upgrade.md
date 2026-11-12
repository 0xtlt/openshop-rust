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
   version of the CLI of crates.io that created the project, or the commit
   (`rev`) of a CLI built from the repository, or a tag given with
   `--openshop-tag`. Point both `openshop` entries at the release:

   ```toml
   [dependencies]
   openshop = { version = "<version>" }

   [dev-dependencies]
   openshop = { version = "<version>", features = ["testing"] }
   ```

   A project that stays on Git names the tag of the release instead:
   `git = "https://github.com/0xtlt/openshop-rust", tag = "v<version>"`.

   Then update the lockfile for that package only. Avoid an unreviewed
   `cargo update` of every dependency in the same deployment.

   ```bash
   cargo update -p openshop
   ```

   Open each custom admin page and send each of its forms: their views and
   `openshop.js` change with the framework.

3. Compare the current project with the template it was made from,
   `templates/<name>/` and `templates/shared/` in the matching release. Generated
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
cargo install --locked --force openshop-cli --version <version>
# or, from a checkout of the repository:
cargo install --locked --force --path crates/openshop-cli
```

`--force` replaces the CLI installed: without it, cargo keeps it when the
version number has not changed. Then `openshop --version` prints the version
and the commit of the CLI that runs; one that answers
`Unknown command: --version` is older than this check.

For migration steps, see [Manage migrations](manage-migrations.md). For the
package compatibility policy, see [Versioning](../reference/versioning.md).
