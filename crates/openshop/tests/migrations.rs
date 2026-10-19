//! Client-owned SQL migrations (`./migrations`), ported from the TypeScript
//! `cli/schema.spec.ts` and adapted to the Rust migrator.

mod common;

use std::path::{Path, PathBuf};

use common::{TEST_SHOP, TempProject, create_config, empty_database, noop_flow, test_env};
use openshop::db::Table;
use openshop::db::migrate::{
    FRAMEWORK_MIGRATIONS, MigrationStatus, assert_migrated, check_migrations, generate_migrations,
    get_client_migration_status, migrate_schema, push_schema,
};
use openshop::testing::{MockTransport, TestDb};
use openshop::{DispatchOptions, OpenShop, PgPool};

const INIT: &str = "0000_project_init";
const NEXT: &str = "0001_project_next";

const INIT_SQL: &str = "
CREATE TABLE openshop_project_migration_test (
  id integer PRIMARY KEY
);
";
const NEXT_SQL: &str = "
ALTER TABLE openshop_project_migration_test ADD COLUMN name text;
";

/// A project directory that is removed when the test ends.
/// A project with two migrations, the second depending on the first.
fn project_with_migrations() -> TempProject {
    let project = TempProject::new();
    // Written out of order: only the file names decide the order.
    project.write(NEXT, NEXT_SQL);
    project.write(INIT, INIT_SQL);
    project
}

async fn tables(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = current_schema() ORDER BY table_name",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn columns(pool: &PgPool, table: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = $1 ORDER BY column_name",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn indexes(pool: &PgPool, table: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT indexname::text FROM pg_indexes \
         WHERE schemaname = current_schema() AND tablename = $1 ORDER BY indexname",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap()
}

const QUEUE_INDEXES: [&str; 2] = ["flow_runs_claimable_idx", "flow_runs_deadline_idx"];

/// Made redundant by the queue indexes, and dropped by a later migration.
const DROPPED_INDEX: &str = "flow_runs_status_available_created_idx";

/// Generates for an application without models and returns the files.
fn generate(cwd: &Path, args: &[String], production: bool) -> openshop::Result<Vec<PathBuf>> {
    Ok(generate_migrations(cwd, args, production, &[])?.files)
}

fn tags(tags: &[&str]) -> Vec<String> {
    tags.iter().map(|tag| tag.to_string()).collect()
}

async fn status(db: &TestDb, project: &TempProject) -> MigrationStatus {
    get_client_migration_status(&db.pool, project.path()).await.unwrap()
}

#[tokio::test]
async fn migrate_fails_when_client_migrations_are_missing() {
    let db = TestDb::create().await.unwrap();
    let project = TempProject::new();
    let expected = "[openshop] No client migrations found in ./migrations. Run `openshop migrate generate`, \
                    review the generated SQL, then run `openshop migrate`.";

    let error = migrate_schema(&db.pool, project.path()).await.unwrap_err();
    assert_eq!(error.to_string(), expected);
    assert_eq!(status(&db, &project).await, MigrationStatus::default());

    // A folder without any SQL file is the same as no folder.
    project.write_file("README.md", "Migrations live here.");
    let error = migrate_schema(&db.pool, project.path()).await.unwrap_err();
    assert_eq!(error.to_string(), expected);
    assert_eq!(status(&db, &project).await.folder, None);
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn status_reports_pending_client_migrations_before_migrate() {
    let db = TestDb::create().await.unwrap();
    let project = project_with_migrations();

    assert_eq!(
        status(&db, &project).await,
        MigrationStatus {
            folder: Some(project.folder()),
            applied: Vec::new(),
            pending: tags(&[INIT, NEXT]),
        }
    );
    assert!(columns(&db.pool, "openshop_project_migration_test").await.is_empty());
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn migrate_applies_client_migrations_in_file_name_order() {
    let db = TestDb::create().await.unwrap();
    let project = project_with_migrations();
    project.write(
        "0010_project_index",
        "CREATE INDEX project_name_idx ON openshop_project_migration_test (name);",
    );
    project.write(
        "0002_project_rename",
        "ALTER TABLE openshop_project_migration_test RENAME COLUMN name TO title;",
    );
    project.write(
        "0003_project_restore",
        "ALTER TABLE openshop_project_migration_test RENAME COLUMN title TO name;",
    );
    let all = tags(&[
        INIT,
        NEXT,
        "0002_project_rename",
        "0003_project_restore",
        "0010_project_index",
    ]);

    let applied = migrate_schema(&db.pool, project.path()).await.unwrap();
    assert_eq!(applied, all);

    let status = status(&db, &project).await;
    assert_eq!(status.applied, all);
    assert!(status.pending.is_empty());
    assert_eq!(
        columns(&db.pool, "openshop_project_migration_test").await,
        ["id", "name"]
    );

    let recorded: Vec<(String, String)> =
        sqlx::query_as("SELECT tag, checksum FROM __openshop_migrations ORDER BY tag")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(recorded.iter().map(|(tag, _)| tag.clone()).collect::<Vec<_>>(), all);
    assert!(recorded.iter().all(|(_, checksum)| checksum.len() == 64));
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn migrate_only_applies_what_is_pending() {
    let db = TestDb::create().await.unwrap();
    let project = project_with_migrations();

    assert_eq!(
        migrate_schema(&db.pool, project.path()).await.unwrap(),
        tags(&[INIT, NEXT])
    );
    // Neither migration can run twice, so a second pass has to skip them.
    assert!(migrate_schema(&db.pool, project.path()).await.unwrap().is_empty());
    assert!(migrate_schema(&db.pool, project.path()).await.unwrap().is_empty());

    project.write(
        "0002_project_more",
        "ALTER TABLE openshop_project_migration_test ADD COLUMN note text;",
    );
    assert_eq!(status(&db, &project).await.pending, tags(&["0002_project_more"]));
    assert_eq!(
        migrate_schema(&db.pool, project.path()).await.unwrap(),
        tags(&["0002_project_more"])
    );
    assert_eq!(
        columns(&db.pool, "openshop_project_migration_test").await,
        ["id", "name", "note"]
    );
    assert_eq!(
        status(&db, &project).await.applied,
        tags(&[INIT, NEXT, "0002_project_more"])
    );
    db.cleanup().await.unwrap();
}

// TS "migrate does not load drizzle config or require generation tooling": there is no
// external tooling here; the closest guarantee is that only `*.sql` files are read.
#[tokio::test]
async fn migrate_only_reads_sql_files_of_the_migrations_folder() {
    let db = TestDb::create().await.unwrap();
    let project = project_with_migrations();
    project.write_file("README.md", "not SQL");
    project.write_file("0002_draft.sql.bak", "SELECT * FROM table_that_does_not_exist;");
    project.write_file(".0003_hidden.sql", "SELECT * FROM table_that_does_not_exist;");
    project.write_file("archive/0004_old.sql", "SELECT * FROM table_that_does_not_exist;");
    std::fs::write(
        project.path().join("0005_outside.sql"),
        "SELECT * FROM table_that_does_not_exist;",
    )
    .unwrap();

    assert_eq!(
        migrate_schema(&db.pool, project.path()).await.unwrap(),
        tags(&[INIT, NEXT])
    );
    let status = status(&db, &project).await;
    assert_eq!(status.applied, tags(&[INIT, NEXT]));
    assert!(status.pending.is_empty());
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn migration_can_contain_several_statements_and_dollar_quoted_bodies() {
    let db = TestDb::create().await.unwrap();
    let project = project_with_migrations();
    project.write(
        "0002_project_trigger",
        "
-- Semicolons inside the function body must not split the migration.
CREATE FUNCTION project_default_name() RETURNS trigger AS $$
BEGIN
  NEW.name := coalesce(NEW.name, 'unnamed; really');
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER project_default_name BEFORE INSERT ON openshop_project_migration_test
  FOR EACH ROW EXECUTE FUNCTION project_default_name();

INSERT INTO openshop_project_migration_test (id) VALUES (1), (2);
",
    );

    migrate_schema(&db.pool, project.path()).await.unwrap();

    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM openshop_project_migration_test ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(names, ["unnamed; really", "unnamed; really"]);
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn migrate_rejects_a_migration_modified_after_it_was_applied() {
    let db = TestDb::create().await.unwrap();
    let project = project_with_migrations();
    migrate_schema(&db.pool, project.path()).await.unwrap();

    project.write(INIT, &INIT_SQL.replace("integer", "bigint"));
    project.write(
        "0002_project_more",
        "ALTER TABLE openshop_project_migration_test ADD COLUMN note text;",
    );
    let error = migrate_schema(&db.pool, project.path()).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "[openshop] Migration \"0000_project_init\" was modified after it was applied. \
         Restore it and add a new migration instead."
    );
    // Nothing after the tampered file is applied.
    assert_eq!(status(&db, &project).await.pending, tags(&["0002_project_more"]));
    assert_eq!(
        columns(&db.pool, "openshop_project_migration_test").await,
        ["id", "name"]
    );

    project.write(INIT, INIT_SQL);
    assert_eq!(
        migrate_schema(&db.pool, project.path()).await.unwrap(),
        tags(&["0002_project_more"])
    );
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn migrate_fails_on_real_migration_errors() {
    let db = TestDb::create().await.unwrap();
    let project = project_with_migrations();
    project.write(INIT, "SELECT * FROM table_that_does_not_exist;");

    let error = migrate_schema(&db.pool, project.path()).await.unwrap_err();
    assert_eq!(error.to_string(), "[openshop] Migration \"0000_project_init\" failed");
    assert!(format!("{error:#}").contains("table_that_does_not_exist"), "{error:#}");

    let status = status(&db, &project).await;
    assert!(status.applied.is_empty());
    assert_eq!(status.pending, tags(&[INIT, NEXT]));
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn failing_migration_is_rolled_back_and_reported_by_its_tag() {
    let db = TestDb::create().await.unwrap();
    let project = project_with_migrations();
    project.write(
        "0002_broken",
        "
CREATE TABLE half_done (id integer PRIMARY KEY);
INSERT INTO openshop_project_migration_test (id, name) VALUES (1, 'half');
INSERT INTO table_that_does_not_exist VALUES (1);
",
    );
    project.write(
        "0003_never_reached",
        "CREATE TABLE never_reached (id integer PRIMARY KEY);",
    );

    let error = migrate_schema(&db.pool, project.path()).await.unwrap_err();
    assert_eq!(error.to_string(), "[openshop] Migration \"0002_broken\" failed");
    assert!(format!("{error:#}").contains("table_that_does_not_exist"), "{error:#}");

    // Earlier migrations stay applied; the failing one leaves nothing behind.
    let status_after_failure = status(&db, &project).await;
    assert_eq!(status_after_failure.applied, tags(&[INIT, NEXT]));
    assert_eq!(
        status_after_failure.pending,
        tags(&["0002_broken", "0003_never_reached"])
    );
    let existing = tables(&db.pool).await;
    assert!(existing.contains(&"openshop_project_migration_test".to_string()));
    assert!(!existing.contains(&"half_done".to_string()));
    assert!(!existing.contains(&"never_reached".to_string()));
    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM openshop_project_migration_test")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    // Once the file is fixed, the migration resumes where it stopped.
    project.write("0002_broken", "CREATE TABLE half_done (id integer PRIMARY KEY);");
    assert_eq!(
        migrate_schema(&db.pool, project.path()).await.unwrap(),
        tags(&["0002_broken", "0003_never_reached"])
    );
    db.cleanup().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_migrators_apply_each_migration_once() {
    let db = TestDb::create().await.unwrap();
    let project = project_with_migrations();
    project.write(
        "0002_project_seed",
        "INSERT INTO openshop_project_migration_test (id, name) VALUES (1, 'seed');",
    );

    // Several replicas deploying at once.
    let migrators: Vec<_> = (0..6)
        .map(|_| {
            let (pool, cwd) = (db.pool.clone(), project.path().to_path_buf());
            tokio::spawn(async move { migrate_schema(&pool, &cwd).await })
        })
        .collect();
    let mut applied = Vec::new();
    for migrator in migrators {
        applied.extend(migrator.await.unwrap().expect("every migrator succeeds"));
    }
    applied.sort();

    assert_eq!(applied, tags(&[INIT, NEXT, "0002_project_seed"]));
    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM openshop_project_migration_test")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn generate_then_migrate_creates_the_framework_tables() {
    let db = empty_database().await;
    assert!(tables(&db.pool).await.is_empty());
    let project = TempProject::new();
    let error = check_migrations(project.path(), &[]).unwrap_err();
    assert!(error.to_string().contains("is not covered by ./migrations"), "{error}");

    let created = generate(project.path(), &[], false).unwrap();
    assert_eq!(created.len(), FRAMEWORK_MIGRATIONS.len());
    assert!(created.iter().all(|path| path.starts_with(project.folder())));
    check_migrations(project.path(), &[]).unwrap();
    let generated: Vec<String> = created
        .iter()
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(status(&db, &project).await.pending, generated);

    assert_eq!(migrate_schema(&db.pool, project.path()).await.unwrap(), generated);
    assert_eq!(
        tables(&db.pool).await,
        [
            "__openshop_migrations",
            "cron_overrides",
            "cron_ticks",
            "flow_runs",
            "installations",
            "logs",
            "mcp_audit_logs",
            "mcp_permission_grants",
            "mcp_tokens",
            "provider_configs",
            "step_results",
        ]
    );

    // The migrated database is a working one.
    let app = OpenShop::with_transport(
        create_config([noop_flow("test-flow")]),
        db.pool.clone(),
        test_env(),
        MockTransport::new(),
    )
    .unwrap();
    app.dispatch_flow(DispatchOptions::new("test-flow", TEST_SHOP))
        .await
        .unwrap();

    // Nothing is left to generate or to apply.
    assert!(generate(project.path(), &[], false).unwrap().is_empty());
    assert!(migrate_schema(&db.pool, project.path()).await.unwrap().is_empty());
    let status = status(&db, &project).await;
    assert_eq!(status.applied, generated);
    assert!(status.pending.is_empty());
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn generate_then_migrate_upgrades_a_deployed_framework_schema() {
    // A project created when the framework schema was one migration, deployed and in use.
    let db = empty_database().await;
    let project = TempProject::new();
    let init = &FRAMEWORK_MIGRATIONS[0];
    project.write(
        "20260101000000_openshop_init",
        &format!("-- openshop:framework {}\n\n{}", init.id, init.sql),
    );
    migrate_schema(&db.pool, project.path()).await.unwrap();
    sqlx::query(
        "INSERT INTO flow_runs (shop, flow_name, available_at, deadline_at) VALUES ($1, 'test-flow', now(), now())",
    )
    .bind(TEST_SHOP)
    .execute(&db.pool)
    .await
    .unwrap();
    let before = indexes(&db.pool, "flow_runs").await;
    assert!(QUEUE_INDEXES.iter().all(|index| !before.contains(&index.to_string())));
    assert!(check_migrations(project.path(), &[]).is_err());

    // Only what the project lacks is generated, and it applies to a table that has rows.
    let created = generate(project.path(), &[], false).unwrap();
    assert_eq!(created.len(), FRAMEWORK_MIGRATIONS.len() - 1);
    check_migrations(project.path(), &[]).unwrap();
    assert_eq!(
        migrate_schema(&db.pool, project.path()).await.unwrap().len(),
        created.len()
    );
    let after = indexes(&db.pool, "flow_runs").await;
    assert!(
        QUEUE_INDEXES.iter().all(|index| after.contains(&index.to_string())),
        "{after:?}"
    );
    // Nothing is lost but the index that the queue indexes replace.
    assert!(before.contains(&DROPPED_INDEX.to_string()) && !after.contains(&DROPPED_INDEX.to_string()));
    assert!(
        before
            .iter()
            .filter(|index| *index != DROPPED_INDEX)
            .all(|index| after.contains(index))
    );
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn generated_and_hand_written_migrations_apply_together() {
    let db = empty_database().await;
    let project = TempProject::new();

    let created = generate(project.path(), &["--name=Add Reviews".to_string()], false).unwrap();
    assert_eq!(created.len(), FRAMEWORK_MIGRATIONS.len() + 1);
    let custom = created.last().unwrap();
    assert!(custom.to_string_lossy().ends_with("_add_reviews.sql"));
    // The hand-written part references a framework table, so it has to run after it.
    std::fs::write(
        custom,
        "CREATE TABLE reviews (id integer PRIMARY KEY, shop text NOT NULL);\n\
         CREATE INDEX reviews_run_idx ON flow_runs (parent_run_id);\n",
    )
    .unwrap();

    let applied = migrate_schema(&db.pool, project.path()).await.unwrap();
    assert_eq!(applied.len(), created.len());
    assert!(tables(&db.pool).await.contains(&"reviews".to_string()));
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn generated_placeholder_migration_can_be_applied_before_it_is_filled_in() {
    let db = empty_database().await;
    let project = TempProject::new();

    let created = generate(project.path(), &["--name=later".to_string()], false).unwrap();
    let placeholder = std::fs::read_to_string(created.last().unwrap()).unwrap();
    assert!(placeholder.lines().all(|line| line.starts_with("--")), "{placeholder}");

    let applied = migrate_schema(&db.pool, project.path()).await.unwrap();
    assert_eq!(applied.len(), created.len());
    assert!(status(&db, &project).await.pending.is_empty());
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn generate_refuses_to_run_in_production() {
    let project = TempProject::new();

    let error = generate(project.path(), &[], true).unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("[openshop] Refusing to generate migrations in production."),
        "{error}"
    );
    assert!(!project.folder().exists());

    let allowed = ["--allow-generate-in-production".to_string()];
    assert_eq!(
        generate(project.path(), &allowed, true).unwrap().len(),
        FRAMEWORK_MIGRATIONS.len()
    );
}

#[tokio::test]
async fn push_schema_creates_framework_and_model_tables() {
    let db = empty_database().await;
    let reviews = Table::conventional("reviews")
        .column("rating", "integer NOT NULL")
        .unique_index("reviews_shop_rating_unique", &["shop", "rating"]);

    let error = push_schema(&db.pool, std::slice::from_ref(&reviews), true)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "[openshop] Refusing to push the schema in production. Run `openshop migrate` before starting OpenShop."
    );
    assert!(tables(&db.pool).await.is_empty());

    // Pushing is idempotent: `dev` does it on every start.
    for _ in 0..2 {
        push_schema(&db.pool, std::slice::from_ref(&reviews), false)
            .await
            .unwrap();
    }
    let existing = tables(&db.pool).await;
    assert!(existing.contains(&"flow_runs".to_string()));
    assert!(existing.contains(&"reviews".to_string()));
    assert!(!existing.contains(&"__openshop_migrations".to_string()));
    let queue = indexes(&db.pool, "flow_runs").await;
    assert!(
        QUEUE_INDEXES.iter().all(|index| queue.contains(&index.to_string())),
        "{queue:?}"
    );
    assert_eq!(
        columns(&db.pool, "reviews").await,
        ["created_at", "id", "rating", "shop", "updated_at"]
    );
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn start_and_worker_refuse_a_database_that_is_behind_the_migrations() {
    let db = empty_database().await;
    let project = TempProject::new();

    // No migrations folder: nothing to compare the database with.
    assert_migrated(&db.pool, project.path()).await.unwrap();

    // The framework schema is not in the folder yet.
    project.write(INIT, INIT_SQL);
    let error = assert_migrated(&db.pool, project.path()).await.unwrap_err();
    assert!(error.to_string().contains("is not covered by ./migrations"), "{error}");

    // Covered, but never applied: the table that tracks them does not exist either.
    generate(project.path(), &[], false).unwrap();
    let error = assert_migrated(&db.pool, project.path()).await.unwrap_err().to_string();
    let count = FRAMEWORK_MIGRATIONS.len() + 1;
    assert!(
        error.starts_with(&format!(
            "[openshop] Not starting: {count} migrations are not applied ("
        )),
        "{error}"
    );
    assert!(
        error.contains(INIT) && error.ends_with("Run `migrate` first."),
        "{error}"
    );
    // Looking did not create anything.
    assert!(tables(&db.pool).await.is_empty());

    // One more migration, after the others were applied.
    migrate_schema(&db.pool, project.path()).await.unwrap();
    assert_migrated(&db.pool, project.path()).await.unwrap();
    project.write(NEXT, NEXT_SQL);
    let error = assert_migrated(&db.pool, project.path()).await.unwrap_err().to_string();
    assert_eq!(
        error,
        format!("[openshop] Not starting: 1 migration is not applied ({NEXT}). Run `migrate` first.")
    );

    migrate_schema(&db.pool, project.path()).await.unwrap();
    assert_migrated(&db.pool, project.path()).await.unwrap();
    db.cleanup().await.unwrap();
}
