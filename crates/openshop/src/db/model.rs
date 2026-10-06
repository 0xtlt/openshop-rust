//! Application tables with the conventional OpenShop columns.

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
    /// Other constraints written with the column, such as `UNIQUE` or `REFERENCES …`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub constraints: String,
    /// The name the column had before, for `migrate generate` to rename it
    /// instead of dropping and adding it.
    #[serde(skip)]
    pub renamed_from: Option<String>,
    /// The definition as written with [`Model::column`], rendered verbatim.
    #[serde(skip)]
    written: Option<String>,
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
            constraints: String::new(),
            renamed_from: None,
            written: None,
        }
    }

    /// `id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL`.
    pub fn id() -> Self {
        Self::new("id", "uuid").primary_key().default("gen_random_uuid()")
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

    pub fn primary_key(mut self) -> Self {
        self.primary_key = true;
        self.nullable = false;
        self
    }

    pub fn renamed_from(mut self, previous: impl Into<String>) -> Self {
        self.renamed_from = Some(previous.into());
        self
    }

    /// The definition that follows the column name in `CREATE TABLE`.
    pub fn definition_sql(&self) -> String {
        if let Some(written) = &self.written {
            return written.clone();
        }
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
        if !self.constraints.is_empty() {
            sql.push_str(&format!(" {}", self.constraints));
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

/// A table definition that renders to `CREATE TABLE` SQL.
///
/// Every model gets `id` (uuid primary key) and, unless disabled, `shop`,
/// `created_at` and `updated_at`. Application code is responsible for filtering
/// its own queries by shop; the typed rows of a `#[derive(Model)]` struct do it.
///
/// The serialized form is what the schema snapshot records for the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
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

/// Define a model with auto-generated columns:
/// - `id` (uuid, primary key)
/// - `shop` (text) — unless [`Model::without_shop`]
/// - `created_at` / `updated_at` (timestamptz, default now)
pub fn define_model(table: impl Into<String>) -> Model {
    Model {
        table: table.into(),
        columns: vec![
            ColumnDef::id(),
            ColumnDef::shop(),
            ColumnDef::created_at(),
            ColumnDef::updated_at(),
        ],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        renamed_from: None,
    }
}

pub(crate) fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

impl Model {
    /// A table without any column. `#[derive(Model)]` starts from it;
    /// [`define_model`] adds the conventional columns.
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

    /// Adds a column, e.g. `.column("product_id", "text NOT NULL")`.
    pub fn column(mut self, name: impl Into<String>, definition: impl Into<String>) -> Self {
        self.columns.push(parse_definition(name.into(), definition.into()));
        self
    }

    /// Adds a column described field by field.
    pub fn with_column(mut self, column: ColumnDef) -> Self {
        self.columns.push(column);
        self
    }

    fn without(mut self, column: &str) -> Self {
        self.columns.retain(|existing| existing.name != column);
        self
    }

    pub fn without_shop(self) -> Self {
        self.without("shop")
    }

    pub fn without_created_at(self) -> Self {
        self.without("created_at")
    }

    pub fn without_updated_at(self) -> Self {
        self.without("updated_at")
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
    /// The constraint is not part of [`Model::create_sql`]: it is added once
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

/// Splits on spaces that are outside parentheses and quotes.
fn definition_tokens(definition: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut depth = 0_usize;
    let mut quote: Option<char> = None;
    for character in definition.chars() {
        match quote {
            Some(open) => {
                if character == open {
                    quote = None;
                }
                current.push(character);
            }
            None if character == '\'' || character == '"' => {
                quote = Some(character);
                current.push(character);
            }
            None if character == '(' => {
                depth += 1;
                current.push(character);
            }
            None if character == ')' => {
                depth = depth.saturating_sub(1);
                current.push(character);
            }
            None if character.is_whitespace() && depth == 0 => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// The words that end the type or the default expression of a column.
fn is_constraint_keyword(token: &str) -> bool {
    matches!(
        token.to_ascii_uppercase().as_str(),
        "NOT"
            | "NULL"
            | "DEFAULT"
            | "PRIMARY"
            | "UNIQUE"
            | "REFERENCES"
            | "CHECK"
            | "GENERATED"
            | "COLLATE"
            | "CONSTRAINT"
    )
}

/// Reads a column definition such as `text NOT NULL DEFAULT ''`: its type,
/// nullability and default. What is not recognized is kept as `constraints`,
/// and the definition itself is kept to be rendered as written.
fn parse_definition(name: String, definition: String) -> ColumnDef {
    let tokens = definition_tokens(&definition);
    let type_end = tokens
        .iter()
        .position(|token| is_constraint_keyword(token))
        .unwrap_or(tokens.len());
    let mut parsed = ColumnDef {
        name,
        sql_type: normalize_type(&tokens[..type_end].join(" ")),
        nullable: true,
        default: None,
        primary_key: false,
        constraints: String::new(),
        renamed_from: None,
        written: None,
    };

    let mut constraints: Vec<&str> = Vec::new();
    let mut index = type_end;
    while index < tokens.len() {
        let token = tokens[index].to_ascii_uppercase();
        let next = tokens.get(index + 1).map(|next| next.to_ascii_uppercase());
        match (token.as_str(), next.as_deref()) {
            ("NOT", Some("NULL")) => {
                parsed.nullable = false;
                index += 2;
            }
            ("NULL", _) => index += 1,
            ("PRIMARY", Some("KEY")) => {
                parsed.primary_key = true;
                parsed.nullable = false;
                index += 2;
            }
            ("DEFAULT", _) => {
                let end = tokens[index + 1..]
                    .iter()
                    .position(|token| is_constraint_keyword(token))
                    .map_or(tokens.len(), |offset| index + 1 + offset);
                parsed.default = Some(tokens[index + 1..end].join(" ")).filter(|default| !default.is_empty());
                index = end;
            }
            _ => {
                // An unrecognized constraint runs until the next one this parser reads.
                let end = tokens[index + 1..]
                    .iter()
                    .position(|token| {
                        matches!(
                            token.to_ascii_uppercase().as_str(),
                            "NOT" | "NULL" | "DEFAULT" | "PRIMARY"
                        )
                    })
                    .map_or(tokens.len(), |offset| index + 1 + offset);
                constraints.extend(tokens[index..end].iter().map(String::as_str));
                index = end;
            }
        }
    }
    parsed.constraints = constraints.join(" ");
    parsed.written = Some(definition);
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_the_conventional_columns_by_default() {
        let sql = define_model("reviews")
            .column("product_id", "text NOT NULL")
            .column("approved", "boolean DEFAULT false")
            .create_sql();
        assert_eq!(
            sql,
            "CREATE TABLE IF NOT EXISTS \"reviews\" (\n\
             \x20 \"id\" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,\n\
             \x20 \"shop\" text NOT NULL,\n\
             \x20 \"created_at\" timestamp with time zone DEFAULT now() NOT NULL,\n\
             \x20 \"updated_at\" timestamp with time zone DEFAULT now() NOT NULL,\n\
             \x20 \"product_id\" text NOT NULL,\n\
             \x20 \"approved\" boolean DEFAULT false\n\
             );"
        );
    }

    #[test]
    fn can_drop_conventional_columns_and_add_indexes() {
        let sql = define_model("settings")
            .without_shop()
            .without_created_at()
            .without_updated_at()
            .column("key", "text NOT NULL")
            .unique_index("settings_key_unique", &["key"])
            .index("settings_key_idx", &["key", "id"])
            .create_sql();
        assert!(!sql.contains("\"shop\""));
        assert!(!sql.contains("created_at"));
        assert!(!sql.contains("updated_at"));
        assert!(sql.contains("CREATE UNIQUE INDEX IF NOT EXISTS \"settings_key_unique\" ON \"settings\" (\"key\");"));
        assert!(sql.contains("CREATE INDEX IF NOT EXISTS \"settings_key_idx\" ON \"settings\" (\"key\", \"id\");"));
    }

    #[test]
    fn renders_columns_described_field_by_field() {
        let model = Model::new("reviews")
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

    fn written(definition: &str) -> ColumnDef {
        parse_definition("c".to_string(), definition.to_string())
    }

    #[test]
    fn reads_the_facts_of_a_written_definition() {
        let column = written("text NOT NULL");
        assert_eq!(
            (column.sql_type.as_str(), column.nullable, column.default.as_deref()),
            ("text", false, None)
        );

        let column = written("boolean DEFAULT false");
        assert_eq!(
            (column.sql_type.as_str(), column.nullable, column.default.as_deref()),
            ("boolean", true, Some("false"))
        );

        let column = written("TIMESTAMPTZ   default now()  not null");
        assert_eq!(column.sql_type, "timestamp with time zone");
        assert_eq!((column.nullable, column.default.as_deref()), (false, Some("now()")));

        let column = written("numeric(10, 2) NOT NULL DEFAULT 0 CHECK (c >= 0)");
        assert_eq!(column.sql_type, "numeric(10, 2)");
        assert_eq!((column.nullable, column.default.as_deref()), (false, Some("0")));
        assert_eq!(column.constraints, "CHECK (c >= 0)");

        let column = written("text DEFAULT 'not null' REFERENCES products(id) ON DELETE CASCADE NOT NULL");
        assert_eq!(column.default.as_deref(), Some("'not null'"));
        assert_eq!(column.constraints, "REFERENCES products(id) ON DELETE CASCADE");
        assert!(!column.nullable);

        let column = written("uuid PRIMARY KEY");
        assert!(column.primary_key && !column.nullable);

        let column = written("varchar(64) UNIQUE");
        assert_eq!(
            (column.sql_type.as_str(), column.constraints.as_str()),
            ("character varying(64)", "UNIQUE")
        );

        let column = written("jsonb DEFAULT '{}'::jsonb NOT NULL");
        assert_eq!(column.default.as_deref(), Some("'{}'::jsonb"));

        // A written definition is rendered as written.
        assert_eq!(written("TEXT  not null").definition_sql(), "TEXT  not null");
    }

    #[test]
    fn foreign_keys_are_added_once_the_tables_exist() {
        let votes = define_model("review_votes")
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
