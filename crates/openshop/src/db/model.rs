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

    /// The part of [`Table::adopt_sql`] that brings an existing column to
    /// this definition: added when missing, converted between `json` and
    /// `jsonb`, given its default and its nullability. Another type fails.
    fn adopt_sql(&self, table: &str) -> String {
        let target = quote(table);
        let column = quote(&self.name);
        let alter = format!("ALTER TABLE {target} ALTER COLUMN {column}");
        let mut sql = format!(
            "  SELECT format_type(atttypid, atttypmod) INTO existing FROM pg_attribute\n    \
               WHERE attrelid = {}::regclass AND attname = {} AND attnum > 0 AND NOT attisdropped;\n  \
             IF existing IS NULL THEN\n    \
               {}\n  \
             ELSE\n",
            literal(&target),
            literal(&self.name),
            self.add_sql(table)
        );
        let mismatch = format!(
            "replace(existing, ' ', '') <> {} THEN\n      \
               RAISE EXCEPTION '[openshop] Column %.% is % in the database and % in the model. \
             Give the field the type of the database with `sql_type`, or convert the column in a migration of its own.', \
             {}, {}, existing, {};\n    \
             END IF;\n",
            literal(&self.sql_type.replace(' ', "")),
            literal(table),
            literal(&self.name),
            literal(&self.sql_type)
        );
        if matches!(self.sql_type.as_str(), "json" | "jsonb") {
            // Lossless for valid JSON. `jsonb` drops duplicate keys and reorders them.
            sql.push_str(&format!(
                "    IF existing IN ('json', 'jsonb') AND existing <> {ty} THEN\n      \
                   {alter} DROP DEFAULT;\n      \
                   {alter} TYPE {ty_sql} USING {column}::{ty_sql};\n    \
                 ELSIF {mismatch}",
                ty = literal(&self.sql_type),
                ty_sql = self.sql_type,
            ));
        } else {
            sql.push_str(&format!("    IF {mismatch}"));
        }
        match &self.default {
            Some(default) => sql.push_str(&format!("    {alter} SET DEFAULT {default};\n")),
            None => sql.push_str(&format!("    {alter} DROP DEFAULT;\n")),
        }
        sql.push_str(&format!(
            "    {alter} {} NOT NULL;\n  END IF;\n",
            if self.nullable { "DROP" } else { "SET" }
        ));
        sql
    }
}

/// A SQL string literal.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
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

    /// The part of [`Table::adopt_sql`] that gives this name to an index of
    /// the table on the same columns, when the table has none by this name.
    fn adopt_sql(&self, table: &str) -> String {
        let columns: Vec<String> = self.columns.iter().map(|column| literal(column)).collect();
        format!(
            "  IF to_regclass({name}) IS NULL THEN\n    \
               SELECT i.indexrelid::regclass::text INTO existing FROM pg_index i\n      \
                 JOIN pg_class c ON c.oid = i.indexrelid\n      \
                 JOIN pg_am am ON am.oid = c.relam AND am.amname = 'btree'\n      \
                 WHERE i.indrelid = {table}::regclass AND NOT i.indisprimary AND i.indisunique = {unique}\n        \
                   AND i.indexprs IS NULL AND i.indpred IS NULL AND i.indnkeyatts = i.indnatts\n        \
                   AND ARRAY(\n          \
                     SELECT a.attname::text FROM unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)\n          \
                     JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum ORDER BY k.ord\n        \
                   ) = ARRAY[{columns}]::text[]\n      \
                 ORDER BY c.relname LIMIT 1;\n    \
               IF existing IS NOT NULL THEN\n      \
                 EXECUTE format('ALTER INDEX %s RENAME TO %I', existing, {plain});\n    \
               END IF;\n  \
             END IF;\n",
            name = literal(&quote(&self.name)),
            table = literal(&quote(table)),
            unique = self.unique,
            columns = columns.join(", "),
            plain = literal(&self.name),
        )
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

    /// `pg_constraint.confdeltype` of the `on_delete` action.
    fn delete_code(&self) -> char {
        match self.on_delete.as_deref() {
            Some("CASCADE") => 'c',
            Some("RESTRICT") => 'r',
            Some("SET NULL") => 'n',
            Some("SET DEFAULT") => 'd',
            _ => 'a',
        }
    }

    /// The part of [`Table::adopt_sql`] that leaves the column with this
    /// constraint alone: the other foreign keys of the column are renamed to
    /// its name when they are the same constraint, dropped otherwise.
    fn adopt_sql(&self, table: &str) -> String {
        let target = literal(&quote(table));
        let referenced = literal(&quote(&self.table));
        let name = literal(&self.name(table));
        format!(
            "  matched := EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = {target}::regclass AND conname = {name});\n  \
             FOR existing IN\n    \
               SELECT conname::text FROM pg_constraint\n    \
               WHERE conrelid = {target}::regclass AND contype = 'f' AND conname <> {name}\n      \
                 AND conkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = {target}::regclass AND attname = {column})]\n    \
               ORDER BY conname\n  \
             LOOP\n    \
               IF NOT matched AND EXISTS (\n      \
                 SELECT 1 FROM pg_constraint\n      \
                 WHERE conrelid = {target}::regclass AND conname = existing\n        \
                   AND confrelid = to_regclass({referenced})\n        \
                   AND confkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = to_regclass({referenced}) AND attname = {references})]\n        \
                   AND confdeltype = '{delete}' AND confupdtype = 'a' AND confmatchtype = 's'\n    \
               ) THEN\n      \
                 EXECUTE format('ALTER TABLE %s RENAME CONSTRAINT %I TO %I', {target}, existing, {name});\n      \
                 matched := true;\n    \
               ELSE\n      \
                 EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', {target}, existing);\n    \
               END IF;\n  \
             END LOOP;\n",
            column = literal(&self.column),
            references = literal(&self.references),
            delete = self.delete_code(),
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

    /// A block that brings the table to the model when it already exists,
    /// and does nothing otherwise. It is how a migration takes over a table
    /// that another tool created, such as the TypeScript version of OpenShop
    /// with Drizzle:
    ///
    /// - missing columns are added, and existing ones get the default and the
    ///   nullability of the model; `json` and `jsonb` are converted into each
    ///   other, and any other difference of type fails the migration;
    /// - a `NOT NULL` column without default that the model lacks fails it too,
    ///   since every insertion would;
    /// - an index on the same columns, or a foreign key with the same target
    ///   and the same `ON DELETE`, is renamed to the name of the model instead
    ///   of being created a second time. Other foreign keys of a referencing
    ///   column are dropped.
    ///
    /// It runs before [`Table::create_sql`], whose statements then find what
    /// they would create.
    pub fn adopt_sql(&self) -> String {
        let target = literal(&quote(&self.table));
        let mut body = String::new();
        for column in &self.columns {
            body.push_str(&column.adopt_sql(&self.table));
        }
        let known: Vec<String> = self.columns.iter().map(|column| literal(&column.name)).collect();
        body.push_str(&format!(
            "  SELECT string_agg(quote_ident(attname), ', ' ORDER BY attnum) INTO existing FROM pg_attribute\n    \
               WHERE attrelid = {target}::regclass AND attnum > 0 AND NOT attisdropped AND attnotnull\n      \
                 AND NOT atthasdef AND attidentity = '' AND attgenerated = ''\n      \
                 AND attname NOT IN ({});\n  \
             IF existing IS NOT NULL THEN\n    \
               RAISE EXCEPTION '[openshop] % has NOT NULL columns without default that the model does not have: %. \
             Add them to the model, or drop them in a migration of their own.', {}, existing;\n  \
             END IF;\n",
            known.join(", "),
            literal(&self.table)
        ));
        for index in &self.indexes {
            body.push_str(&index.adopt_sql(&self.table));
        }
        for foreign_key in &self.foreign_keys {
            body.push_str(&foreign_key.adopt_sql(&self.table));
        }
        format!(
            "-- When {} already exists, such as a table created by the TypeScript version of OpenShop,\n\
             -- this brings it to the model; otherwise it does nothing.\n\
             DO $openshop$\n\
             DECLARE\n  \
               existing text;\n  \
               matched boolean;\n\
             BEGIN\n  \
               IF to_regclass({target}) IS NULL THEN\n    \
                 RETURN;\n  \
               END IF;\n\
             {body}\
             END\n\
             $openshop$;",
            quote(&self.table)
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
