//! Compile-time support for typed Shopify Admin GraphQL operations.
//!
//! [`analyze`] checks an operation against a [`Schema`] and describes the
//! shape of its variables and of its result; [`codegen`] turns that
//! description into Rust types. The `graphql!` macro of the `openshop` crate
//! is a thin layer over both, and the `openshop codegen` command uses
//! [`Schema::from_introspection`] to store the schema of another API version.

pub mod analyze;
pub mod codegen;
pub mod schema;

pub use analyze::{
    Error, FieldBase, Operation, OperationKind, Scalar, SelectedField, Selection, Variable, VariableBase, analyze,
};
pub use schema::{Field, INTROSPECTION_QUERY, InputValue, Schema, SchemaError, TypeDef, TypeRef};

/// Where an application keeps the schema of another API version than the
/// bundled one, relative to its `Cargo.toml`.
pub const PROJECT_SCHEMA_PATH: &str = "graphql/admin.schema.json";
/// The environment variable naming another schema file for `graphql!`.
pub const SCHEMA_ENV: &str = "OPENSHOP_GRAPHQL_SCHEMA";
