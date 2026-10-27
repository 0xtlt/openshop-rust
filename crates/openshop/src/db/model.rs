//! The tables of the application models, as `#[derive(Model)]` describes them.

use serde::{Deserialize, Serialize};

fn is_false(value: &bool) -> bool {
    !*value
}

/// A column of an application table.
///
/// The serialized form is what the schema snapshot stores, and what two
/// versions of a model are compared on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    /// The SQL type, such as `text` or `timestamp with time zone`.
    #[serde(rename = "type")]
    pub sql_type: String,
    pub nullable: bool,
    /// The SQL expression of the default value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(rename = "primaryKey", default, skip_serializing_if = "is_false")]
    pub primary_key: bool,
    /// The name the column had before, for `migrate generate` to rename it
    /// instead of dropping and adding it.
    #[serde(skip)]
    pub renamed_from: Option<String>,
}

impl ColumnDef {
    /// A `NOT NULL` column without default.
    pub fn new(name: impl Into<String>, sql_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sql_type: normalize_type(&sql_type.into()),
            nullable: false,
            default: None,
            primary_key: false,
            renamed_from: None,
        }
    }

    /// `id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL`, the primary
    /// key of every model.
    pub fn id() -> Self {
        Self {
            primary_key: true,
            ..Self::new("id", "uuid").default("gen_random_uuid()")
        }
    }

    /// `shop text NOT NULL`.
    pub fn shop() -> Self {
        Self::new("shop", "text")
    }

    /// `created_at timestamp with time zone DEFAULT now() NOT NULL`.
    pub fn created_at() -> Self {
        Self::new("created_at", "timestamp with time zone").default("now()")
    }

    /// `updated_at timestamp with time zone DEFAULT now() NOT NULL`.
    pub fn updated_at() -> Self {
        Self::new("updated_at", "timestamp with time zone").default("now()")
    }

    pub fn nullable(mut self, nullable: bool) -> Self {
        self.nullable = nullable;
        self
    }

    /// Sets the default to a SQL expression, such as `false` or `now()`.
    pub fn default(mut self, expression: impl Into<String>) -> Self {
        self.default = Some(expression.into());
        self
    }

    pub fn renamed_from(mut self, previous: impl Into<String>) -> Self {
        self.renamed_from = Some(previous.into());
        self
    }

    /// The definition that follows the column name in `CREATE TABLE`.
    pub fn definition_sql(&self) -> String {
        let mut sql = self.sql_type.clone();
        if self.primary_key {
            sql.push_str(" PRIMARY KEY");
        }
        if let Some(default) = &self.default {
            sql.push_str(&format!(" DEFAULT {default}"));
        }
        if !self.nullable {
            sql.push_str(" NOT NULL");
        }
        sql
    }

    /// `"name" definition`.
    pub fn sql(&self) -> String {
        format!("{} {}", quote(&self.name), self.definition_sql())
    }

    /// `ADD COLUMN IF NOT EXISTS "name" definition`, an action of `ALTER TABLE`.
    pub fn add_clause(&self) -> String {
        format!("ADD COLUMN IF NOT EXISTS {}", self.sql())
    }

    /// `ALTER TABLE … ADD COLUMN IF NOT EXISTS …;`
    pub fn add_sql(&self, table: &str) -> String {
        format!("ALTER TABLE {} {};", quote(table), self.add_clause())
    }
}

/// An index of an application table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDef {
    pub name: String,
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub unique: bool,
}

impl IndexDef {
    /// `CREATE [UNIQUE] INDEX IF NOT EXISTS … ON table (…);`
    pub fn create_sql(&self, table: &str) -> String {
        let columns: Vec<String> = self.columns.iter().map(|column| quote(column)).collect();
        format!(
            "CREATE {} IF NOT EXISTS {} ON {} ({});",
            if self.unique { "UNIQUE INDEX" } else { "INDEX" },
            quote(&self.name),
            quote(table),
            columns.join(", ")
        )
    }

    /// `DROP INDEX IF EXISTS …;`
    pub fn drop_sql(&self) -> String {
        format!("DROP INDEX IF EXISTS {};", quote(&self.name))
    }
}

/// A column that must hold the key of a row of another table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKeyDef {
    pub column: String,
    /// The referenced table.
    pub table: String,
    /// The referenced column, `id` unless stated.
    pub references: String,
    /// What happens to the row when the referenced row is deleted: `CASCADE`,
    /// `RESTRICT`, `SET NULL`, `SET DEFAULT` or `NO ACTION`. PostgreSQL applies
    /// `NO ACTION` when it is not stated.
    #[serde(rename = "onDelete", default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
}

impl ForeignKeyDef {
    /// `column` references the `id` of `table`.
    pub fn new(column: impl Into<String>, table: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            table: table.into(),
            references: "id".to_string(),
            on_delete: None,
        }
    }

    /// References another column than `id`. It must be unique in its table.
    pub fn references(mut self, column: impl Into<String>) -> Self {
        self.references = column.into();
        self
    }

    /// Sets the action taken when the referenced row is deleted.
    pub fn on_delete(mut self, action: impl Into<String>) -> Self {
        self.on_delete = Some(collapse_spaces(&action.into()).to_uppercase());
        self
    }

    /// The name of the constraint on `table`: the one PostgreSQL gives by default.
    pub fn name(&self, table: &str) -> String {
        format!("{table}_{}_fkey", self.column)
    }

    /// `FOREIGN KEY (…) REFERENCES … (…) [ON DELETE …]`.
    fn definition_sql(&self) -> String {
        let mut sql = format!(
            "FOREIGN KEY ({}) REFERENCES {} ({})",
            quote(&self.column),
            quote(&self.table),
            quote(&self.references)
        );
        if let Some(action) = &self.on_delete {
            sql.push_str(&format!(" ON DELETE {action}"));
        }
        sql
    }

    /// `ALTER TABLE … ADD CONSTRAINT … FOREIGN KEY …;`
    pub fn add_sql(&self, table: &str) -> String {
        format!(
            "ALTER TABLE {} ADD CONSTRAINT {} {};",
            quote(table),
            quote(&self.name(table)),
            self.definition_sql()
        )
    }

    /// The same, doing nothing when the table already has the constraint.
    pub fn add_if_missing_sql(&self, table: &str) -> String {
        let name = self.name(table);
        format!(
            "DO $$ BEGIN\n  \
               IF NOT EXISTS (\n    \
                 SELECT 1 FROM pg_constraint\n    \
                 WHERE conname = '{}' AND conrelid = to_regclass('{}')\n  \
               ) THEN\n    \
                 {}\n  \
               END IF;\n\
             END $$;",
            name.replace('\'', "''"),
            quote(table).replace('\'', "''"),
            self.add_sql(table)
        )
    }
}

/// A table definition that renders to `CREATE TABLE` SQL: what
/// `#[derive(Model)]` builds from a struct.
///
/// The serialized form is what the schema snapshot records for the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    #[serde(rename = "name")]
    pub table: String,
    columns: Vec<ColumnDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    indexes: Vec<IndexDef>,
    #[serde(rename = "foreignKeys", default, skip_serializing_if = "Vec::is_empty")]
    foreign_keys: Vec<ForeignKeyDef>,
    #[serde(skip)]
    renamed_from: Option<String>,
}

pub(crate) fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

impl Table {
    /// A table without any column. `#[derive(Model)]` starts from it.
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            columns: Vec::new(),
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            renamed_from: None,
        }
    }

    pub fn columns(&self) -> &[ColumnDef] {
        &self.columns
    }

    pub fn indexes(&self) -> &[IndexDef] {
        &self.indexes
    }

    pub fn foreign_keys(&self) -> &[ForeignKeyDef] {
        &self.foreign_keys
    }

    /// The name the table had before, when it was renamed.
    pub fn previous_name(&self) -> Option<&str> {
        self.renamed_from.as_deref()
    }

    /// Adds a column.
    pub fn with_column(mut self, column: ColumnDef) -> Self {
        self.columns.push(column);
        self
    }

    fn with_index(mut self, name: String, columns: &[&str], unique: bool) -> Self {
        self.indexes.push(IndexDef {
            name,
            columns: columns.iter().map(|column| column.to_string()).collect(),
            unique,
        });
        self
    }

    pub fn index(self, name: impl Into<String>, columns: &[&str]) -> Self {
        self.with_index(name.into(), columns, false)
    }

    pub fn unique_index(self, name: impl Into<String>, columns: &[&str]) -> Self {
        self.with_index(name.into(), columns, true)
    }

    /// Makes a column reference a row of another table.
    ///
    /// The constraint is not part of [`Table::create_sql`]: it is added once
    /// every table exists, so that models can be registered in any order.
    pub fn with_foreign_key(mut self, foreign_key: ForeignKeyDef) -> Self {
        self.foreign_keys.push(foreign_key);
        self
    }

    /// Tells `migrate generate` that the table was called `previous`, so that
    /// it is renamed instead of dropped and created again.
    pub fn renamed_from(mut self, previous: impl Into<String>) -> Self {
        self.renamed_from = Some(previous.into());
        self
    }

    /// Idempotent DDL for the table alone, without its indexes.
    pub fn create_table_sql(&self) -> String {
        let columns: Vec<String> = self
            .columns
            .iter()
            .map(|column| format!("  {}", column.sql()))
            .collect();
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
            quote(&self.table),
            columns.join(",\n")
        )
    }

    /// Idempotent DDL for the table and its indexes.
    pub fn create_sql(&self) -> String {
        let mut sql = self.create_table_sql();
        for index in &self.indexes {
            sql.push('\n');
            sql.push_str(&index.create_sql(&self.table));
        }
        sql
    }
}

/// Lowercases a SQL type, collapses its spaces and resolves the usual aliases,
/// so that `TIMESTAMPTZ` and `timestamp with time zone` compare equal.
pub(crate) fn normalize_type(sql_type: &str) -> String {
    let collapsed = collapse_spaces(sql_type).to_lowercase();
    let (base, suffix) = match collapsed.find(['(', '[']) {
        Some(index) => collapsed.split_at(index),
        None => (collapsed.as_str(), ""),
    };
    let base = match base.trim() {
        "int" | "int4" => "integer",
        "int2" => "smallint",
        "int8" => "bigint",
        "bool" => "boolean",
        "float4" => "real",
        "float8" => "double precision",
        "timestamptz" => "timestamp with time zone",
        "varchar" => "character varying",
        other => other,
    };
    format!("{base}{suffix}")
}

fn collapse_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A table with the columns most models have: `id`, `shop`, `created_at` and
/// `updated_at`.
#[cfg(test)]
pub(crate) fn conventional(table: &str) -> Table {
    Table::new(table)
        .with_column(ColumnDef::id())
        .with_column(ColumnDef::shop())
        .with_column(ColumnDef::created_at())
        .with_column(ColumnDef::updated_at())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_table_and_its_indexes() {
        let sql = conventional("settings")
            .with_column(ColumnDef::new("key", "text"))
            .unique_index("settings_key_unique", &["key"])
            .index("settings_key_idx", &["key", "id"])
            .create_sql();
        assert_eq!(
            sql,
            "CREATE TABLE IF NOT EXISTS \"settings\" (\n\
             \x20 \"id\" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,\n\
             \x20 \"shop\" text NOT NULL,\n\
             \x20 \"created_at\" timestamp with time zone DEFAULT now() NOT NULL,\n\
             \x20 \"updated_at\" timestamp with time zone DEFAULT now() NOT NULL,\n\
             \x20 \"key\" text NOT NULL\n\
             );\n\
             CREATE UNIQUE INDEX IF NOT EXISTS \"settings_key_unique\" ON \"settings\" (\"key\");\n\
             CREATE INDEX IF NOT EXISTS \"settings_key_idx\" ON \"settings\" (\"key\", \"id\");"
        );
    }

    #[test]
    fn renders_columns_described_field_by_field() {
        let model = Table::new("reviews")
            .with_column(ColumnDef::id())
            .with_column(ColumnDef::new("rating", "INT"))
            .with_column(ColumnDef::new("note", "text").nullable(true))
            .with_column(ColumnDef::new("approved", "boolean").default("false"));
        assert_eq!(
            model.create_sql(),
            "CREATE TABLE IF NOT EXISTS \"reviews\" (\n\
             \x20 \"id\" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,\n\
             \x20 \"rating\" integer NOT NULL,\n\
             \x20 \"note\" text,\n\
             \x20 \"approved\" boolean DEFAULT false NOT NULL\n\
             );"
        );
    }

    #[test]
    fn foreign_keys_are_added_once_the_tables_exist() {
        let votes = conventional("review_votes")
            .with_column(ColumnDef::new("review_id", "uuid"))
            .with_column(ColumnDef::new("author_code", "text").nullable(true))
            .with_foreign_key(ForeignKeyDef::new("review_id", "reviews").on_delete("cascade"))
            .with_foreign_key(
                ForeignKeyDef::new("author_code", "authors")
                    .references("code")
                    .on_delete("set  null"),
            );

        // The table is created without them.
        assert!(!votes.create_sql().contains("REFERENCES"));
        let [review, author] = votes.foreign_keys() else {
            panic!("two foreign keys");
        };
        assert_eq!(review.name("review_votes"), "review_votes_review_id_fkey");
        assert_eq!(
            review.add_sql("review_votes"),
            "ALTER TABLE \"review_votes\" ADD CONSTRAINT \"review_votes_review_id_fkey\" \
             FOREIGN KEY (\"review_id\") REFERENCES \"reviews\" (\"id\") ON DELETE CASCADE;"
        );
        assert_eq!(
            author.add_sql("review_votes"),
            "ALTER TABLE \"review_votes\" ADD CONSTRAINT \"review_votes_author_code_fkey\" \
             FOREIGN KEY (\"author_code\") REFERENCES \"authors\" (\"code\") ON DELETE SET NULL;"
        );
        let guarded = review.add_if_missing_sql("review_votes");
        assert!(guarded.starts_with("DO $$ BEGIN\n  IF NOT EXISTS ("), "{guarded}");
        assert!(guarded.contains("conname = 'review_votes_review_id_fkey'"), "{guarded}");
        assert!(guarded.contains(&review.add_sql("review_votes")), "{guarded}");
        assert_eq!(
            serde_json::to_value(review).unwrap(),
            serde_json::json!({ "column": "review_id", "table": "reviews", "references": "id", "onDelete": "CASCADE" })
        );
    }

    #[test]
    fn normalizes_type_aliases() {
        for (written, normalized) in [
            ("INT", "integer"),
            ("int8", "bigint"),
            ("Bool", "boolean"),
            ("timestamptz", "timestamp with time zone"),
            ("double   precision", "double precision"),
            ("varchar(32)", "character varying(32)"),
            ("text[]", "text[]"),
            ("int[]", "integer[]"),
        ] {
            assert_eq!(normalize_type(written), normalized);
        }
    }

    #[test]
    fn the_snapshot_form_keeps_only_what_is_compared() {
        let column = ColumnDef::new("rating", "integer").renamed_from("stars");
        assert_eq!(
            serde_json::to_value(&column).unwrap(),
            serde_json::json!({ "name": "rating", "type": "integer", "nullable": false })
        );
        assert_eq!(
            serde_json::to_value(ColumnDef::id()).unwrap(),
            serde_json::json!({
                "name": "id",
                "type": "uuid",
                "nullable": false,
                "default": "gen_random_uuid()",
                "primaryKey": true,
            })
        );
        let index = IndexDef {
            name: "reviews_shop_idx".to_string(),
            columns: vec!["shop".to_string()],
            unique: false,
        };
        assert_eq!(
            serde_json::to_value(&index).unwrap(),
            serde_json::json!({ "name": "reviews_shop_idx", "columns": ["shop"] })
        );
    }
}
