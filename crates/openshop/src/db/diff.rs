//! The schema snapshot of the application models, and the SQL that takes a
//! database from one snapshot to the current models.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::model::{ColumnDef, ForeignKeyDef, IndexDef, Model, quote};

/// The file, next to the migrations, that records the models as of the last
/// generated migration.
pub const SNAPSHOT_FILE: &str = "schema.snapshot.json";

const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSnapshot {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexDef>,
    #[serde(rename = "foreignKeys", default, skip_serializing_if = "Vec::is_empty")]
    pub foreign_keys: Vec<ForeignKeyDef>,
}

impl TableSnapshot {
    pub fn of(model: &Model) -> Self {
        Self {
            name: model.table.clone(),
            columns: model.columns().to_vec(),
            indexes: model.indexes().to_vec(),
            foreign_keys: model.foreign_keys().to_vec(),
        }
    }
}

/// The application tables as of the last generated migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaSnapshot {
    pub version: u32,
    pub tables: Vec<TableSnapshot>,
}

impl Default for SchemaSnapshot {
    fn default() -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            tables: Vec::new(),
        }
    }
}

impl SchemaSnapshot {
    pub fn of(models: &[Model]) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            tables: models.iter().map(TableSnapshot::of).collect(),
        }
    }

    /// Reads the snapshot of a migrations folder. `None` when there is none yet.
    pub fn read(folder: &Path) -> Result<Option<Self>> {
        let path = folder.join(SNAPSHOT_FILE);
        if !path.is_file() {
            return Ok(None);
        }
        let json =
            std::fs::read_to_string(&path).with_context(|| format!("[openshop] Failed to read {}", path.display()))?;
        let snapshot: Self = serde_json::from_str(&json).with_context(|| {
            format!(
                "[openshop] {} is not a schema snapshot. Restore it from version control.",
                path.display()
            )
        })?;
        if snapshot.version != SNAPSHOT_VERSION {
            anyhow::bail!(
                "[openshop] {} has version {}, and this version of OpenShop reads version {SNAPSHOT_VERSION}.",
                path.display(),
                snapshot.version
            );
        }
        Ok(Some(snapshot))
    }

    pub fn to_json(&self) -> String {
        let mut json = serde_json::to_string_pretty(self).expect("a snapshot serializes");
        json.push('\n');
        json
    }

    /// The models are the ones the snapshot records.
    pub fn matches(&self, models: &[Model]) -> bool {
        diff(self, models).is_empty()
    }
}

/// One statement of a generated migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub sql: String,
    /// Why the statement deserves a second look before it is applied.
    pub warning: Option<String>,
}

impl Change {
    fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            warning: None,
        }
    }

    fn risky(sql: impl Into<String>, warning: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            warning: Some(warning.into()),
        }
    }

    /// A change that cannot be written automatically: a comment in the migration.
    fn manual(explanation: impl Into<String>) -> Self {
        let explanation = explanation.into();
        Self {
            sql: format!("-- TODO: {explanation}"),
            warning: Some(explanation),
        }
    }
}

/// The statements of a migration, each preceded by its warning.
pub fn render(changes: &[Change]) -> String {
    let mut sql = String::new();
    for change in changes {
        if !sql.is_empty() {
            sql.push('\n');
        }
        match &change.warning {
            Some(warning) if !change.sql.starts_with("-- TODO: ") => {
                sql.push_str(&format!("-- WARNING: {warning}\n{}\n", change.sql));
            }
            _ => {
                sql.push_str(&change.sql);
                sql.push('\n');
            }
        }
    }
    sql
}

/// The statements that take a column from `before` to `after`, the name set aside.
fn alter_column(table: &str, before: &ColumnDef, after: &ColumnDef, changes: &mut Vec<Change>) {
    let target = format!("ALTER TABLE {} ALTER COLUMN {}", quote(table), quote(&after.name));
    let place = format!("{}.{}", table, after.name);

    if before.sql_type != after.sql_type {
        changes.push(Change::risky(
            format!(
                "{target} TYPE {} USING {}::{};",
                after.sql_type,
                quote(&after.name),
                after.sql_type
            ),
            format!(
                "converts the values of {place} from {} to {}. Check that the cast is the one you want.",
                before.sql_type, after.sql_type
            ),
        ));
    }
    if before.default != after.default {
        changes.push(match &after.default {
            Some(default) => Change::new(format!("{target} SET DEFAULT {default};")),
            None => Change::new(format!("{target} DROP DEFAULT;")),
        });
    }
    if before.nullable != after.nullable {
        changes.push(if after.nullable {
            Change::new(format!("{target} DROP NOT NULL;"))
        } else {
            Change::risky(
                format!("{target} SET NOT NULL;"),
                format!("fails while a row has no value for {place}. Backfill it first."),
            )
        });
    }
    if before.primary_key != after.primary_key {
        changes.push(Change::manual(format!(
            "the primary key of {table} changed on {}. Write the constraint change by hand.",
            after.name
        )));
    }
    if before.constraints != after.constraints {
        let show = |constraints: &str| {
            if constraints.is_empty() {
                "none".to_string()
            } else {
                format!("`{constraints}`")
            }
        };
        changes.push(Change::manual(format!(
            "the constraints of {place} changed from {} to {}. Write the statements by hand.",
            show(&before.constraints),
            show(&after.constraints)
        )));
    }
}

/// Old and new names of what was renamed.
type Renames<'a> = Vec<(&'a str, &'a str)>;

fn renamed<'a>(renames: &Renames<'a>, name: &'a str) -> &'a str {
    renames
        .iter()
        .find(|(old, _)| *old == name)
        .map_or(name, |(_, new)| new)
}

/// The statements that take a table from `before` to the model, under its
/// current name. The foreign keys to add go to `added_foreign_keys`: they wait
/// until every table exists.
fn alter_table<'a>(
    before: &'a TableSnapshot,
    model: &'a Model,
    table_renames: &Renames<'a>,
    changes: &mut Vec<Change>,
    added_foreign_keys: &mut Vec<Change>,
) {
    let table = quote(&model.table);
    let mut renames = Vec::new();
    let mut additions = Vec::new();
    let mut alterations = Vec::new();
    let mut kept: Vec<&str> = Vec::new();
    let mut column_renames: Renames = Vec::new();

    for column in model.columns() {
        let same_name = before.columns.iter().find(|existing| existing.name == column.name);
        let previous = same_name.or_else(|| {
            // A rename, unless the previous name is still a column of the model.
            let previous = column.renamed_from.as_deref()?;
            let still_used = model.columns().iter().any(|other| other.name == previous);
            before
                .columns
                .iter()
                .find(|existing| existing.name == previous && !still_used)
        });
        match previous {
            Some(previous) => {
                kept.push(&previous.name);
                if previous.name != column.name {
                    column_renames.push((&previous.name, &column.name));
                    renames.push(Change::new(format!(
                        "ALTER TABLE {table} RENAME COLUMN {} TO {};",
                        quote(&previous.name),
                        quote(&column.name)
                    )));
                }
                alter_column(&model.table, previous, column, &mut alterations);
            }
            None => {
                let sql = format!("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS {};", column.sql());
                additions.push(if !column.nullable && column.default.is_none() {
                    Change::risky(
                        sql,
                        format!(
                            "fails if {} already has rows: {} is NOT NULL without a default. Give it a default, or add it as nullable and backfill it first.",
                            model.table, column.name
                        ),
                    )
                } else {
                    Change::new(sql)
                });
            }
        }
    }

    let removed_columns = before
        .columns
        .iter()
        .filter(|column| !kept.contains(&column.name.as_str()))
        .map(|column| {
            Change::risky(
                format!("ALTER TABLE {table} DROP COLUMN IF EXISTS {};", quote(&column.name)),
                format!("drops {}.{} and its data.", model.table, column.name),
            )
        });

    // A foreign key is the same link when its column and its target are, renames followed.
    let same_link = |previous: &ForeignKeyDef, current: &ForeignKeyDef| {
        renamed(&column_renames, &previous.column) == current.column
            && renamed(table_renames, &previous.table) == current.table
            && previous.references == current.references
    };
    let mut kept_foreign_keys: Vec<&ForeignKeyDef> = Vec::new();
    let mut dropped_foreign_keys = Vec::new();
    for foreign_key in model.foreign_keys() {
        let link = before
            .foreign_keys
            .iter()
            .find(|previous| same_link(previous, foreign_key));
        match link {
            Some(previous) if previous.on_delete == foreign_key.on_delete => {
                kept_foreign_keys.push(previous);
                // The constraint kept the name it was created with.
                let (old, new) = (previous.name(&before.name), foreign_key.name(&model.table));
                if old != new {
                    renames.push(Change::new(format!(
                        "ALTER TABLE {table} RENAME CONSTRAINT {} TO {};",
                        quote(&old),
                        quote(&new)
                    )));
                }
            }
            // The same link with another action: the rows already satisfy it.
            Some(_) => added_foreign_keys.push(Change::new(foreign_key.add_sql(&model.table))),
            None => added_foreign_keys.push(Change::risky(
                foreign_key.add_sql(&model.table),
                format!(
                    "fails if a row of {} has a {} that is not the {} of a row of {}.",
                    model.table, foreign_key.column, foreign_key.references, foreign_key.table
                ),
            )),
        }
    }
    for previous in &before.foreign_keys {
        if !kept_foreign_keys.iter().any(|kept| std::ptr::eq(*kept, previous)) {
            dropped_foreign_keys.push(Change::new(format!(
                "ALTER TABLE {table} DROP CONSTRAINT IF EXISTS {};",
                quote(&previous.name(&before.name))
            )));
        }
    }

    let mut dropped_indexes = Vec::new();
    let mut created_indexes = Vec::new();
    for index in model.indexes() {
        match before.indexes.iter().find(|existing| existing.name == index.name) {
            Some(existing) if existing == index => {}
            existing => {
                if existing.is_some() {
                    dropped_indexes.push(Change::new(format!("DROP INDEX IF EXISTS {};", quote(&index.name))));
                }
                let sql = index.create_sql(&model.table);
                created_indexes.push(if index.unique {
                    Change::risky(
                        sql,
                        format!(
                            "fails if two rows of {} have the same ({}).",
                            model.table,
                            index.columns.join(", ")
                        ),
                    )
                } else {
                    Change::new(sql)
                });
            }
        }
    }
    for index in &before.indexes {
        if !model.indexes().iter().any(|kept| kept.name == index.name) {
            dropped_indexes.push(Change::new(format!("DROP INDEX IF EXISTS {};", quote(&index.name))));
        }
    }

    changes.extend(renames);
    changes.extend(additions);
    changes.extend(alterations);
    changes.extend(dropped_foreign_keys);
    changes.extend(dropped_indexes);
    changes.extend(removed_columns);
    changes.extend(created_indexes);
}

/// The table a model continues: the one of the same name, or the one it was renamed from.
fn table_before<'a>(previous: &'a SchemaSnapshot, models: &[Model], model: &Model) -> Option<&'a TableSnapshot> {
    let same_name = previous.tables.iter().find(|table| table.name == model.table);
    same_name.or_else(|| {
        let renamed_from = model.previous_name()?;
        let still_used = models.iter().any(|other| other.table == renamed_from);
        previous
            .tables
            .iter()
            .find(|table| table.name == renamed_from && !still_used)
    })
}

/// The statements that take a database matching `previous` to `models`.
///
/// Tables and columns are matched by name, then by the previous name given
/// with `renamed_from`. What matches nothing is created or dropped. Foreign
/// keys are added after every table exists, and tables are dropped starting
/// with those that reference others.
pub fn diff(previous: &SchemaSnapshot, models: &[Model]) -> Vec<Change> {
    let matched: Vec<(&Model, Option<&TableSnapshot>)> = models
        .iter()
        .map(|model| (model, table_before(previous, models, model)))
        .collect();
    let table_renames: Renames = matched
        .iter()
        .filter_map(|(model, before)| Some((before.as_ref()?.name.as_str(), model.table.as_str())))
        .filter(|(old, new)| old != new)
        .collect();

    let mut changes = Vec::new();
    let mut added_foreign_keys = Vec::new();
    for (model, before) in &matched {
        match before {
            Some(before) => {
                if before.name != model.table {
                    changes.push(Change::new(format!(
                        "ALTER TABLE {} RENAME TO {};",
                        quote(&before.name),
                        quote(&model.table)
                    )));
                }
                alter_table(before, model, &table_renames, &mut changes, &mut added_foreign_keys);
            }
            None => {
                changes.push(Change::new(model.create_sql()));
                // A new table has no row that could break its foreign keys.
                added_foreign_keys.extend(
                    model
                        .foreign_keys()
                        .iter()
                        .map(|foreign_key| Change::new(foreign_key.add_sql(&model.table))),
                );
            }
        }
    }
    changes.extend(added_foreign_keys);

    // A table is dropped once no other table to drop references it.
    let mut dropped: Vec<&TableSnapshot> = previous
        .tables
        .iter()
        .filter(|table| {
            !matched
                .iter()
                .any(|(_, before)| before.is_some_and(|before| std::ptr::eq(before, *table)))
        })
        .collect();
    while !dropped.is_empty() {
        let free = dropped.iter().position(|table| {
            !dropped.iter().any(|other| {
                !std::ptr::eq(*other, *table)
                    && other
                        .foreign_keys
                        .iter()
                        .any(|foreign_key| foreign_key.table == table.name)
            })
        });
        // Tables that reference each other are dropped in the order of the snapshot.
        let table = dropped.remove(free.unwrap_or(0));
        changes.push(Change::risky(
            format!("DROP TABLE IF EXISTS {};", quote(&table.name)),
            format!("drops the table {} and its data.", table.name),
        ));
    }
    changes
}

/// A short description of each change, for messages.
pub fn summarize(changes: &[Change]) -> Vec<String> {
    changes
        .iter()
        .map(|change| {
            let first_line = change.sql.lines().next().unwrap_or_default();
            first_line
                .trim_start_matches("-- TODO: ")
                .trim_end_matches([';', '('])
                .trim()
                .to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::define_model;

    fn reviews() -> Model {
        define_model("reviews")
            .with_column(ColumnDef::new("product_id", "text"))
            .with_column(ColumnDef::new("rating", "integer"))
            .index("reviews_shop_product_idx", &["shop", "product_id"])
    }

    fn sql(previous: &[Model], current: &[Model]) -> String {
        render(&diff(&SchemaSnapshot::of(previous), current))
    }

    #[test]
    fn identical_models_need_no_statement() {
        assert!(diff(&SchemaSnapshot::of(&[reviews()]), &[reviews()]).is_empty());
        assert!(SchemaSnapshot::of(&[reviews()]).matches(&[reviews()]));
        // A definition written as a string compares on what it means.
        let written = define_model("reviews")
            .column("product_id", "TEXT not null")
            .column("rating", "int NOT NULL")
            .index("reviews_shop_product_idx", &["shop", "product_id"]);
        assert!(diff(&SchemaSnapshot::of(&[reviews()]), &[written]).is_empty());
    }

    #[test]
    fn new_tables_are_created_and_removed_ones_dropped() {
        let votes = define_model("review_votes").with_column(ColumnDef::new("value", "integer"));
        assert_eq!(
            sql(&[reviews()], &[reviews(), votes.clone()]),
            format!("{}\n", votes.create_sql())
        );
        assert_eq!(
            sql(&[reviews(), votes], &[reviews()]),
            "-- WARNING: drops the table review_votes and its data.\nDROP TABLE IF EXISTS \"review_votes\";\n"
        );
    }

    #[test]
    fn added_columns_warn_when_they_cannot_be_filled() {
        let current = reviews()
            .with_column(ColumnDef::new("note", "text").nullable(true))
            .with_column(ColumnDef::new("approved", "boolean").default("false"))
            .with_column(ColumnDef::new("author", "text"));
        assert_eq!(
            sql(&[reviews()], &[current]),
            "ALTER TABLE \"reviews\" ADD COLUMN IF NOT EXISTS \"note\" text;\n\
             \n\
             ALTER TABLE \"reviews\" ADD COLUMN IF NOT EXISTS \"approved\" boolean DEFAULT false NOT NULL;\n\
             \n\
             -- WARNING: fails if reviews already has rows: author is NOT NULL without a default. Give it a default, or add it as nullable and backfill it first.\n\
             ALTER TABLE \"reviews\" ADD COLUMN IF NOT EXISTS \"author\" text NOT NULL;\n"
        );
    }

    #[test]
    fn removed_columns_are_dropped_with_a_warning() {
        let previous = reviews().with_column(ColumnDef::new("legacy", "text").nullable(true));
        assert_eq!(
            sql(&[previous], &[reviews()]),
            "-- WARNING: drops reviews.legacy and its data.\nALTER TABLE \"reviews\" DROP COLUMN IF EXISTS \"legacy\";\n"
        );
    }

    #[test]
    fn changed_columns_are_altered_in_place() {
        let previous = define_model("reviews")
            .with_column(ColumnDef::new("rating", "integer"))
            .with_column(ColumnDef::new("note", "text").nullable(true).default("''"))
            .with_column(ColumnDef::new("approved", "boolean"));
        let current = define_model("reviews")
            .with_column(ColumnDef::new("rating", "bigint").nullable(true))
            .with_column(ColumnDef::new("note", "text"))
            .with_column(ColumnDef::new("approved", "boolean").default("false"));
        assert_eq!(
            sql(&[previous], &[current]),
            "-- WARNING: converts the values of reviews.rating from integer to bigint. Check that the cast is the one you want.\n\
             ALTER TABLE \"reviews\" ALTER COLUMN \"rating\" TYPE bigint USING \"rating\"::bigint;\n\
             \n\
             ALTER TABLE \"reviews\" ALTER COLUMN \"rating\" DROP NOT NULL;\n\
             \n\
             ALTER TABLE \"reviews\" ALTER COLUMN \"note\" DROP DEFAULT;\n\
             \n\
             -- WARNING: fails while a row has no value for reviews.note. Backfill it first.\n\
             ALTER TABLE \"reviews\" ALTER COLUMN \"note\" SET NOT NULL;\n\
             \n\
             ALTER TABLE \"reviews\" ALTER COLUMN \"approved\" SET DEFAULT false;\n"
        );
    }

    #[test]
    fn renames_need_the_previous_name() {
        let previous = define_model("reviews").with_column(ColumnDef::new("stars", "integer"));

        // Without it, a rename is a column dropped and another added.
        let guessed = define_model("reviews").with_column(ColumnDef::new("rating", "integer"));
        let statements = sql(std::slice::from_ref(&previous), &[guessed]);
        assert!(
            statements.contains("ADD COLUMN IF NOT EXISTS \"rating\""),
            "{statements}"
        );
        assert!(statements.contains("DROP COLUMN IF EXISTS \"stars\""), "{statements}");

        let renamed = define_model("product_reviews")
            .renamed_from("reviews")
            .with_column(ColumnDef::new("rating", "bigint").renamed_from("stars"));
        assert_eq!(
            sql(std::slice::from_ref(&previous), std::slice::from_ref(&renamed)),
            "ALTER TABLE \"reviews\" RENAME TO \"product_reviews\";\n\
             \n\
             ALTER TABLE \"product_reviews\" RENAME COLUMN \"stars\" TO \"rating\";\n\
             \n\
             -- WARNING: converts the values of product_reviews.rating from integer to bigint. Check that the cast is the one you want.\n\
             ALTER TABLE \"product_reviews\" ALTER COLUMN \"rating\" TYPE bigint USING \"rating\"::bigint;\n"
        );

        // Once the snapshot has the new names, `renamed_from` no longer does anything.
        assert!(diff(&SchemaSnapshot::of(std::slice::from_ref(&renamed)), &[renamed]).is_empty());
    }

    #[test]
    fn indexes_are_created_dropped_and_replaced() {
        let previous = reviews().index("reviews_rating_idx", &["rating"]);
        let current = define_model("reviews")
            .with_column(ColumnDef::new("product_id", "text"))
            .with_column(ColumnDef::new("rating", "integer"))
            .index("reviews_shop_product_idx", &["shop", "product_id", "rating"])
            .unique_index("reviews_shop_product_unique", &["shop", "product_id"]);
        assert_eq!(
            sql(&[previous], &[current]),
            "DROP INDEX IF EXISTS \"reviews_shop_product_idx\";\n\
             \n\
             DROP INDEX IF EXISTS \"reviews_rating_idx\";\n\
             \n\
             CREATE INDEX IF NOT EXISTS \"reviews_shop_product_idx\" ON \"reviews\" (\"shop\", \"product_id\", \"rating\");\n\
             \n\
             -- WARNING: fails if two rows of reviews have the same (shop, product_id).\n\
             CREATE UNIQUE INDEX IF NOT EXISTS \"reviews_shop_product_unique\" ON \"reviews\" (\"shop\", \"product_id\");\n"
        );
    }

    fn votes() -> Model {
        define_model("review_votes")
            .with_column(ColumnDef::new("review_id", "uuid"))
            .with_foreign_key(ForeignKeyDef::new("review_id", "reviews").on_delete("cascade"))
    }

    #[test]
    fn foreign_keys_of_new_tables_wait_for_every_table() {
        // The referencing model is registered first: its table is still created
        // before the constraint is added.
        let statements = sql(&[], &[votes(), reviews()]);
        let position = |needle: &str| {
            statements
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} in {statements}"))
        };
        let constraint = "ALTER TABLE \"review_votes\" ADD CONSTRAINT \"review_votes_review_id_fkey\" \
                          FOREIGN KEY (\"review_id\") REFERENCES \"reviews\" (\"id\") ON DELETE CASCADE;";
        assert!(position("CREATE TABLE IF NOT EXISTS \"review_votes\"") < position(constraint));
        assert!(position("CREATE TABLE IF NOT EXISTS \"reviews\"") < position(constraint));
        assert!(!statements.contains("WARNING"), "{statements}");

        assert!(diff(&SchemaSnapshot::of(&[votes(), reviews()]), &[votes(), reviews()]).is_empty());
    }

    #[test]
    fn foreign_keys_are_added_changed_and_dropped_on_existing_tables() {
        let plain = define_model("review_votes").with_column(ColumnDef::new("review_id", "uuid"));

        assert_eq!(
            sql(&[reviews(), plain.clone()], &[reviews(), votes()]),
            "-- WARNING: fails if a row of review_votes has a review_id that is not the id of a row of reviews.\n\
             ALTER TABLE \"review_votes\" ADD CONSTRAINT \"review_votes_review_id_fkey\" \
             FOREIGN KEY (\"review_id\") REFERENCES \"reviews\" (\"id\") ON DELETE CASCADE;\n"
        );

        // Another action on the same link: the constraint is replaced, without warning.
        let restricted = define_model("review_votes")
            .with_column(ColumnDef::new("review_id", "uuid"))
            .with_foreign_key(ForeignKeyDef::new("review_id", "reviews").on_delete("restrict"));
        assert_eq!(
            sql(&[reviews(), votes()], &[reviews(), restricted]),
            "ALTER TABLE \"review_votes\" DROP CONSTRAINT IF EXISTS \"review_votes_review_id_fkey\";\n\
             \n\
             ALTER TABLE \"review_votes\" ADD CONSTRAINT \"review_votes_review_id_fkey\" \
             FOREIGN KEY (\"review_id\") REFERENCES \"reviews\" (\"id\") ON DELETE RESTRICT;\n"
        );

        assert_eq!(
            sql(&[reviews(), votes()], &[reviews(), plain]),
            "ALTER TABLE \"review_votes\" DROP CONSTRAINT IF EXISTS \"review_votes_review_id_fkey\";\n"
        );
    }

    #[test]
    fn renames_keep_foreign_keys_and_rename_their_constraints() {
        let renamed_reviews = define_model("product_reviews")
            .renamed_from("reviews")
            .with_column(ColumnDef::new("product_id", "text"))
            .with_column(ColumnDef::new("rating", "integer"))
            .index("reviews_shop_product_idx", &["shop", "product_id"]);
        let renamed_votes = define_model("votes")
            .renamed_from("review_votes")
            .with_column(ColumnDef::new("target_id", "uuid").renamed_from("review_id"))
            .with_foreign_key(ForeignKeyDef::new("target_id", "product_reviews").on_delete("cascade"));
        assert_eq!(
            sql(&[reviews(), votes()], &[renamed_reviews, renamed_votes]),
            "ALTER TABLE \"reviews\" RENAME TO \"product_reviews\";\n\
             \n\
             ALTER TABLE \"review_votes\" RENAME TO \"votes\";\n\
             \n\
             ALTER TABLE \"votes\" RENAME COLUMN \"review_id\" TO \"target_id\";\n\
             \n\
             ALTER TABLE \"votes\" RENAME CONSTRAINT \"review_votes_review_id_fkey\" TO \"votes_target_id_fkey\";\n"
        );
    }

    #[test]
    fn tables_that_reference_others_are_dropped_first() {
        let replies = define_model("vote_replies")
            .with_column(ColumnDef::new("vote_id", "uuid"))
            .with_foreign_key(ForeignKeyDef::new("vote_id", "review_votes"));
        let statements = sql(&[reviews(), votes(), replies], &[]);
        let position = |table: &str| statements.find(&format!("DROP TABLE IF EXISTS \"{table}\";")).unwrap();
        assert!(position("vote_replies") < position("review_votes"), "{statements}");
        assert!(position("review_votes") < position("reviews"), "{statements}");

        // A column dropped with its foreign key loses the constraint first.
        let without = define_model("review_votes");
        assert_eq!(
            sql(&[reviews(), votes()], &[reviews(), without]),
            "ALTER TABLE \"review_votes\" DROP CONSTRAINT IF EXISTS \"review_votes_review_id_fkey\";\n\
             \n\
             -- WARNING: drops review_votes.review_id and its data.\n\
             ALTER TABLE \"review_votes\" DROP COLUMN IF EXISTS \"review_id\";\n"
        );
    }

    #[test]
    fn constraints_that_changed_are_left_to_write() {
        let previous = define_model("reviews").column("product_id", "text NOT NULL");
        let current = define_model("reviews").column("product_id", "text NOT NULL REFERENCES products(id)");
        let changes = diff(&SchemaSnapshot::of(&[previous]), &[current]);
        assert_eq!(
            render(&changes),
            "-- TODO: the constraints of reviews.product_id changed from none to `REFERENCES products(id)`. Write the statements by hand.\n"
        );
        assert_eq!(
            summarize(&changes),
            [
                "the constraints of reviews.product_id changed from none to `REFERENCES products(id)`. Write the statements by hand."
            ]
        );
    }

    #[test]
    fn snapshots_round_trip_as_readable_json() {
        let snapshot = SchemaSnapshot::of(&[reviews()]);
        let json = snapshot.to_json();
        assert!(json.ends_with("}\n"));
        assert!(json.contains("\"name\": \"reviews\""));
        assert_eq!(serde_json::from_str::<SchemaSnapshot>(&json).unwrap(), snapshot);
    }
}
