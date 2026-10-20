//! The schema snapshot of the application models, and the SQL that takes a
//! database from one snapshot to the current models.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::model::{ColumnDef, ForeignKeyDef, Table, quote};

/// The file, next to the migrations, that records the models as of the last
/// generated migration.
pub const SNAPSHOT_FILE: &str = "schema.snapshot.json";

const SNAPSHOT_VERSION: u32 = 1;

/// The application tables as of the last generated migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaSnapshot {
    pub version: u32,
    pub tables: Vec<Table>,
}

impl Default for SchemaSnapshot {
    fn default() -> Self {
        Self::of(&[])
    }
}

impl SchemaSnapshot {
    pub fn of(models: &[Table]) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            tables: models.to_vec(),
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
}

/// One step of a generated migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A statement, with the reason it deserves a second look before it is
    /// applied, when there is one. `destructive` marks a statement that loses
    /// data the database holds.
    Statement {
        sql: String,
        warning: Option<String>,
        destructive: bool,
    },
    /// A change that cannot be written automatically: what to write by hand.
    Manual(String),
}

impl Change {
    fn new(sql: impl Into<String>) -> Self {
        Self::Statement {
            sql: sql.into(),
            warning: None,
            destructive: false,
        }
    }

    fn risky(sql: impl Into<String>, warning: impl Into<String>) -> Self {
        Self::Statement {
            sql: sql.into(),
            warning: Some(warning.into()),
            destructive: false,
        }
    }

    /// A statement that loses data: `migrate generate` refuses it without `--allow-destructive`.
    fn destructive(sql: impl Into<String>, warning: impl Into<String>) -> Self {
        Self::Statement {
            sql: sql.into(),
            warning: Some(warning.into()),
            destructive: true,
        }
    }

    /// Whether applying the change loses data.
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            Self::Statement {
                destructive: true,
                ..
            }
        )
    }

    /// What to review before the migration is applied.
    pub fn warning(&self) -> Option<&str> {
        match self {
            Self::Statement { warning, .. } => warning.as_deref(),
            Self::Manual(explanation) => Some(explanation),
        }
    }

    /// The first line of the change, without its terminator: for messages.
    fn summary(&self) -> &str {
        match self {
            Self::Statement { sql, .. } => sql
                .lines()
                .next()
                .unwrap_or_default()
                .trim_end_matches([';', '('])
                .trim(),
            Self::Manual(explanation) => explanation,
        }
    }
}

/// The line of a change to write by hand opens with this, then its explanation.
pub const MANUAL_TODO: &str = "-- TODO: ";

/// The statements of a migration, each preceded by its warning.
pub fn render(changes: &[Change]) -> String {
    let rendered: Vec<String> = changes
        .iter()
        .map(|change| match change {
            Change::Statement {
                sql,
                warning: Some(warning),
                ..
            } => format!("-- WARNING: {warning}\n{sql}\n"),
            Change::Statement { sql, warning: None, .. } => format!("{sql}\n"),
            Change::Manual(explanation) => format!("{MANUAL_TODO}{explanation}\n"),
        })
        .collect();
    rendered.join("\n")
}

/// A short description of each change, for messages.
pub fn summarize(changes: &[Change]) -> Vec<String> {
    changes.iter().map(|change| change.summary().to_string()).collect()
}

/// What each change that loses data does, for messages.
pub fn destructive(changes: &[Change]) -> Vec<String> {
    changes
        .iter()
        .filter(|change| change.is_destructive())
        .map(|change| match change.warning() {
            Some(warning) => format!("{}: {warning}", change.summary()),
            None => change.summary().to_string(),
        })
        .collect()
}

/// A type split into its name, its numeric modifiers and the array marks that follow.
struct ParsedType<'a> {
    base: &'a str,
    /// `None` without parentheses.
    modifiers: Option<Vec<u32>>,
    dimensions: &'a str,
}

/// Splits `base(1, 2)[]`. `None` for anything else, such as `timestamp(3) with
/// time zone`, so that what is not understood is never taken for a widening.
fn parse_type(sql_type: &str) -> Option<ParsedType<'_>> {
    let (head, dimensions) = match sql_type.find('[') {
        Some(index) => sql_type.split_at(index),
        None => (sql_type, ""),
    };
    let marks_ok = dimensions
        .split_inclusive(']')
        .all(|mark| mark.starts_with('[') && mark.ends_with(']'));
    if !dimensions.is_empty() && !marks_ok {
        return None;
    }
    let (base, modifiers) = match head.split_once('(') {
        Some((base, rest)) => {
            let inside = rest.strip_suffix(')')?;
            let modifiers = inside
                .split(',')
                .map(|modifier| modifier.trim().parse::<u32>().ok())
                .collect::<Option<Vec<u32>>>()?;
            (base.trim(), Some(modifiers))
        }
        None => (head.trim(), None),
    };
    Some(ParsedType {
        base,
        modifiers,
        dimensions,
    })
}

/// Whether every value of the type `before` fits the type `after`, so that
/// converting the column loses nothing. Only what is proven to fit counts: a
/// type that is not understood loses data until shown otherwise.
fn widens(before: &str, after: &str) -> bool {
    let (Some(before), Some(after)) = (parse_type(before), parse_type(after)) else {
        return false;
    };
    // The same array dimensions, and the element widens. `text[]` does not become `text`.
    if before.dimensions != after.dimensions {
        return false;
    }
    let rank = |base: &str| match base {
        "smallint" => Some(1),
        "integer" => Some(2),
        "bigint" => Some(3),
        _ => None,
    };
    match (before.base, after.base) {
        (a, b) if rank(a).is_some() && rank(b).is_some() => {
            before.modifiers.is_none() && after.modifiers.is_none() && rank(a) < rank(b)
        }
        ("real", "double precision") => before.modifiers.is_none() && after.modifiers.is_none(),
        ("character varying" | "character" | "char", "text") => after.modifiers.is_none(),
        ("character varying", "character varying") => match (&before.modifiers, &after.modifiers) {
            (_, None) => true,
            (Some(before), Some(after)) => before.len() == 1 && after.len() == 1 && before[0] <= after[0],
            (None, Some(_)) => false,
        },
        ("numeric", "numeric") => match (&before.modifiers, &after.modifiers) {
            (_, None) => true,
            // Same scale, room for at least as many digits before the point.
            (Some(before), Some(after)) => {
                let scale = |modifiers: &[u32]| modifiers.get(1).copied().unwrap_or(0);
                matches!((before.len(), after.len()), (1..=2, 1..=2))
                    && scale(before) == scale(after)
                    && before[0] <= after[0]
            }
            (None, Some(_)) => false,
        },
        _ => false,
    }
}

/// What `current` continues among `before`: the item of the same name, else
/// the one it was renamed from, unless that name is still in use.
fn continued<'a, T>(
    before: &'a [T],
    name_of: impl Fn(&T) -> &str,
    name: &str,
    renamed_from: Option<&str>,
    still_used: impl Fn(&str) -> bool,
) -> Option<&'a T> {
    let same_name = before.iter().find(|item| name_of(item) == name);
    same_name.or_else(|| {
        let previous = renamed_from.filter(|previous| !still_used(previous))?;
        before.iter().find(|item| name_of(item) == previous)
    })
}

/// Old and new names of what was renamed.
type Renames<'a> = Vec<(&'a str, &'a str)>;

fn renamed<'a>(renames: &Renames<'a>, name: &'a str) -> &'a str {
    renames
        .iter()
        .find(|(old, _)| *old == name)
        .map_or(name, |(_, new)| new)
}

/// The statements that take a column from `before` to `after`, the name set aside.
fn alter_column(table: &str, before: &ColumnDef, after: &ColumnDef, changes: &mut Vec<Change>) {
    let target = format!("ALTER TABLE {} ALTER COLUMN {}", quote(table), quote(&after.name));
    let place = format!("{}.{}", table, after.name);

    if before.sql_type != after.sql_type {
        let sql = format!(
            "{target} TYPE {} USING {}::{};",
            after.sql_type,
            quote(&after.name),
            after.sql_type
        );
        let warning = format!(
            "converts the values of {place} from {} to {}. Check that the cast is the one you want.",
            before.sql_type, after.sql_type
        );
        // A conversion that can truncate or round values loses data.
        changes.push(if widens(&before.sql_type, &after.sql_type) {
            Change::risky(sql, warning)
        } else {
            Change::destructive(sql, warning)
        });
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
        changes.push(Change::Manual(primary_key_note(table, after)));
    }
    if before.constraints != after.constraints {
        changes.push(Change::Manual(constraints_note(&place, before, after)));
    }
}

fn primary_key_note(table: &str, after: &ColumnDef) -> String {
    let now = if after.primary_key {
        "is now part of the primary key"
    } else {
        "is no longer part of the primary key"
    };
    format!(
        "the primary key of {table} changed: {} {now}. Write the constraint change by hand.",
        after.name
    )
}

fn constraints_note(place: &str, before: &ColumnDef, after: &ColumnDef) -> String {
    let show = |constraints: &str| {
        if constraints.is_empty() {
            "none".to_string()
        } else {
            format!("`{constraints}`")
        }
    };
    format!(
        "the constraints of {place} changed from {} to {}. Write the statements by hand.",
        show(&before.constraints),
        show(&after.constraints)
    )
}

/// The changes that remain once the changes to write by hand that `skip`
/// accepts, by their explanation, are left out.
pub fn without_manual(changes: Vec<Change>, skip: impl Fn(&str) -> bool) -> Vec<Change> {
    changes
        .into_iter()
        .filter(|change| match change {
            Change::Manual(explanation) => !skip(explanation),
            Change::Statement { .. } => true,
        })
        .collect()
}

/// The snapshot to record for `models`: the models, except that a primary key
/// or constraints that changed keep their previous state, unless `accepted`:
/// the author said the migrations write them. Recording them earlier would make
/// the models and the database look in line while the database was not.
pub fn recorded(previous: &SchemaSnapshot, models: &[Table], accepted: bool) -> SchemaSnapshot {
    let mut snapshot = SchemaSnapshot::of(models);
    if accepted {
        return snapshot;
    }
    for table in &mut snapshot.tables {
        let Some(before) = continued(
            &previous.tables,
            |before| &before.table,
            &table.table,
            table.previous_name(),
            |name| models.iter().any(|other| other.table == name),
        ) else {
            continue;
        };
        let names = table.columns().to_vec();
        for (column, current) in table.columns_mut().iter_mut().zip(&names) {
            let Some(was) = continued(
                before.columns(),
                |column| &column.name,
                &current.name,
                current.renamed_from.as_deref(),
                |name| names.iter().any(|other| other.name == name),
            ) else {
                continue;
            };
            if was.primary_key != column.primary_key {
                column.primary_key = was.primary_key;
            }
            if was.constraints != column.constraints {
                column.constraints = was.constraints.clone();
            }
        }
    }
    snapshot
}

/// The changes of one table, grouped by the place they take in the migration.
#[derive(Default)]
struct TableChanges {
    renames: Vec<Change>,
    additions: Vec<Change>,
    alterations: Vec<Change>,
    dropped_constraints: Vec<Change>,
    dropped_columns: Vec<Change>,
    created_indexes: Vec<Change>,
    /// Foreign keys wait until every table of the migration exists.
    added_foreign_keys: Vec<Change>,
}

/// Compares the columns. Returns the old and new names of those renamed.
fn diff_columns<'a>(before: &'a Table, model: &'a Table, changes: &mut TableChanges) -> Renames<'a> {
    let table = quote(&model.table);
    let mut kept: Vec<&str> = Vec::new();
    let mut renames: Renames = Vec::new();

    for column in model.columns() {
        let previous = continued(
            before.columns(),
            |column| &column.name,
            &column.name,
            column.renamed_from.as_deref(),
            |name| model.columns().iter().any(|other| other.name == name),
        );
        let Some(previous) = previous else {
            let sql = column.add_sql(&model.table);
            changes.additions.push(if !column.nullable && column.default.is_none() {
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
            continue;
        };
        kept.push(&previous.name);
        if previous.name != column.name {
            renames.push((&previous.name, &column.name));
            changes.renames.push(Change::new(format!(
                "ALTER TABLE {table} RENAME COLUMN {} TO {};",
                quote(&previous.name),
                quote(&column.name)
            )));
        }
        alter_column(&model.table, previous, column, &mut changes.alterations);
    }

    for column in before.columns() {
        if !kept.contains(&column.name.as_str()) {
            changes.dropped_columns.push(Change::destructive(
                format!("ALTER TABLE {table} DROP COLUMN IF EXISTS {};", quote(&column.name)),
                format!("drops {}.{} and its data.", model.table, column.name),
            ));
        }
    }
    renames
}

/// Compares the foreign keys. A foreign key is the same link when its column
/// and its target are, renames followed.
fn diff_foreign_keys(
    before: &Table,
    model: &Table,
    column_renames: &Renames,
    table_renames: &Renames,
    changes: &mut TableChanges,
) {
    let table = quote(&model.table);
    let same_link = |previous: &ForeignKeyDef, current: &ForeignKeyDef| {
        renamed(column_renames, &previous.column) == current.column
            && renamed(table_renames, &previous.table) == current.table
            && previous.references == current.references
    };

    let mut kept: Vec<&ForeignKeyDef> = Vec::new();
    for foreign_key in model.foreign_keys() {
        let link = before
            .foreign_keys()
            .iter()
            .find(|previous| same_link(previous, foreign_key));
        match link {
            Some(previous) if previous.on_delete == foreign_key.on_delete => {
                kept.push(previous);
                // The constraint kept the name it was created with.
                let (old, new) = (previous.name(&before.table), foreign_key.name(&model.table));
                if old != new {
                    changes.renames.push(Change::new(format!(
                        "ALTER TABLE {table} RENAME CONSTRAINT {} TO {};",
                        quote(&old),
                        quote(&new)
                    )));
                }
            }
            // The same link with another action: the rows already satisfy it.
            Some(_) => changes
                .added_foreign_keys
                .push(Change::new(foreign_key.add_sql(&model.table))),
            None => changes.added_foreign_keys.push(Change::risky(
                foreign_key.add_sql(&model.table),
                format!(
                    "fails if a row of {} has a {} that is not the {} of a row of {}.",
                    model.table, foreign_key.column, foreign_key.references, foreign_key.table
                ),
            )),
        }
    }
    for previous in before.foreign_keys() {
        if !kept.iter().any(|kept| std::ptr::eq(*kept, previous)) {
            changes.dropped_constraints.push(Change::new(format!(
                "ALTER TABLE {table} DROP CONSTRAINT IF EXISTS {};",
                quote(&previous.name(&before.table))
            )));
        }
    }
}

/// Compares the indexes: one that changed is dropped and created again.
fn diff_indexes(before: &Table, model: &Table, changes: &mut TableChanges) {
    for index in before.indexes() {
        if !model.indexes().contains(index) {
            changes.dropped_constraints.push(Change::new(index.drop_sql()));
        }
    }
    for index in model.indexes() {
        if before.indexes().contains(index) {
            continue;
        }
        let sql = index.create_sql(&model.table);
        changes.created_indexes.push(if index.unique {
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

/// The statements that take a database matching `previous` to `models`.
///
/// Tables and columns are matched by name, then by the previous name given
/// with `renamed_from`. What matches nothing is created or dropped. Foreign
/// keys are added after every table exists, and tables are dropped starting
/// with those that reference others.
pub fn diff(previous: &SchemaSnapshot, models: &[Table]) -> Vec<Change> {
    let matched: Vec<(&Table, Option<&Table>)> = models
        .iter()
        .map(|model| {
            let before = continued(
                &previous.tables,
                |table| &table.table,
                &model.table,
                model.previous_name(),
                |name| models.iter().any(|other| other.table == name),
            );
            (model, before)
        })
        .collect();
    let table_renames: Renames = matched
        .iter()
        .filter_map(|(model, before)| Some((before.as_ref()?.table.as_str(), model.table.as_str())))
        .filter(|(old, new)| old != new)
        .collect();

    let mut changes = Vec::new();
    let mut added_foreign_keys = Vec::new();
    for (model, before) in &matched {
        let Some(before) = before else {
            changes.push(Change::new(model.create_sql()));
            // A new table has no row that could break its foreign keys.
            added_foreign_keys.extend(
                model
                    .foreign_keys()
                    .iter()
                    .map(|foreign_key| Change::new(foreign_key.add_sql(&model.table))),
            );
            continue;
        };
        if before.table != model.table {
            changes.push(Change::new(format!(
                "ALTER TABLE {} RENAME TO {};",
                quote(&before.table),
                quote(&model.table)
            )));
        }
        let mut table = TableChanges::default();
        let column_renames = diff_columns(before, model, &mut table);
        diff_foreign_keys(before, model, &column_renames, &table_renames, &mut table);
        diff_indexes(before, model, &mut table);

        changes.extend(table.renames);
        changes.extend(table.additions);
        changes.extend(table.alterations);
        changes.extend(table.dropped_constraints);
        changes.extend(table.dropped_columns);
        changes.extend(table.created_indexes);
        added_foreign_keys.extend(table.added_foreign_keys);
    }
    changes.extend(added_foreign_keys);

    // A table is dropped once no other table to drop references it.
    let mut dropped: Vec<&Table> = previous
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
                        .foreign_keys()
                        .iter()
                        .any(|foreign_key| foreign_key.table == table.table)
            })
        });
        // Tables that reference each other are dropped in the order of the snapshot.
        let table = dropped.remove(free.unwrap_or(0));
        changes.push(Change::destructive(
            format!("DROP TABLE IF EXISTS {};", quote(&table.table)),
            format!("drops the table {} and its data.", table.table),
        ));
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Table;

    fn reviews() -> Table {
        Table::conventional("reviews")
            .with_column(ColumnDef::new("product_id", "text"))
            .with_column(ColumnDef::new("rating", "integer"))
            .index("reviews_shop_product_idx", &["shop", "product_id"])
    }

    fn sql(previous: &[Table], current: &[Table]) -> String {
        render(&diff(&SchemaSnapshot::of(previous), current))
    }

    #[test]
    fn identical_models_need_no_statement() {
        assert!(diff(&SchemaSnapshot::of(&[reviews()]), &[reviews()]).is_empty());
        // A definition written as a string compares on what it means.
        let written = Table::conventional("reviews")
            .column("product_id", "TEXT not null")
            .column("rating", "int NOT NULL")
            .index("reviews_shop_product_idx", &["shop", "product_id"]);
        assert!(diff(&SchemaSnapshot::of(&[reviews()]), &[written]).is_empty());
    }

    #[test]
    fn new_tables_are_created_and_removed_ones_dropped() {
        let votes = Table::conventional("review_votes").with_column(ColumnDef::new("value", "integer"));
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
    fn what_loses_data_is_told_apart() {
        let destructive = |previous: &[Table], current: &[Table]| destructive(&diff(&SchemaSnapshot::of(previous), current));

        let votes = Table::conventional("review_votes").with_column(ColumnDef::new("value", "integer"));
        assert_eq!(
            destructive(&[reviews(), votes.clone()], &[reviews()]),
            ["DROP TABLE IF EXISTS \"review_votes\": drops the table review_votes and its data."]
        );
        let legacy = reviews().with_column(ColumnDef::new("legacy", "text").nullable(true));
        assert_eq!(
            destructive(&[legacy], &[reviews()]),
            ["ALTER TABLE \"reviews\" DROP COLUMN IF EXISTS \"legacy\": drops reviews.legacy and its data."]
        );

        // Created, renamed, indexed, relaxed and widened: nothing is lost.
        let as_type = |sql_type: &str| Table::conventional("t").with_column(ColumnDef::new("c", sql_type));
        let loses = |before: &str, after: &str| !destructive(&[as_type(before)], &[as_type(after)]).is_empty();
        for (before, after) in [
            ("smallint", "integer"),
            ("integer", "bigint"),
            ("smallint", "bigint"),
            ("real", "double precision"),
            ("varchar(20)", "varchar(40)"),
            ("varchar(20)", "varchar"),
            ("varchar(20)", "text"),
            ("char(2)", "text"),
            ("varchar(20)[]", "varchar(40)[]"),
            ("integer[]", "bigint[]"),
            ("numeric(10, 2)", "numeric(12, 2)"),
            ("numeric(10, 2)", "numeric"),
        ] {
            assert!(!loses(before, after), "{before} to {after}");
        }
        for (before, after) in [
            ("bigint", "integer"),
            ("integer", "smallint"),
            ("text", "varchar(20)"),
            ("varchar(40)", "varchar(20)"),
            ("varchar", "varchar(20)"),
            ("double precision", "real"),
            ("numeric(10, 2)", "integer"),
            ("text", "integer"),
            ("timestamp with time zone", "date"),
            ("varchar(20)[]", "varchar(3)[]"),
            ("varchar(20)[]", "varchar(3)[][]"),
            ("varchar(20)[]", "text"),
            ("text[]", "varchar(20)[]"),
            ("integer[]", "integer"),
            ("integer", "integer[]"),
            ("numeric(10, 2)", "numeric(12, 3)"),
            ("numeric(10, 2)", "numeric(8, 2)"),
            ("numeric", "numeric(10, 2)"),
            ("timestamp(3) with time zone", "timestamp(6) with time zone"),
            ("timestamp(6)", "timestamp(3)"),
            ("varchar(x)", "varchar(40)"),
            ("bit(8)", "bit(4)"),
            ("char(8)", "char(4)"),
            ("char(4)", "char(8)"),
        ] {
            assert!(loses(before, after), "{before} to {after}");
        }

        let renamed = Table::conventional("product_reviews")
            .renamed_from("reviews")
            .with_column(ColumnDef::new("product_id", "text"))
            .with_column(ColumnDef::new("score", "integer").renamed_from("rating"))
            .index("reviews_score_idx", &["score"]);
        assert!(destructive(&[reviews()], &[renamed]).is_empty());
    }

    #[test]
    fn changed_columns_are_altered_in_place() {
        let previous = Table::conventional("reviews")
            .with_column(ColumnDef::new("rating", "integer"))
            .with_column(ColumnDef::new("note", "text").nullable(true).default("''"))
            .with_column(ColumnDef::new("approved", "boolean"));
        let current = Table::conventional("reviews")
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
        let previous = Table::conventional("reviews").with_column(ColumnDef::new("stars", "integer"));

        // Without it, a rename is a column dropped and another added.
        let guessed = Table::conventional("reviews").with_column(ColumnDef::new("rating", "integer"));
        let statements = sql(std::slice::from_ref(&previous), &[guessed]);
        assert!(
            statements.contains("ADD COLUMN IF NOT EXISTS \"rating\""),
            "{statements}"
        );
        assert!(statements.contains("DROP COLUMN IF EXISTS \"stars\""), "{statements}");

        let renamed = Table::conventional("product_reviews")
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
        let current = Table::conventional("reviews")
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

    fn votes() -> Table {
        Table::conventional("review_votes")
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
        let plain = Table::conventional("review_votes").with_column(ColumnDef::new("review_id", "uuid"));

        assert_eq!(
            sql(&[reviews(), plain.clone()], &[reviews(), votes()]),
            "-- WARNING: fails if a row of review_votes has a review_id that is not the id of a row of reviews.\n\
             ALTER TABLE \"review_votes\" ADD CONSTRAINT \"review_votes_review_id_fkey\" \
             FOREIGN KEY (\"review_id\") REFERENCES \"reviews\" (\"id\") ON DELETE CASCADE;\n"
        );

        // Another action on the same link: the constraint is replaced, without warning.
        let restricted = Table::conventional("review_votes")
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
        let renamed_reviews = Table::conventional("product_reviews")
            .renamed_from("reviews")
            .with_column(ColumnDef::new("product_id", "text"))
            .with_column(ColumnDef::new("rating", "integer"))
            .index("reviews_shop_product_idx", &["shop", "product_id"]);
        let renamed_votes = Table::conventional("votes")
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
        let replies = Table::conventional("vote_replies")
            .with_column(ColumnDef::new("vote_id", "uuid"))
            .with_foreign_key(ForeignKeyDef::new("vote_id", "review_votes"));
        let statements = sql(&[reviews(), votes(), replies], &[]);
        let position = |table: &str| statements.find(&format!("DROP TABLE IF EXISTS \"{table}\";")).unwrap();
        assert!(position("vote_replies") < position("review_votes"), "{statements}");
        assert!(position("review_votes") < position("reviews"), "{statements}");

        // A column dropped with its foreign key loses the constraint first.
        let without = Table::conventional("review_votes");
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
        let previous = Table::conventional("reviews").column("product_id", "text NOT NULL");
        let current = Table::conventional("reviews").column("product_id", "text NOT NULL REFERENCES products(id)");
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
