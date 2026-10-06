//! Application tables with the conventional OpenShop columns.

/// A table definition that renders to `CREATE TABLE` SQL.
///
/// Every model gets `id` (uuid primary key) and, unless disabled, `shop`,
/// `created_at` and `updated_at`. Application code is responsible for filtering
/// its own queries by shop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub table: String,
    columns: Vec<(String, String)>,
    shop: bool,
    created_at: bool,
    updated_at: bool,
    indexes: Vec<String>,
}

/// Define a model with auto-generated columns:
/// - `id` (uuid, primary key)
/// - `shop` (text) — unless [`Model::without_shop`]
/// - `created_at` / `updated_at` (timestamptz, default now)
pub fn define_model(table: impl Into<String>) -> Model {
    Model {
        table: table.into(),
        columns: Vec::new(),
        shop: true,
        created_at: true,
        updated_at: true,
        indexes: Vec::new(),
    }
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

impl Model {
    /// Adds a column, e.g. `.column("product_id", "text NOT NULL")`.
    pub fn column(mut self, name: impl Into<String>, definition: impl Into<String>) -> Self {
        self.columns.push((name.into(), definition.into()));
        self
    }

    pub fn without_shop(mut self) -> Self {
        self.shop = false;
        self
    }

    pub fn without_created_at(mut self) -> Self {
        self.created_at = false;
        self
    }

    pub fn without_updated_at(mut self) -> Self {
        self.updated_at = false;
        self
    }

    pub fn index(mut self, name: impl Into<String>, columns: &[&str]) -> Self {
        self.indexes.push(self.index_sql("INDEX", &name.into(), columns));
        self
    }

    pub fn unique_index(mut self, name: impl Into<String>, columns: &[&str]) -> Self {
        self.indexes.push(self.index_sql("UNIQUE INDEX", &name.into(), columns));
        self
    }

    fn index_sql(&self, kind: &str, name: &str, columns: &[&str]) -> String {
        let columns: Vec<String> = columns.iter().map(|column| quote(column)).collect();
        format!(
            "CREATE {kind} IF NOT EXISTS {} ON {} ({});",
            quote(name),
            quote(&self.table),
            columns.join(", ")
        )
    }

    /// Idempotent DDL for the table and its indexes.
    pub fn create_sql(&self) -> String {
        let mut columns = vec![format!(
            "  {} uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL",
            quote("id")
        )];
        if self.shop {
            columns.push(format!("  {} text NOT NULL", quote("shop")));
        }
        if self.created_at {
            columns.push(format!(
                "  {} timestamp with time zone DEFAULT now() NOT NULL",
                quote("created_at")
            ));
        }
        if self.updated_at {
            columns.push(format!(
                "  {} timestamp with time zone DEFAULT now() NOT NULL",
                quote("updated_at")
            ));
        }
        for (name, definition) in &self.columns {
            columns.push(format!("  {} {definition}", quote(name)));
        }

        let mut sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
            quote(&self.table),
            columns.join(",\n")
        );
        for index in &self.indexes {
            sql.push('\n');
            sql.push_str(index);
        }
        sql
    }
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
}
