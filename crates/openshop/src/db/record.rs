//! Typed rows of application tables.
//!
//! `#[derive(Model)]` implements [`Model`] for a struct and gives it typed
//! columns and an entry point, such as `Review::for_shop(&db, &shop)`. The
//! [`Rows`] it returns insert, read, update and delete rows of that struct.

use std::marker::PhantomData;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::postgres::{PgHasArrayType, PgRow};
use sqlx::{Encode, FromRow, PgPool, Postgres, QueryBuilder, Type};
use uuid::Uuid;

use super::model::{Table, quote};

/// A Rust type that a column can hold.
///
/// Implemented for the types below and for `Option` of them, which makes the
/// column nullable. Implement it for your own type when `sqlx` can encode and
/// decode it:
///
/// | Rust | SQL |
/// | --- | --- |
/// | `String` | `text` |
/// | `bool` | `boolean` |
/// | `i16`, `i32`, `i64` | `smallint`, `integer`, `bigint` |
/// | `f32`, `f64` | `real`, `double precision` |
/// | `Uuid` | `uuid` |
/// | `DateTime<Utc>` | `timestamp with time zone` |
/// | `serde_json::Value` | `jsonb` |
/// | `Vec<String>`, `Vec<i32>`, `Vec<i64>`, `Vec<Uuid>` | `text[]`, `integer[]`, `bigint[]`, `uuid[]` |
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a type a model column can hold",
    label = "no SQL type is known for `{Self}`",
    note = "use one of the supported types, or implement `openshop::db::ColumnType` for `{Self}`"
)]
pub trait ColumnType {
    /// The SQL type of the column.
    const SQL_TYPE: &'static str;
    /// Whether the column accepts `NULL`.
    const NULLABLE: bool = false;
    /// What a filter compares the column with: the type itself, or what its `Option` holds.
    type Value;
}

macro_rules! column_type {
    ($($rust:ty => $sql:literal),+ $(,)?) => {$(
        impl ColumnType for $rust {
            const SQL_TYPE: &'static str = $sql;
            type Value = $rust;
        }
    )+};
}

column_type!(
    String => "text",
    bool => "boolean",
    i16 => "smallint",
    i32 => "integer",
    i64 => "bigint",
    f32 => "real",
    f64 => "double precision",
    Uuid => "uuid",
    DateTime<Utc> => "timestamp with time zone",
    Value => "jsonb",
    Vec<String> => "text[]",
    Vec<i32> => "integer[]",
    Vec<i64> => "bigint[]",
    Vec<Uuid> => "uuid[]",
);

impl<T: ColumnType> ColumnType for Option<T> {
    const SQL_TYPE: &'static str = T::SQL_TYPE;
    const NULLABLE: bool = true;
    type Value = T::Value;
}

/// A value that can be sent to PostgreSQL as a query parameter.
pub trait Bindable: for<'q> Encode<'q, Postgres> + Type<Postgres> + Send + 'static {}

impl<T> Bindable for T where T: for<'q> Encode<'q, Postgres> + Type<Postgres> + Send + 'static {}

/// The query under construction that generated code appends to.
pub type SqlBuilder = QueryBuilder<'static, Postgres>;

/// A struct mapped to an application table. Implemented by `#[derive(Model)]`.
pub trait Model: Sized + Send + Unpin + for<'r> FromRow<'r, PgRow> {
    /// What an insertion takes: the fields, without those the database fills
    /// (`id`, `created_at`, `updated_at`) and without `shop`, which the rows
    /// of a shop fill.
    type New: Send;

    const TABLE: &'static str;
    /// The columns, in the order of the fields.
    const COLUMNS: &'static [&'static str];
    /// Whether a row belongs to a shop.
    const HAS_SHOP: bool;

    /// The table definition, for the schema push and the migrations.
    fn table() -> Table;

    fn id(&self) -> Uuid;

    /// Appends `(columns) VALUES (values)`.
    #[doc(hidden)]
    fn push_insert(new: Self::New, shop: Option<String>, query: &mut SqlBuilder);

    /// Appends the `column = value` assignments of an update.
    #[doc(hidden)]
    fn push_update(&self, query: &mut SqlBuilder);
}

/// A column of the table of `M`, holding values of type `T`.
///
/// `#[derive(Model)]` declares one constant per field: `Review::RATING`.
pub struct Column<M, T> {
    name: &'static str,
    marker: PhantomData<fn() -> (M, T)>,
}

impl<M, T> Clone for Column<M, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M, T> Copy for Column<M, T> {}

impl<M, T> Column<M, T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            marker: PhantomData,
        }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    fn condition(self, push: impl FnOnce(&mut SqlBuilder, String) + Send + 'static) -> Condition<M> {
        let column = quote(self.name);
        Condition {
            push: Box::new(move |query| push(query, column)),
            marker: PhantomData,
        }
    }

    pub fn is_null(self) -> Condition<M> {
        self.condition(|query, column| {
            query.push(format!("{column} IS NULL"));
        })
    }

    pub fn is_not_null(self) -> Condition<M> {
        self.condition(|query, column| {
            query.push(format!("{column} IS NOT NULL"));
        })
    }

    /// Smallest first.
    pub fn asc(self) -> Order<M> {
        Order {
            sql: format!("{} ASC", quote(self.name)),
            marker: PhantomData,
        }
    }

    /// Largest first.
    pub fn desc(self) -> Order<M> {
        Order {
            sql: format!("{} DESC", quote(self.name)),
            marker: PhantomData,
        }
    }
}

impl<M, T: Bindable> Column<M, T> {
    fn compare(self, operator: &'static str, value: T) -> Condition<M> {
        self.condition(move |query, column| {
            query.push(format!("{column} {operator} "));
            query.push_bind(value);
        })
    }

    pub fn eq(self, value: impl Into<T>) -> Condition<M> {
        self.compare("=", value.into())
    }

    pub fn ne(self, value: impl Into<T>) -> Condition<M> {
        self.compare("<>", value.into())
    }

    pub fn gt(self, value: impl Into<T>) -> Condition<M> {
        self.compare(">", value.into())
    }

    pub fn gte(self, value: impl Into<T>) -> Condition<M> {
        self.compare(">=", value.into())
    }

    pub fn lt(self, value: impl Into<T>) -> Condition<M> {
        self.compare("<", value.into())
    }

    pub fn lte(self, value: impl Into<T>) -> Condition<M> {
        self.compare("<=", value.into())
    }
}

impl<M, T: Bindable + PgHasArrayType> Column<M, T> {
    /// The column holds one of the values. No row matches an empty list.
    pub fn is_in<V: Into<T>>(self, values: impl IntoIterator<Item = V>) -> Condition<M> {
        let values: Vec<T> = values.into_iter().map(Into::into).collect();
        // One array parameter: the statement is the same whatever the number of values.
        self.condition(move |query, column| {
            query.push(format!("{column} = ANY("));
            query.push_bind(values);
            query.push(")");
        })
    }
}

/// A test on one column, built from a [`Column`]: `Review::RATING.gte(4)`.
pub struct Condition<M> {
    push: Box<dyn FnOnce(&mut SqlBuilder) + Send>,
    marker: PhantomData<fn() -> M>,
}

/// A sort key, built from a [`Column`]: `Review::CREATED_AT.desc()`.
pub struct Order<M> {
    sql: String,
    marker: PhantomData<fn() -> M>,
}

fn id_is<M>(id: Uuid) -> Condition<M> {
    Column::<M, Uuid>::new("id").eq(id)
}

fn column_list<M: Model>() -> String {
    M::COLUMNS
        .iter()
        .map(|column| quote(column))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The rows of a model: those of one shop, or all of them.
///
/// Every operation on the rows of a shop is limited to that shop: a row of
/// another shop is not found, updated or deleted, even by its id.
pub struct Rows<'a, M: Model> {
    db: &'a PgPool,
    shop: Option<String>,
    marker: PhantomData<fn() -> M>,
}

impl<'a, M: Model> Rows<'a, M> {
    /// Called by the entry points `#[derive(Model)]` generates.
    #[doc(hidden)]
    pub fn new(db: &'a PgPool, shop: Option<String>) -> Self {
        Self {
            db,
            shop,
            marker: PhantomData,
        }
    }

    /// A query on these rows, to filter, sort and limit.
    pub fn query(&self) -> Query<'a, M> {
        Query {
            db: self.db,
            shop: self.shop.clone(),
            conditions: Vec::new(),
            order: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    pub fn filter(&self, condition: Condition<M>) -> Query<'a, M> {
        self.query().filter(condition)
    }

    pub fn order_by(&self, order: Order<M>) -> Query<'a, M> {
        self.query().order_by(order)
    }

    pub fn limit(&self, limit: i64) -> Query<'a, M> {
        self.query().limit(limit)
    }

    pub async fn all(&self) -> Result<Vec<M>> {
        self.query().all().await
    }

    pub async fn first(&self) -> Result<Option<M>> {
        self.query().first().await
    }

    pub async fn count(&self) -> Result<i64> {
        self.query().count().await
    }

    /// The row with this id, when it is one of these rows.
    pub async fn find(&self, id: Uuid) -> Result<Option<M>> {
        self.filter(id_is(id)).first().await
    }

    /// Inserts a row and returns it as stored, with the values the database filled.
    pub async fn insert(&self, new: M::New) -> Result<M> {
        if M::HAS_SHOP && self.shop.is_none() {
            bail!(
                "[openshop] A row of \"{}\" belongs to a shop: insert it through `for_shop`",
                M::TABLE
            );
        }
        let mut query = SqlBuilder::new(format!("INSERT INTO {} ", quote(M::TABLE)));
        M::push_insert(new, self.shop.clone(), &mut query);
        query.push(format!(" RETURNING {}", column_list::<M>()));
        Ok(query.build_query_as::<M>().fetch_one(self.db).await?)
    }

    /// Writes every field of `row` except `id`, `shop` and `created_at`, and
    /// sets `updated_at` to now. Returns the row as stored, or `None` when it
    /// is not one of these rows.
    pub async fn save(&self, row: &M) -> Result<Option<M>> {
        let mut query = SqlBuilder::new(format!("UPDATE {} SET ", quote(M::TABLE)));
        row.push_update(&mut query);
        Query::push_where(self.shop.clone(), vec![id_is::<M>(row.id())], &mut query);
        query.push(format!(" RETURNING {}", column_list::<M>()));
        Ok(query.build_query_as::<M>().fetch_optional(self.db).await?)
    }

    /// Deletes the row with this id. Returns whether it was one of these rows.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        Ok(self.filter(id_is(id)).delete().await? > 0)
    }
}

/// Rows of a model that match conditions, in an order, within limits.
pub struct Query<'a, M: Model> {
    db: &'a PgPool,
    shop: Option<String>,
    conditions: Vec<Condition<M>>,
    order: Vec<Order<M>>,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl<M: Model> Query<'_, M> {
    /// Keeps the rows that also satisfy `condition`.
    pub fn filter(mut self, condition: Condition<M>) -> Self {
        self.conditions.push(condition);
        self
    }

    /// Sorts by `order`, after the orders already given.
    pub fn order_by(mut self, order: Order<M>) -> Self {
        self.order.push(order);
        self
    }

    pub fn limit(mut self, limit: i64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn offset(mut self, offset: i64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Appends ` WHERE …` for the shop and the conditions.
    fn push_where(shop: Option<String>, conditions: Vec<Condition<M>>, query: &mut SqlBuilder) {
        let mut separator = " WHERE ";
        if let Some(shop) = shop {
            query.push(separator);
            query.push("\"shop\" = ");
            query.push_bind(shop);
            separator = " AND ";
        }
        for condition in conditions {
            query.push(separator);
            (condition.push)(query);
            separator = " AND ";
        }
    }

    fn select(self) -> SqlBuilder {
        let mut query = SqlBuilder::new(format!("SELECT {} FROM {}", column_list::<M>(), quote(M::TABLE)));
        Self::push_where(self.shop, self.conditions, &mut query);
        if !self.order.is_empty() {
            let order: Vec<String> = self.order.into_iter().map(|order| order.sql).collect();
            query.push(format!(" ORDER BY {}", order.join(", ")));
        }
        if let Some(limit) = self.limit {
            query.push(" LIMIT ");
            query.push_bind(limit);
        }
        if let Some(offset) = self.offset {
            query.push(" OFFSET ");
            query.push_bind(offset);
        }
        query
    }

    pub async fn all(self) -> Result<Vec<M>> {
        let db = self.db;
        Ok(self.select().build_query_as::<M>().fetch_all(db).await?)
    }

    /// The first row in the given order.
    pub async fn first(self) -> Result<Option<M>> {
        let db = self.db;
        Ok(self.limit(1).select().build_query_as::<M>().fetch_optional(db).await?)
    }

    /// The number of rows that match the conditions. The order and the limits do not count.
    pub async fn count(self) -> Result<i64> {
        let mut query = SqlBuilder::new(format!("SELECT count(*) FROM {}", quote(M::TABLE)));
        Self::push_where(self.shop, self.conditions, &mut query);
        Ok(query.build_query_scalar::<i64>().fetch_one(self.db).await?)
    }

    /// Deletes the rows that match the conditions and returns how many there were.
    pub async fn delete(self) -> Result<u64> {
        if self.limit.is_some() || self.offset.is_some() {
            bail!(
                "[openshop] Deleting rows of \"{}\" does not take a limit or an offset: filter the rows to delete",
                M::TABLE
            );
        }
        let mut query = SqlBuilder::new(format!("DELETE FROM {}", quote(M::TABLE)));
        Self::push_where(self.shop, self.conditions, &mut query);
        Ok(query.build().execute(self.db).await?.rows_affected())
    }
}
