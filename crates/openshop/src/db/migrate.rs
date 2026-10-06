//! Client-owned SQL migrations in `./migrations`.
//!
//! The framework ships its schema as embedded SQL. `migrate generate` copies the
//! parts a project does not have yet into `./migrations`, and writes the
//! statements that bring the application tables in line with the models. Both
//! are reviewed and committed like any other migration; `migrate` applies
//! pending files in file-name order. Development and tests push the schema
//! directly instead.
//!
//! The models as of the last generated migration are recorded next to the
//! migrations, in `schema.snapshot.json`. The next generation compares the
//! models with it, so it needs no database.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::{Executor, PgPool};

use super::diff::{self, Change, SNAPSHOT_FILE, SchemaSnapshot, TableSnapshot};
use super::model::{Model, quote};

pub const MIGRATIONS_DIR: &str = "migrations";
const MIGRATIONS_TABLE: &str = "__openshop_migrations";
const FRAMEWORK_MARKER: &str = "-- openshop:framework ";

/// A schema change shipped with the framework.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameworkMigration {
    /// Stable identifier recorded in the generated file.
    pub id: &'static str,
    /// File-name suffix of the generated migration.
    pub name: &'static str,
    pub sql: &'static str,
}

pub const FRAMEWORK_MIGRATIONS: &[FrameworkMigration] = &[FrameworkMigration {
    id: "0001_openshop_init",
    name: "openshop_init",
    sql: include_str!("../../migrations/0001_openshop_init.sql"),
}];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationFile {
    /// File name without the `.sql` extension.
    pub tag: String,
    pub path: PathBuf,
    pub sql: String,
    pub checksum: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationStatus {
    pub folder: Option<PathBuf>,
    pub applied: Vec<String>,
    pub pending: Vec<String>,
}

fn checksum(sql: &str) -> String {
    hex::encode(Sha256::digest(sql.as_bytes()))
}

fn sql_file_names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_file())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| name.ends_with(".sql") && !name.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// `./migrations` when it contains at least one SQL file.
pub fn resolve_client_migrations_folder(cwd: &Path) -> Option<PathBuf> {
    let folder = cwd.join(MIGRATIONS_DIR);
    (!sql_file_names(&folder).is_empty()).then_some(folder)
}

pub fn read_migration_files(folder: &Path) -> Result<Vec<MigrationFile>> {
    sql_file_names(folder)
        .into_iter()
        .map(|name| {
            let path = folder.join(&name);
            let sql = std::fs::read_to_string(&path)
                .with_context(|| format!("[openshop] Failed to read migration {}", path.display()))?;
            Ok(MigrationFile {
                tag: name.trim_end_matches(".sql").to_string(),
                checksum: checksum(&sql),
                path,
                sql,
            })
        })
        .collect()
}

const MIGRATION_LOCK: &str = "SELECT pg_advisory_xact_lock(hashtext('openshop:migrate'))";

async fn ensure_migrations_table(pool: &PgPool) -> Result<()> {
    // `CREATE TABLE IF NOT EXISTS` fails when two sessions create the table at
    // the same time, so concurrent migrators create it under the migration lock.
    let mut tx = pool.begin().await?;
    sqlx::query(MIGRATION_LOCK).execute(&mut *tx).await?;
    sqlx::query(&format!(
        "CREATE TABLE IF NOT EXISTS {MIGRATIONS_TABLE} (\
           tag text PRIMARY KEY, \
           checksum text NOT NULL, \
           applied_at timestamp with time zone DEFAULT now() NOT NULL\
         )"
    ))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn applied_migrations(pool: &PgPool) -> Result<Vec<(String, String)>> {
    ensure_migrations_table(pool).await?;
    Ok(
        sqlx::query_as(&format!("SELECT tag, checksum FROM {MIGRATIONS_TABLE} ORDER BY tag"))
            .fetch_all(pool)
            .await?,
    )
}

fn assert_unmodified(file: &MigrationFile, applied: &[(String, String)]) -> Result<bool> {
    match applied.iter().find(|(tag, _)| *tag == file.tag) {
        Some((_, recorded)) if *recorded != file.checksum => bail!(
            "[openshop] Migration \"{}\" was modified after it was applied. Restore it and add a new migration instead.",
            file.tag
        ),
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

pub async fn get_client_migration_status(pool: &PgPool, cwd: &Path) -> Result<MigrationStatus> {
    let Some(folder) = resolve_client_migrations_folder(cwd) else {
        return Ok(MigrationStatus::default());
    };

    let applied = applied_migrations(pool).await?;
    let mut status = MigrationStatus {
        folder: Some(folder.clone()),
        ..Default::default()
    };
    for file in read_migration_files(&folder)? {
        if applied.iter().any(|(tag, _)| *tag == file.tag) {
            status.applied.push(file.tag);
        } else {
            status.pending.push(file.tag);
        }
    }
    Ok(status)
}

const NO_MIGRATIONS: &str = "[openshop] No client migrations found in ./migrations. Run `openshop migrate generate`, review the generated SQL, then run `openshop migrate`.";

/// Applies pending migrations, each in its own transaction. Returns the applied tags.
pub async fn migrate_schema(pool: &PgPool, cwd: &Path) -> Result<Vec<String>> {
    let Some(folder) = resolve_client_migrations_folder(cwd) else {
        bail!(NO_MIGRATIONS);
    };
    let files = read_migration_files(&folder)?;
    ensure_migrations_table(pool).await?;

    let mut newly_applied = Vec::new();
    for file in files {
        let mut tx = pool.begin().await?;
        // Serialize concurrent migrators (several replicas deploying at once).
        sqlx::query(MIGRATION_LOCK).execute(&mut *tx).await?;
        let applied: Vec<(String, String)> =
            sqlx::query_as(&format!("SELECT tag, checksum FROM {MIGRATIONS_TABLE} WHERE tag = $1"))
                .bind(&file.tag)
                .fetch_all(&mut *tx)
                .await?;
        if assert_unmodified(&file, &applied)? {
            continue;
        }

        // Called through `Executor` so that the future of this function stays `Send`.
        (&mut *tx)
            .execute(sqlx::raw_sql(&file.sql))
            .await
            .with_context(|| format!("[openshop] Migration \"{}\" failed", file.tag))?;
        sqlx::query(&format!(
            "INSERT INTO {MIGRATIONS_TABLE} (tag, checksum) VALUES ($1, $2)"
        ))
        .bind(&file.tag)
        .bind(&file.checksum)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        newly_applied.push(file.tag);
    }

    Ok(newly_applied)
}

fn covered_framework_ids(files: &[MigrationFile]) -> Vec<String> {
    files
        .iter()
        .flat_map(|file| file.sql.lines())
        .filter_map(|line| line.trim().strip_prefix(FRAMEWORK_MARKER))
        .map(|id| id.trim().to_string())
        .collect()
}

fn missing_framework_migrations(folder: &Path) -> Result<Vec<&'static FrameworkMigration>> {
    let files = if folder.exists() {
        read_migration_files(folder)?
    } else {
        Vec::new()
    };
    let covered = covered_framework_ids(&files);
    Ok(FRAMEWORK_MIGRATIONS
        .iter()
        .filter(|migration| !covered.iter().any(|id| id == migration.id))
        .collect())
}

fn sanitize_name(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('_').to_string();
    if sanitized.is_empty() {
        "migration".to_string()
    } else {
        sanitized
    }
}

/// Writes migrations for framework schema the project does not cover yet, plus an
/// empty one when `--name=<name>` is given. Returns the created files.
pub fn generate_client_migrations(cwd: &Path, args: &[String], production: bool) -> Result<Vec<PathBuf>> {
    generate_client_migrations_for(cwd, args, production, &[])
}

/// Models whose table is not created by any migration file.
fn uncovered_models<'a>(folder: &Path, models: &'a [Model]) -> Result<Vec<&'a Model>> {
    let files = if folder.exists() {
        read_migration_files(folder)?
    } else {
        Vec::new()
    };
    Ok(models
        .iter()
        .filter(|model| {
            let pattern = format!(
                r#"(?i)create\s+table\s+(?:if\s+not\s+exists\s+)?"?{}"?[\s(]"#,
                regex::escape(&model.table)
            );
            let creates_table = regex::Regex::new(&pattern).expect("valid regex");
            !files.iter().any(|file| creates_table.is_match(&file.sql))
        })
        .collect())
}

/// The snapshot the models are compared with. A project without one starts
/// from the tables its migrations already create, taken as the models
/// describe them today.
///
/// A table that a hand-written migration creates, and that the snapshot does
/// not know, is taken the same way: generating does not create it again.
fn baseline(folder: &Path, models: &[Model]) -> Result<(SchemaSnapshot, bool)> {
    let recorded = SchemaSnapshot::read(folder)?;
    let has_snapshot = recorded.is_some();
    let mut snapshot = recorded.unwrap_or_default();

    let uncovered = uncovered_models(folder, models)?;
    for model in models {
        let known = |name: &str| snapshot.tables.iter().any(|table| table.name == name);
        let renamed = model.previous_name().is_some_and(known);
        let created_by_hand = !uncovered.iter().any(|new| new.table == model.table);
        if !known(&model.table) && !renamed && created_by_hand {
            snapshot.tables.push(TableSnapshot::of(model));
        }
    }
    Ok((snapshot, has_snapshot))
}

/// What `migrate generate` wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeneratedMigrations {
    /// The migration files, in the order they apply.
    pub files: Vec<PathBuf>,
    /// The statements of the model migration that deserve a second look.
    pub warnings: Vec<String>,
    /// The schema snapshot, when it was created or changed.
    pub snapshot: Option<PathBuf>,
}

/// Like [`generate_client_migrations`], and also writes the statements that
/// bring the application tables in line with `models`. Returns the created files.
pub fn generate_client_migrations_for(
    cwd: &Path,
    args: &[String],
    production: bool,
    models: &[Model],
) -> Result<Vec<PathBuf>> {
    Ok(generate_migrations(cwd, args, production, models)?.files)
}

/// Writes the migrations a project is missing:
///
/// - the framework schema it does not cover yet;
/// - the statements that take the application tables from the schema snapshot
///   to `models`: created and dropped tables, added, dropped, renamed and
///   altered columns, created and dropped indexes;
/// - an empty migration when `--name=<name>` is given and nothing changed.
///
/// The snapshot is then updated to `models`.
pub fn generate_migrations(
    cwd: &Path,
    args: &[String],
    production: bool,
    models: &[Model],
) -> Result<GeneratedMigrations> {
    let allow_production = args.iter().any(|arg| arg == "--allow-generate-in-production");
    if production && !allow_production {
        bail!(
            "[openshop] Refusing to generate migrations in production. Generate migrations locally, commit them, then run `openshop migrate` during deploy."
        );
    }

    let name = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--name="))
        .map(sanitize_name);

    let folder = cwd.join(MIGRATIONS_DIR);
    let missing = missing_framework_migrations(&folder)?;
    let (previous, has_snapshot) = baseline(&folder, models)?;
    let changes = diff::diff(&previous, models);
    let current = SchemaSnapshot::of(models);
    // A project without models and without snapshot does not get one.
    let snapshot_json = (has_snapshot || !models.is_empty()).then(|| current.to_json());
    let snapshot_path = folder.join(SNAPSHOT_FILE);
    let snapshot_changed = snapshot_json
        .as_ref()
        .is_some_and(|json| std::fs::read_to_string(&snapshot_path).ok().as_ref() != Some(json));

    let mut generated = GeneratedMigrations::default();
    if missing.is_empty() && changes.is_empty() && name.is_none() && !snapshot_changed {
        return Ok(generated);
    }
    std::fs::create_dir_all(&folder)?;

    let existing = sql_file_names(&folder);
    let mut timestamp = Utc::now();
    let mut next_path = |suffix: &str| loop {
        let prefix = timestamp.format("%Y%m%d%H%M%S").to_string();
        timestamp += chrono::Duration::seconds(1);
        if !existing.iter().any(|file| file.starts_with(&prefix)) {
            break folder.join(format!("{prefix}_{suffix}.sql"));
        }
    };

    for migration in missing {
        let path = next_path(migration.name);
        std::fs::write(
            &path,
            format!(
                "{FRAMEWORK_MARKER}{}\n-- Generated by `openshop migrate generate`. Review before applying.\n\n{}",
                migration.id, migration.sql
            ),
        )?;
        generated.files.push(path);
    }
    if !changes.is_empty() {
        let name = name.unwrap_or_else(|| "models".to_string());
        let path = next_path(&name);
        std::fs::write(
            &path,
            format!(
                "-- {name}\n-- Generated by `openshop migrate generate`. Review before applying.\n\n{}",
                diff::render(&changes)
            ),
        )?;
        generated.files.push(path);
        generated.warnings = changes.iter().filter_map(|change| change.warning.clone()).collect();
    } else if let Some(name) = name {
        let path = next_path(&name);
        std::fs::write(&path, format!("-- {name}\n-- Write your schema changes below.\n"))?;
        generated.files.push(path);
    }
    if let Some(json) = snapshot_json.filter(|_| snapshot_changed) {
        std::fs::write(&snapshot_path, json)?;
        generated.snapshot = Some(snapshot_path);
    }
    Ok(generated)
}

/// Fails when migration files are unreadable or do not cover the framework schema.
pub fn check_client_migrations(cwd: &Path) -> Result<()> {
    check_client_migrations_for(cwd, &[])
}

/// The model changes no migration covers yet.
fn pending_model_changes(folder: &Path, models: &[Model]) -> Result<Vec<Change>> {
    let (previous, _) = baseline(folder, models)?;
    Ok(diff::diff(&previous, models))
}

/// Like [`check_client_migrations`], and also fails when the application
/// models changed since the last generated migration.
pub fn check_client_migrations_for(cwd: &Path, models: &[Model]) -> Result<()> {
    let folder = cwd.join(MIGRATIONS_DIR);
    if !missing_framework_migrations(&folder)?.is_empty() {
        bail!(
            "[openshop] The current OpenShop schema is not covered by ./migrations. Run `openshop migrate generate`, review and commit the generated SQL, then run `openshop migrate`."
        );
    }
    let new_models = uncovered_models(&folder, models)?;
    if !new_models.is_empty() && SchemaSnapshot::read(&folder)?.is_none() {
        let tables: Vec<&str> = new_models.iter().map(|model| model.table.as_str()).collect();
        bail!(
            "[openshop] No migration creates the table of these models: {}. Run `openshop migrate generate`, review and commit the generated SQL, then run `openshop migrate`.",
            tables.join(", ")
        );
    }
    let pending = pending_model_changes(&folder, models)?;
    if !pending.is_empty() {
        let changes: Vec<String> = diff::summarize(&pending)
            .into_iter()
            .map(|change| format!("\n  - {change}"))
            .collect();
        bail!(
            "[openshop] The models changed since the last generated migration:{}\nRun `openshop migrate generate`, review and commit the generated SQL, then run `openshop migrate`.",
            changes.concat()
        );
    }
    Ok(())
}

/// Creates the framework tables and application models directly, without
/// migration history. For development and tests only.
pub async fn push_schema(pool: &PgPool, models: &[Model], production: bool) -> Result<()> {
    if production {
        bail!("[openshop] Refusing to push the schema in production. Run `openshop migrate` before starting OpenShop.");
    }
    for migration in FRAMEWORK_MIGRATIONS {
        sqlx::raw_sql(migration.sql).execute(pool).await?;
    }
    for model in models {
        sqlx::raw_sql(&model.create_table_sql()).execute(pool).await?;

        // A table created by an earlier push lacks what the model gained
        // since. What cannot be added is left to a migration.
        let table = quote(&model.table);
        let add_column = |column: &super::model::ColumnDef| format!("ADD COLUMN IF NOT EXISTS {}", column.sql());
        let every_column: Vec<String> = model.columns().iter().map(add_column).collect();
        let together = format!("ALTER TABLE {table} {}", every_column.join(", "));
        let mut statements: Vec<(String, String)> = Vec::new();
        if !every_column.is_empty() && sqlx::raw_sql(&together).execute(pool).await.is_err() {
            // One of them cannot be added: try each, to add the others and name it.
            statements.extend(model.columns().iter().map(|column| {
                (
                    format!("the column {}", column.name),
                    format!("ALTER TABLE {table} {}", add_column(column)),
                )
            }));
        }
        statements.extend(
            model
                .indexes()
                .iter()
                .map(|index| (format!("the index {}", index.name), index.create_sql(&model.table))),
        );
        for (what, statement) in statements {
            if let Err(error) = sqlx::raw_sql(&statement).execute(pool).await {
                tracing::warn!(
                    "[openshop] Could not add {what} to {}: {error}. Generate and apply a migration, or reset the development database.",
                    model.table
                );
            }
        }
    }
    // Foreign keys wait until every table exists, whatever the order of the models.
    for model in models {
        for foreign_key in model.foreign_keys() {
            let statement = foreign_key.add_if_missing_sql(&model.table);
            if let Err(error) = sqlx::raw_sql(&statement).execute(pool).await {
                tracing::warn!(
                    "[openshop] Could not add the foreign key {} to {}: {error}. Check that the referenced model is registered, then generate and apply a migration or reset the development database.",
                    foreign_key.name(&model.table),
                    model.table
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openshop-migrate-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn has_no_folder_until_a_migration_exists() {
        let dir = temp_dir("empty");
        assert_eq!(resolve_client_migrations_folder(&dir), None);
        std::fs::create_dir_all(dir.join(MIGRATIONS_DIR)).unwrap();
        assert_eq!(resolve_client_migrations_folder(&dir), None);
        std::fs::write(dir.join(MIGRATIONS_DIR).join("0001_a.sql"), "SELECT 1;").unwrap();
        assert_eq!(resolve_client_migrations_folder(&dir), Some(dir.join(MIGRATIONS_DIR)));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn generates_the_framework_schema_once() {
        let dir = temp_dir("generate");
        assert!(check_client_migrations(&dir).is_err());

        let created = generate_client_migrations(&dir, &[], false).unwrap();
        assert_eq!(created.len(), FRAMEWORK_MIGRATIONS.len());
        let content = std::fs::read_to_string(&created[0]).unwrap();
        assert!(content.starts_with("-- openshop:framework 0001_openshop_init\n"));
        assert!(content.contains("CREATE TABLE IF NOT EXISTS flow_runs"));
        assert!(
            created[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("_openshop_init.sql")
        );

        check_client_migrations(&dir).unwrap();
        assert!(generate_client_migrations(&dir, &[], false).unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn generates_named_empty_migrations_in_order() {
        let dir = temp_dir("named");
        let created = generate_client_migrations(&dir, &["--name=Add Reviews!".to_string()], false).unwrap();
        assert_eq!(created.len(), 2);
        let names: Vec<String> = created
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names[1].ends_with("_add_reviews.sql"));
        assert!(names[0] < names[1]);

        let files = read_migration_files(&dir.join(MIGRATIONS_DIR)).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].tag, names[0].trim_end_matches(".sql"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn generates_tables_for_models_without_a_migration() {
        use crate::db::define_model;

        let dir = temp_dir("models");
        let models = [
            define_model("reviews").column("rating", "integer NOT NULL"),
            define_model("review_votes")
                .without_shop()
                .column("value", "integer NOT NULL"),
        ];
        generate_client_migrations(&dir, &[], false).unwrap();
        let error = check_client_migrations_for(&dir, &models).unwrap_err().to_string();
        assert!(error.contains("these models: reviews, review_votes"), "{error}");

        let created = generate_client_migrations_for(&dir, &[], false, &models).unwrap();
        assert_eq!(created.len(), 1);
        assert!(created[0].to_string_lossy().ends_with("_models.sql"));
        let sql = std::fs::read_to_string(&created[0]).unwrap();
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS \"reviews\""));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS \"review_votes\""));

        check_client_migrations_for(&dir, &models).unwrap();
        assert!(
            generate_client_migrations_for(&dir, &[], false, &models)
                .unwrap()
                .is_empty()
        );

        // A table created by hand-written SQL counts as covered, whatever its casing or quoting.
        let folder = dir.join(MIGRATIONS_DIR);
        std::fs::write(
            folder.join("99990101000000_orders.sql"),
            "create table orders (id uuid);",
        )
        .unwrap();
        let mut more = models.to_vec();
        more.extend([define_model("orders"), define_model("order_lines")]);
        let created = generate_client_migrations_for(&dir, &["--name=order_lines".to_string()], false, &more).unwrap();
        assert_eq!(created.len(), 1);
        let sql = std::fs::read_to_string(&created[0]).unwrap();
        assert!(sql.contains("\"order_lines\"") && !sql.contains("\"orders\""));
        std::fs::remove_dir_all(dir).ok();
    }

    fn generate(dir: &Path, models: &[Model]) -> GeneratedMigrations {
        generate_migrations(dir, &[], false, models).unwrap()
    }

    fn file_name(path: &Path) -> String {
        path.file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn records_the_models_and_generates_what_changed_since() {
        use crate::db::{ColumnDef, define_model};

        let dir = temp_dir("diff");
        let folder = dir.join(MIGRATIONS_DIR);
        let reviews = define_model("reviews").with_column(ColumnDef::new("rating", "integer"));

        // The first generation creates the table and records the models.
        let first = generate(&dir, std::slice::from_ref(&reviews));
        assert_eq!(first.files.len(), 2);
        assert!(file_name(&first.files[1]).ends_with("_models.sql"));
        assert_eq!(first.snapshot, Some(folder.join(SNAPSHOT_FILE)));
        assert!(first.warnings.is_empty());
        let snapshot = SchemaSnapshot::read(&folder).unwrap().unwrap();
        assert_eq!(snapshot.tables.len(), 1);
        assert_eq!(snapshot.tables[0].name, "reviews");
        check_client_migrations_for(&dir, std::slice::from_ref(&reviews)).unwrap();

        // Nothing changed: nothing is written.
        assert_eq!(
            generate(&dir, std::slice::from_ref(&reviews)),
            GeneratedMigrations::default()
        );

        // A changed model is refused by `check` until a migration covers it.
        let changed = define_model("reviews")
            .with_column(ColumnDef::new("rating", "integer"))
            .with_column(ColumnDef::new("note", "text").nullable(true))
            .with_column(ColumnDef::new("author", "text"))
            .index("reviews_shop_idx", &["shop"]);
        let error = check_client_migrations_for(&dir, std::slice::from_ref(&changed)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "[openshop] The models changed since the last generated migration:\n  \
             - ALTER TABLE \"reviews\" ADD COLUMN IF NOT EXISTS \"note\" text\n  \
             - ALTER TABLE \"reviews\" ADD COLUMN IF NOT EXISTS \"author\" text NOT NULL\n  \
             - CREATE INDEX IF NOT EXISTS \"reviews_shop_idx\" ON \"reviews\" (\"shop\")\n\
             Run `openshop migrate generate`, review and commit the generated SQL, then run `openshop migrate`."
        );

        let second = generate_migrations(
            &dir,
            &["--name=review notes".to_string()],
            false,
            std::slice::from_ref(&changed),
        )
        .unwrap();
        assert_eq!(second.files.len(), 1);
        assert!(file_name(&second.files[0]).ends_with("_review_notes.sql"));
        assert_eq!(second.snapshot, Some(folder.join(SNAPSHOT_FILE)));
        assert_eq!(
            second.warnings,
            [
                "fails if reviews already has rows: author is NOT NULL without a default. Give it a default, or add it as nullable and backfill it first."
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&second.files[0]).unwrap(),
            "-- review_notes\n\
             -- Generated by `openshop migrate generate`. Review before applying.\n\
             \n\
             ALTER TABLE \"reviews\" ADD COLUMN IF NOT EXISTS \"note\" text;\n\
             \n\
             -- WARNING: fails if reviews already has rows: author is NOT NULL without a default. Give it a default, or add it as nullable and backfill it first.\n\
             ALTER TABLE \"reviews\" ADD COLUMN IF NOT EXISTS \"author\" text NOT NULL;\n\
             \n\
             CREATE INDEX IF NOT EXISTS \"reviews_shop_idx\" ON \"reviews\" (\"shop\");\n"
        );
        check_client_migrations_for(&dir, std::slice::from_ref(&changed)).unwrap();
        assert_eq!(
            generate(&dir, std::slice::from_ref(&changed)),
            GeneratedMigrations::default()
        );

        // A model that is no longer registered has its table dropped.
        let third = generate(&dir, &[]);
        assert_eq!(third.warnings, ["drops the table reviews and its data."]);
        assert!(
            std::fs::read_to_string(&third.files[0])
                .unwrap()
                .contains("DROP TABLE IF EXISTS \"reviews\";")
        );
        assert!(SchemaSnapshot::read(&folder).unwrap().unwrap().tables.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_project_without_a_snapshot_starts_from_its_existing_tables() {
        use crate::db::{ColumnDef, define_model};

        // As generated before snapshots existed: a migration creates the table.
        let dir = temp_dir("adopt");
        let folder = dir.join(MIGRATIONS_DIR);
        generate_client_migrations(&dir, &[], false).unwrap();
        let reviews = define_model("reviews").with_column(ColumnDef::new("rating", "integer"));
        std::fs::write(
            folder.join("99990101000000_models.sql"),
            format!("{}\n", reviews.create_sql()),
        )
        .unwrap();
        assert_eq!(SchemaSnapshot::read(&folder).unwrap(), None);
        check_client_migrations_for(&dir, std::slice::from_ref(&reviews)).unwrap();

        // Generating records the models without writing a migration.
        let adopted = generate(&dir, std::slice::from_ref(&reviews));
        assert!(adopted.files.is_empty());
        assert_eq!(adopted.snapshot, Some(folder.join(SNAPSHOT_FILE)));

        // From then on, changes are found by comparison.
        let changed = define_model("reviews")
            .with_column(ColumnDef::new("rating", "bigint").renamed_from("stars"))
            .with_column(ColumnDef::new("note", "text").nullable(true));
        let next = generate(&dir, &[changed]);
        let sql = std::fs::read_to_string(&next.files[0]).unwrap();
        assert!(
            sql.contains("ALTER COLUMN \"rating\" TYPE bigint USING \"rating\"::bigint;"),
            "{sql}"
        );
        assert!(sql.contains("ADD COLUMN IF NOT EXISTS \"note\" text;"), "{sql}");
        assert!(!sql.contains("RENAME"), "{sql}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_damaged_snapshot_is_reported() {
        let dir = temp_dir("damaged");
        let folder = dir.join(MIGRATIONS_DIR);
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(SNAPSHOT_FILE);

        std::fs::write(&path, "{").unwrap();
        let error = generate_migrations(&dir, &[], false, &[]).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "[openshop] {} is not a schema snapshot. Restore it from version control.",
                path.display()
            )
        );

        std::fs::write(&path, r#"{ "version": 2, "tables": [] }"#).unwrap();
        let error = generate_migrations(&dir, &[], false, &[]).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "[openshop] {} has version 2, and this version of OpenShop reads version 1.",
                path.display()
            )
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn refuses_to_generate_in_production_without_the_flag() {
        let dir = temp_dir("production");
        let error = generate_client_migrations(&dir, &[], true).unwrap_err().to_string();
        assert!(error.contains("Refusing to generate migrations in production"));
        let created = generate_client_migrations(&dir, &["--allow-generate-in-production".to_string()], true).unwrap();
        assert_eq!(created.len(), FRAMEWORK_MIGRATIONS.len());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn detects_modified_applied_migrations() {
        let file = MigrationFile {
            tag: "0001_a".into(),
            path: PathBuf::from("0001_a.sql"),
            sql: "SELECT 2;".into(),
            checksum: checksum("SELECT 2;"),
        };
        assert!(!assert_unmodified(&file, &[]).unwrap());
        assert!(assert_unmodified(&file, &[("0001_a".into(), checksum("SELECT 2;"))]).unwrap());
        let error = assert_unmodified(&file, &[("0001_a".into(), checksum("SELECT 1;"))]).unwrap_err();
        assert!(error.to_string().contains("was modified after it was applied"));
    }
}
