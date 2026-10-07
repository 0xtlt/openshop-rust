//! `#[derive(Model)]`: a struct as an application table.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Data, DataStruct, DeriveInput, Fields, Ident, LitStr, Type, Visibility};

use crate::util::{constant_ident, plain_name, set, set_value};

/// Identifiers PostgreSQL would truncate are refused: two long names could collide.
const MAX_IDENTIFIER: usize = 63;

/// The columns every model can have, filled by the database or by the rows of a shop.
const CONVENTIONAL: [&str; 4] = ["id", "shop", "created_at", "updated_at"];

struct IndexSpec {
    /// The fields, as written.
    fields: Vec<Ident>,
    name: Option<LitStr>,
    unique: bool,
    span: Span,
}

#[derive(Default)]
struct StructOptions {
    table: Option<LitStr>,
    renamed_from: Option<LitStr>,
    indexes: Vec<IndexSpec>,
}

/// What a field references: another model, or a table by its name.
enum Target {
    Model(syn::Path),
    Table(LitStr),
}

struct Reference {
    target: Target,
    /// The referenced column, when it is not `id`.
    column: Option<LitStr>,
}

/// The actions PostgreSQL knows for `ON DELETE`.
const ON_DELETE: [&str; 5] = ["cascade", "restrict", "set null", "set default", "no action"];

#[derive(Default)]
struct FieldOptions {
    column: Option<LitStr>,
    default: Option<LitStr>,
    sql_type: Option<LitStr>,
    renamed_from: Option<LitStr>,
    references: Option<Reference>,
    on_delete: Option<LitStr>,
    index: bool,
    unique: bool,
}

struct ModelField {
    ident: Ident,
    ty: Type,
    visibility: Visibility,
    /// The name of the column.
    column: String,
    options: FieldOptions,
}

impl ModelField {
    fn is_conventional(&self) -> bool {
        CONVENTIONAL.contains(&self.column.as_str())
    }
}

/// `"name"`, as `openshop::db` quotes identifiers.
fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn parse_index(meta: &syn::meta::ParseNestedMeta, unique: bool) -> syn::Result<IndexSpec> {
    let mut spec = IndexSpec {
        fields: Vec::new(),
        name: None,
        unique,
        span: meta.path.span(),
    };
    meta.parse_nested_meta(|inner| {
        if inner.path.is_ident("name") {
            return set_value(&mut spec.name, &inner);
        }
        match inner.path.get_ident() {
            Some(field) => {
                spec.fields.push(field.clone());
                Ok(())
            }
            None => Err(inner.error("expected a field name")),
        }
    })?;
    if spec.fields.is_empty() {
        return Err(meta.error("expected the fields of the index: `index(shop, product_id)`"));
    }
    Ok(spec)
}

/// `references(Review)`, `references("reviews")`, each with an optional `column = "…"`.
fn parse_reference(meta: &syn::meta::ParseNestedMeta) -> syn::Result<Reference> {
    let content;
    syn::parenthesized!(content in meta.input);
    let target = if content.peek(LitStr) {
        Target::Table(content.parse()?)
    } else {
        Target::Model(content.parse().map_err(|error| {
            syn::Error::new(
                error.span(),
                "expected the referenced model, `references(Review)`, or its table, `references(\"reviews\")`",
            )
        })?)
    };
    let mut column = None;
    if content.parse::<Option<syn::Token![,]>>()?.is_some() && !content.is_empty() {
        let option: Ident = content.parse()?;
        if option != "column" {
            return Err(syn::Error::new(option.span(), "expected `column = \"…\"`"));
        }
        content.parse::<syn::Token![=]>()?;
        column = Some(content.parse()?);
        content.parse::<Option<syn::Token![,]>>()?;
    }
    if !content.is_empty() {
        return Err(content.error("expected the end of `references(…)`"));
    }
    Ok(Reference { target, column })
}

fn struct_options(input: &DeriveInput) -> syn::Result<StructOptions> {
    let mut options = StructOptions::default();
    for attribute in input
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("model"))
    {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                set_value(&mut options.table, &meta)
            } else if meta.path.is_ident("renamed_from") {
                set_value(&mut options.renamed_from, &meta)
            } else if meta.path.is_ident("index") {
                options.indexes.push(parse_index(&meta, false)?);
                Ok(())
            } else if meta.path.is_ident("unique") {
                options.indexes.push(parse_index(&meta, true)?);
                Ok(())
            } else {
                Err(meta.error("unknown model option; expected `table`, `renamed_from`, `index(…)` or `unique(…)`"))
            }
        })?;
    }
    Ok(options)
}

fn field_options(field: &syn::Field) -> syn::Result<FieldOptions> {
    let mut options = FieldOptions::default();
    for attribute in field
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("model"))
    {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("column") {
                set_value(&mut options.column, &meta)
            } else if meta.path.is_ident("default") {
                set_value(&mut options.default, &meta)
            } else if meta.path.is_ident("sql_type") {
                set_value(&mut options.sql_type, &meta)
            } else if meta.path.is_ident("renamed_from") {
                set_value(&mut options.renamed_from, &meta)
            } else if meta.path.is_ident("references") {
                set(&mut options.references, &meta, parse_reference)
            } else if meta.path.is_ident("on_delete") {
                set_value(&mut options.on_delete, &meta)
            } else if meta.path.is_ident("index") {
                options.index = true;
                Ok(())
            } else if meta.path.is_ident("unique") {
                options.unique = true;
                Ok(())
            } else {
                Err(meta.error(
                    "unknown field option; expected `column`, `default`, `sql_type`, `renamed_from`, `references(…)`, `on_delete`, `index` or `unique`",
                ))
            }
        })?;
    }
    if let Some(action) = &options.on_delete {
        let normalized = action
            .value()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !ON_DELETE.contains(&normalized.as_str()) {
            return Err(syn::Error::new(
                action.span(),
                "`on_delete` is \"cascade\", \"restrict\", \"set null\", \"set default\" or \"no action\"",
            ));
        }
        if options.references.is_none() {
            return Err(syn::Error::new(action.span(), "`on_delete` goes with `references(…)`"));
        }
    }
    Ok(options)
}

fn index_name(
    table: &str,
    columns: &[String],
    spec_name: Option<&LitStr>,
    unique: bool,
    span: Span,
) -> syn::Result<String> {
    if let Some(name) = spec_name {
        return Ok(name.value());
    }
    let name = format!(
        "{table}_{}_{}",
        columns.join("_"),
        if unique { "unique" } else { "idx" }
    );
    if name.len() > MAX_IDENTIFIER {
        return Err(syn::Error::new(
            span,
            format!(
                "the index would be named `{name}`, longer than the {MAX_IDENTIFIER} characters PostgreSQL keeps: give it a `name = \"…\"`"
            ),
        ));
    }
    Ok(name)
}

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    let name = &input.ident;
    let visibility = &input.vis;

    let Data::Struct(DataStruct {
        fields: Fields::Named(named),
        ..
    }) = &input.data
    else {
        return Err(syn::Error::new(
            name.span(),
            "`Model` is derived on a struct with named fields",
        ));
    };
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(input.generics.span(), "a model cannot be generic"));
    }

    let options = struct_options(&input)?;
    let Some(table) = &options.table else {
        return Err(syn::Error::new(
            name.span(),
            "a model names its table: add `#[model(table = \"…\")]`",
        ));
    };
    let table_name = table.value();

    let mut fields = Vec::new();
    for field in &named.named {
        let ident = field.ident.clone().expect("a named field");
        let options = field_options(field)?;
        let column = match &options.column {
            Some(column) => column.value(),
            None => plain_name(&ident),
        };
        fields.push(ModelField {
            ident,
            ty: field.ty.clone(),
            visibility: field.vis.clone(),
            column,
            options,
        });
    }
    for (index, field) in fields.iter().enumerate() {
        if fields[..index].iter().any(|earlier| earlier.column == field.column) {
            return Err(syn::Error::new(
                field.ident.span(),
                format!("two fields are stored in the column `{}`", field.column),
            ));
        }
        if field.is_conventional() {
            let options = &field.options;
            let customized = options.default.is_some()
                || options.sql_type.is_some()
                || options.renamed_from.is_some()
                || options.references.is_some();
            if customized {
                return Err(syn::Error::new(
                    field.ident.span(),
                    format!(
                        "`{}` is a conventional column: its type and default are fixed",
                        field.column
                    ),
                ));
            }
        }
    }
    let Some(id_field) = fields.iter().find(|field| field.column == "id") else {
        return Err(syn::Error::new(
            name.span(),
            "a model needs its primary key: add `pub id: Uuid`",
        ));
    };
    let id_field = &id_field.ident;
    let has_shop = fields.iter().any(|field| field.column == "shop");
    let has_updated_at = fields.iter().any(|field| field.column == "updated_at");

    // ── Table definition ────────────────────────────────────────────
    let column_names: Vec<&str> = fields.iter().map(|field| field.column.as_str()).collect();
    let columns = fields.iter().map(|field| {
        let column = &field.column;
        let ty = &field.ty;
        match column.as_str() {
            // `ColumnDef` has a constructor named after each conventional column.
            conventional if field.is_conventional() => {
                let constructor = format_ident!("{}", conventional);
                quote!(#krate::db::ColumnDef::#constructor())
            }
            _ => {
                let sql_type = match &field.options.sql_type {
                    Some(sql_type) => quote!(#sql_type),
                    None => quote!(<#ty as #krate::db::ColumnType>::SQL_TYPE),
                };
                let default = field.options.default.as_ref().map(|default| quote!(.default(#default)));
                let renamed = field
                    .options
                    .renamed_from
                    .as_ref()
                    .map(|previous| quote!(.renamed_from(#previous)));
                quote! {
                    #krate::db::ColumnDef::new(#column, #sql_type)
                        .nullable(<#ty as #krate::db::ColumnType>::NULLABLE)
                        #default
                        #renamed
                }
            }
        }
    });

    let mut indexes = Vec::new();
    for spec in &options.indexes {
        let mut columns = Vec::new();
        for ident in &spec.fields {
            let Some(field) = fields.iter().find(|field| &field.ident == ident) else {
                return Err(syn::Error::new(
                    ident.span(),
                    format!("`{name}` has no field `{ident}`"),
                ));
            };
            columns.push(field.column.clone());
        }
        let index = index_name(&table_name, &columns, spec.name.as_ref(), spec.unique, spec.span)?;
        indexes.push((index, columns, spec.unique));
    }
    for field in &fields {
        for (wanted, unique) in [(field.options.index, false), (field.options.unique, true)] {
            if wanted {
                let columns = vec![field.column.clone()];
                let index = index_name(&table_name, &columns, None, unique, field.ident.span())?;
                indexes.push((index, columns, unique));
            }
        }
    }
    let indexes = indexes.iter().map(|(index, columns, unique)| {
        let method = if *unique {
            format_ident!("unique_index")
        } else {
            format_ident!("index")
        };
        quote!(.#method(#index, &[#(#columns),*]))
    });
    let foreign_keys = fields.iter().filter_map(|field| {
        let reference = field.options.references.as_ref()?;
        let column = &field.column;
        let table = match &reference.target {
            Target::Model(model) => quote!(<#model as #krate::db::Record>::TABLE),
            Target::Table(table) => quote!(#table),
        };
        let referenced = reference.column.as_ref().map(|column| quote!(.references(#column)));
        let on_delete = field
            .options
            .on_delete
            .as_ref()
            .map(|action| quote!(.on_delete(#action)));
        Some(quote! {
            .with_foreign_key(#krate::db::ForeignKeyDef::new(#column, #table) #referenced #on_delete)
        })
    });
    let renamed_table = options
        .renamed_from
        .as_ref()
        .map(|previous| quote!(.renamed_from(#previous)));

    // ── Row mapping ─────────────────────────────────────────────────
    let from_row = fields.iter().map(|field| {
        let ident = &field.ident;
        let column = &field.column;
        quote!(#ident: #krate::sqlx::Row::try_get(row, #column)?)
    });

    // ── Typed columns ───────────────────────────────────────────────
    let constants = fields.iter().map(|field| {
        let constant = constant_ident(&field.ident);
        let column = &field.column;
        let ty = &field.ty;
        let field_visibility = &field.visibility;
        let doc = format!(" The column `{column}`.");
        quote! {
            #[doc = #doc]
            #field_visibility const #constant: #krate::db::Column<Self, <#ty as #krate::db::ColumnType>::Value> =
                #krate::db::Column::new(#column);
        }
    });

    // ── Insertion ───────────────────────────────────────────────────
    let new_name = format_ident!("New{}", name);
    let inserted: Vec<&ModelField> = fields.iter().filter(|field| !field.is_conventional()).collect();
    let new_fields = inserted.iter().map(|field| {
        let ident = &field.ident;
        let ty = &field.ty;
        let field_visibility = &field.visibility;
        if field.options.default.is_some() {
            let doc = " `None` leaves the default of the column.";
            quote! {
                #[doc = #doc]
                #field_visibility #ident: ::core::option::Option<<#ty as #krate::db::ColumnType>::Value>,
            }
        } else {
            quote!(#field_visibility #ident: #ty,)
        }
    });
    let mut insert_columns: Vec<String> = Vec::new();
    if has_shop {
        insert_columns.push(quote_identifier("shop"));
    }
    insert_columns.extend(inserted.iter().map(|field| quote_identifier(&field.column)));
    let push_insert = if insert_columns.is_empty() {
        quote! {
            let _ = (new, shop);
            query.push("DEFAULT VALUES");
        }
    } else {
        let head = format!("({}) VALUES (", insert_columns.join(", "));
        let shop_value = has_shop.then(|| quote!(values.push_bind(shop.unwrap_or_default());));
        let ignore_shop = (!has_shop).then(|| quote!(let _ = shop;));
        let values = inserted.iter().map(|field| {
            let ident = &field.ident;
            if field.options.default.is_some() {
                quote! {
                    match new.#ident {
                        ::core::option::Option::Some(value) => {
                            values.push_bind(value);
                        }
                        ::core::option::Option::None => {
                            values.push("DEFAULT");
                        }
                    }
                }
            } else {
                quote!(values.push_bind(new.#ident);)
            }
        });
        quote! {
            #ignore_shop
            query.push(#head);
            {
                let mut values = query.separated(", ");
                #shop_value
                #(#values)*
            }
            query.push(")");
        }
    };

    // ── Update ──────────────────────────────────────────────────────
    let mut assignments: Vec<TokenStream> = inserted
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let assignment = format!("{} = ", quote_identifier(&field.column));
            quote! {
                assignments.push(#assignment);
                assignments.push_bind_unseparated(::core::clone::Clone::clone(&self.#ident));
            }
        })
        .collect();
    if has_updated_at {
        assignments.push(quote!(assignments.push("\"updated_at\" = now()");));
    }
    if assignments.is_empty() {
        // An update needs an assignment: this one changes nothing.
        assignments.push(quote!(assignments.push("\"id\" = \"id\"");));
    }

    // ── Entry points ────────────────────────────────────────────────
    let entry_points = if has_shop {
        quote! {
            /// The rows of one shop. Reads, updates and deletions never reach
            /// the rows of another shop, and insertions belong to this one.
            #visibility fn for_shop<'a>(db: &'a #krate::PgPool, shop: &str) -> #krate::db::Rows<'a, Self> {
                #krate::db::Rows::new(db, ::core::option::Option::Some(::std::string::ToString::to_string(shop)))
            }

            /// The rows of every shop, for maintenance work. Prefer `for_shop`
            /// wherever a shop is known.
            #visibility fn all_shops(db: &#krate::PgPool) -> #krate::db::Rows<'_, Self> {
                #krate::db::Rows::new(db, ::core::option::Option::None)
            }
        }
    } else {
        quote! {
            /// The rows of the table.
            #visibility fn rows(db: &#krate::PgPool) -> #krate::db::Rows<'_, Self> {
                #krate::db::Rows::new(db, ::core::option::Option::None)
            }
        }
    };

    let new_doc =
        format!(" What inserting a [`{name}`] takes: its fields, without those the database fills and without `shop`.");

    Ok(quote! {
        #[doc = #new_doc]
        #[derive(Debug, Clone)]
        #visibility struct #new_name {
            #(#new_fields)*
        }

        impl #krate::Declared for #name {
            type Kind = #krate::declare::kind::Model;

            fn register(app: #krate::OpenShopBuilder) -> #krate::OpenShopBuilder {
                app.model(<Self as #krate::db::Record>::model())
            }
        }

        impl #krate::db::Record for #name {
            type New = #new_name;

            const TABLE: &'static str = #table;
            const COLUMNS: &'static [&'static str] = &[#(#column_names),*];
            const HAS_SHOP: bool = #has_shop;

            fn model() -> #krate::db::Model {
                #krate::db::Model::new(#table)
                    #(.with_column(#columns))*
                    #(#indexes)*
                    #(#foreign_keys)*
                    #renamed_table
            }

            fn id(&self) -> #krate::sqlx::types::Uuid {
                self.#id_field
            }

            fn push_insert(
                new: Self::New,
                shop: ::core::option::Option<::std::string::String>,
                query: &mut #krate::db::SqlBuilder,
            ) {
                #push_insert
            }

            fn push_update(&self, query: &mut #krate::db::SqlBuilder) {
                let mut assignments = query.separated(", ");
                #(#assignments)*
            }
        }

        impl<'r> #krate::sqlx::FromRow<'r, #krate::sqlx::postgres::PgRow> for #name {
            fn from_row(row: &'r #krate::sqlx::postgres::PgRow) -> ::core::result::Result<Self, #krate::sqlx::Error> {
                ::core::result::Result::Ok(Self {
                    #(#from_row,)*
                })
            }
        }

        impl #name {
            #(#constants)*

            #entry_points
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn refusal(input: DeriveInput) -> String {
        match expand(input) {
            Ok(_) => panic!("accepted"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn accepts_a_struct_with_every_option() {
        let generated = expand(parse_quote! {
            #[model(table = "reviews", renamed_from = "ratings", index(shop, product_id), unique(shop, title, name = "one_title"))]
            pub struct Review {
                pub id: Uuid,
                pub shop: String,
                pub created_at: DateTime<Utc>,
                pub updated_at: DateTime<Utc>,
                #[model(index)]
                pub product_id: String,
                #[model(column = "headline", unique)]
                pub title: String,
                #[model(default = "0", sql_type = "smallint", renamed_from = "stars")]
                pub rating: i16,
            }
        })
        .unwrap()
        .to_string();

        for expected in [
            "pub struct NewReview",
            "const TABLE : & 'static str = \"reviews\"",
            "const HAS_SHOP : bool = true",
            ". renamed_from (\"ratings\")",
            ". index (\"reviews_shop_product_id_idx\" , & [\"shop\" , \"product_id\"])",
            ". unique_index (\"one_title\" , & [\"shop\" , \"headline\"])",
            ". index (\"reviews_product_id_idx\" , & [\"product_id\"])",
            ". unique_index (\"reviews_headline_unique\" , & [\"headline\"])",
            "ColumnDef :: new (\"rating\" , \"smallint\")",
            ". default (\"0\")",
            ". renamed_from (\"stars\")",
            "pub const TITLE",
            "pub fn for_shop",
            "pub fn all_shops",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }
        assert!(!generated.contains("fn rows"), "{generated}");
    }

    #[test]
    fn references_become_foreign_keys() {
        let generated = expand(parse_quote! {
            #[model(table = "review_votes")]
            struct ReviewVote {
                id: Uuid,
                #[model(references(Review), on_delete = "cascade")]
                review_id: Uuid,
                #[model(references(crate::models::Author, column = "code"), on_delete = "Set  Null")]
                author_code: Option<String>,
                #[model(references("legacy_orders"))]
                order_id: Uuid,
            }
        })
        .unwrap()
        .to_string();

        for expected in [
            "ForeignKeyDef :: new (\"review_id\" , < Review as :: openshop :: db :: Record > :: TABLE) . on_delete (\"cascade\")",
            "ForeignKeyDef :: new (\"author_code\" , < crate :: models :: Author as :: openshop :: db :: Record > :: TABLE) . references (\"code\") . on_delete (\"Set  Null\")",
            "ForeignKeyDef :: new (\"order_id\" , \"legacy_orders\")",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }

        assert_eq!(
            refusal(
                parse_quote! { #[model(table = "t")] struct T { id: Uuid, #[model(references(Review), on_delete = "delete")] review_id: Uuid } }
            ),
            "`on_delete` is \"cascade\", \"restrict\", \"set null\", \"set default\" or \"no action\""
        );
        assert_eq!(
            refusal(
                parse_quote! { #[model(table = "t")] struct T { id: Uuid, #[model(on_delete = "cascade")] review_id: Uuid } }
            ),
            "`on_delete` goes with `references(…)`"
        );
        assert_eq!(
            refusal(
                parse_quote! { #[model(table = "t")] struct T { id: Uuid, #[model(references(Review, on = "id"))] review_id: Uuid } }
            ),
            "expected `column = \"…\"`"
        );
        assert_eq!(
            refusal(
                parse_quote! { #[model(table = "t")] struct T { id: Uuid, #[model(references(Review))] shop: String } }
            ),
            "`shop` is a conventional column: its type and default are fixed"
        );
    }

    #[test]
    fn a_struct_without_shop_gets_plain_rows() {
        let generated = expand(parse_quote! {
            #[model(table = "settings")]
            struct Setting {
                id: Uuid,
            }
        })
        .unwrap()
        .to_string();
        assert!(generated.contains("fn rows"), "{generated}");
        assert!(generated.contains("const HAS_SHOP : bool = false"), "{generated}");
        assert!(!generated.contains("fn for_shop"), "{generated}");
        // Nothing to insert or update but what the database fills.
        assert!(generated.contains("\"DEFAULT VALUES\""), "{generated}");
        assert!(generated.contains("\"\\\"id\\\" = \\\"id\\\"\""), "{generated}");
    }

    #[test]
    fn refuses_what_cannot_be_a_table() {
        assert_eq!(
            refusal(parse_quote! { struct Review { id: Uuid } }),
            "a model names its table: add `#[model(table = \"…\")]`"
        );
        assert_eq!(
            refusal(parse_quote! { #[model(table = "reviews")] struct Review { rating: i32 } }),
            "a model needs its primary key: add `pub id: Uuid`"
        );
        assert_eq!(
            refusal(parse_quote! { #[model(table = "reviews")] struct Review(Uuid); }),
            "`Model` is derived on a struct with named fields"
        );
        assert_eq!(
            refusal(parse_quote! { #[model(table = "reviews")] enum Review { A } }),
            "`Model` is derived on a struct with named fields"
        );
        assert_eq!(
            refusal(parse_quote! { #[model(table = "reviews")] struct Review<T> { id: Uuid, value: T } }),
            "a model cannot be generic"
        );
    }

    #[test]
    fn refuses_invalid_options() {
        assert_eq!(
            refusal(parse_quote! { #[model(table = "reviews", tabel = "x")] struct Review { id: Uuid } }),
            "unknown model option; expected `table`, `renamed_from`, `index(…)` or `unique(…)`"
        );
        assert_eq!(
            refusal(
                parse_quote! { #[model(table = "reviews")] struct Review { id: Uuid, #[model(defualt = "0")] rating: i32 } }
            ),
            "unknown field option; expected `column`, `default`, `sql_type`, `renamed_from`, `references(…)`, `on_delete`, `index` or `unique`"
        );
        assert_eq!(
            refusal(parse_quote! { #[model(table = "reviews", table = "other")] struct Review { id: Uuid } }),
            "given twice"
        );
        assert_eq!(
            refusal(
                parse_quote! { #[model(table = "reviews", index(shop, product))] struct Review { id: Uuid, shop: String } }
            ),
            "`Review` has no field `product`"
        );
        assert_eq!(
            refusal(
                parse_quote! { #[model(table = "reviews")] struct Review { id: Uuid, #[model(default = "now()")] created_at: DateTime<Utc> } }
            ),
            "`created_at` is a conventional column: its type and default are fixed"
        );
        assert_eq!(
            refusal(
                parse_quote! { #[model(table = "reviews")] struct Review { id: Uuid, a: i32, #[model(column = "a")] b: i32 } }
            ),
            "two fields are stored in the column `a`"
        );
        let long = refusal(parse_quote! {
            #[model(table = "a_table_with_a_rather_long_name", index(a_first_long_column_name, a_second_long_column_name))]
            struct Review { id: Uuid, a_first_long_column_name: i32, a_second_long_column_name: i32 }
        });
        assert!(
            long.contains("longer than the 63 characters PostgreSQL keeps: give it a `name = \"…\"`"),
            "{long}"
        );
    }
}
