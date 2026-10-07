//! Procedural macros of the OpenShop framework. Use them through the
//! `openshop` crate, which re-exports them and documents them.

mod admin;
mod app;
mod block;
mod function;
mod model;
mod provider;
mod util;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use openshop_graphql::codegen::{DATA_TYPE, VARIABLES_TYPE, data_types, variable_type, variables_types};
use openshop_graphql::{Operation, PROJECT_SCHEMA_PATH, SCHEMA_ENV, Schema, Variable, analyze};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Attribute, Expr, Ident, LitStr, Token, Visibility};

/// `graphql!("query …", name = value, …)` or `graphql! { pub mod name = "query …"; }`.
enum Input {
    Expression {
        query: LitStr,
        variables: Vec<(Ident, Expr)>,
    },
    Module {
        attributes: Vec<Attribute>,
        visibility: Visibility,
        name: Ident,
        query: LitStr,
    },
}

impl Parse for Input {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attributes = input.call(Attribute::parse_outer)?;
        let visibility: Visibility = input.parse()?;

        if input.peek(Token![mod]) {
            input.parse::<Token![mod]>()?;
            let name: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let query: LitStr = input.parse()?;
            input.parse::<Option<Token![;]>>()?;
            if !input.is_empty() {
                return Err(input.error("expected the end of the `graphql!` module"));
            }
            return Ok(Self::Module {
                attributes,
                visibility,
                name,
                query,
            });
        }

        if let Some(attribute) = attributes.first() {
            return Err(syn::Error::new(
                attribute.span(),
                "attributes need the module form: `mod name = \"…\";`",
            ));
        }
        if !matches!(visibility, Visibility::Inherited) {
            return Err(syn::Error::new(
                visibility.span(),
                "a visibility needs the module form: `pub mod name = \"…\";`",
            ));
        }
        let query: LitStr = input
            .parse()
            .map_err(|error| syn::Error::new(error.span(), "expected the GraphQL operation as a string literal"))?;
        let mut variables = Vec::new();
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let name: Ident = input
                .parse()
                .map_err(|error| syn::Error::new(error.span(), "expected a variable: `name = value`"))?;
            input.parse::<Token![=]>()?;
            variables.push((name, input.parse()?));
        }
        Ok(Self::Expression { query, variables })
    }
}

/// Parsed schema files by path, with the modification time they were read at.
type SchemaCache = HashMap<PathBuf, (Option<SystemTime>, Arc<Schema>)>;

/// The schema operations are checked against.
enum LoadedSchema {
    /// The one shipped with the framework.
    Bundled,
    /// The one of the crate being compiled.
    File { path: PathBuf, schema: Arc<Schema> },
}

impl LoadedSchema {
    /// The schema of the crate being compiled, else the bundled one.
    fn load() -> Result<Self, String> {
        let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
        let named = std::env::var_os(SCHEMA_ENV);
        Ok(match schema_file(manifest_dir.as_deref(), named.as_deref())? {
            Some(path) => Self::File {
                schema: read_schema(&path)?,
                path,
            },
            None => Self::Bundled,
        })
    }

    fn schema(&self) -> &Schema {
        match self {
            Self::Bundled => Schema::bundled(),
            Self::File { schema, .. } => schema,
        }
    }

    /// Makes the compiler rebuild the crate when what the expansion depends on changes.
    fn tracking(&self) -> TokenStream {
        // One operation of the crate is enough to track the file, which is large.
        static TRACKED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
        let file = match self {
            Self::File { path, .. } => {
                let mut tracked = TRACKED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                (!tracked.contains(path)).then(|| {
                    tracked.push(path.clone());
                    let path = path.to_string_lossy().to_string();
                    quote!(
                        const _: &[u8] = ::core::include_bytes!(#path);
                    )
                })
            }
            Self::Bundled => None,
        };
        quote! {
            const _: ::core::option::Option<&str> = ::core::option_env!(#SCHEMA_ENV);
            #file
        }
    }

    /// Checks an operation, pointing a refusal at its text.
    fn check(&self, query: &LitStr) -> syn::Result<Operation> {
        analyze(self.schema(), &query.value()).map_err(|error| {
            syn::Error::new(
                query.span(),
                format!(
                    "{error}\n(checked against the Shopify Admin API {})",
                    self.schema().api_version
                ),
            )
        })
    }
}

/// The schema file of the crate being compiled, if it has one: the file named
/// by `OPENSHOP_GRAPHQL_SCHEMA`, else `graphql/admin.schema.json` next to its
/// `Cargo.toml` when that file exists.
fn schema_file(manifest_dir: Option<&Path>, named: Option<&OsStr>) -> Result<Option<PathBuf>, String> {
    let resolve = |path: &Path| match manifest_dir {
        Some(dir) if path.is_relative() => dir.join(path),
        _ => path.to_path_buf(),
    };
    match named.filter(|value| !value.is_empty()) {
        Some(named) => {
            let path = resolve(Path::new(named));
            if !path.is_file() {
                return Err(format!("{SCHEMA_ENV} names `{}`, which is not a file", path.display()));
            }
            Ok(Some(path))
        }
        None => Ok(Some(resolve(Path::new(PROJECT_SCHEMA_PATH))).filter(|path| path.is_file())),
    }
}

/// Reads a schema file. One compilation expands many operations: each file is
/// parsed once, and again when it changes.
fn read_schema(file: &Path) -> Result<Arc<Schema>, String> {
    static CACHE: Mutex<Option<SchemaCache>> = Mutex::new(None);
    let modified = std::fs::metadata(file).and_then(|metadata| metadata.modified()).ok();
    let mut cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some((cached, schema)) = cache.get(file)
        && *cached == modified
    {
        return Ok(schema.clone());
    }
    let json = std::fs::read_to_string(file)
        .map_err(|error| format!("Cannot read the schema `{}`: {error}", file.display()))?;
    let schema = Schema::from_json(&json).map(Arc::new).map_err(|error| {
        format!(
            "`{}`: {error}. Run `openshop codegen` to download it again.",
            file.display()
        )
    })?;
    cache.insert(file.to_path_buf(), (modified, schema.clone()));
    Ok(schema)
}

/// `graphql!("…", name = value)`: the operation as a value.
fn expand_expression(schema: &LoadedSchema, query: &LitStr, variables: &[(Ident, Expr)]) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    let operation = schema.check(query)?;

    // Each value with the variable it is for.
    let mut given: Vec<(&Variable, &Expr)> = Vec::new();
    for (name, value) in variables {
        let Some(declared) = operation.variables.iter().find(|variable| name == &variable.name) else {
            let declared: Vec<String> = operation
                .variables
                .iter()
                .map(|variable| format!("${}", variable.name))
                .collect();
            let known = if declared.is_empty() {
                "it declares no variables".to_string()
            } else {
                format!("it declares {}", declared.join(", "))
            };
            return Err(syn::Error::new(
                name.span(),
                format!("The operation has no variable `${name}`: {known}"),
            ));
        };
        if given.iter().any(|(earlier, _)| earlier.name == declared.name) {
            return Err(syn::Error::new(
                name.span(),
                format!("Variable `${name}` is given twice"),
            ));
        }
        given.push((declared, value));
    }
    if let Some(missing) = operation
        .variables
        .iter()
        .find(|variable| variable.is_required() && !given.iter().any(|(declared, _)| declared.name == variable.name))
    {
        return Err(syn::Error::new(
            query.span(),
            format!(
                "Variable `${}: {}` needs a value: add `{} = …` after the operation",
                missing.name, missing.ty, missing.name
            ),
        ));
    }

    // Evaluated before the generated types are in scope, so that the
    // values cannot name them by accident.
    let locals: Vec<Ident> = (0..given.len())
        .map(|index| Ident::new(&format!("__openshop_variable_{index}"), Span::mixed_site()))
        .collect();
    let values = given.iter().map(|(_, value)| value);
    let setters = given.iter().zip(&locals).map(|((declared, value), local)| {
        let key = &declared.name;
        match variable_type(declared, &krate) {
            Some(ty) => quote_spanned!(value.span()=> .variable::<#ty, _>(#key, #local)),
            None => quote_spanned!(value.span()=> .json_variable(#key, #local)),
        }
    });

    let tracking = schema.tracking();
    let types = data_types(&operation, &krate);
    let data = format_ident!("{}", DATA_TYPE);
    Ok(quote! {{
        #(let #locals = #values;)*
        {
            #tracking
            #types
            #krate::shopify::GraphqlOperation::<#data>::new(#query)
                #(#setters)*
        }
    }})
}

/// `graphql! { mod name = "…"; }`: the operation as a module that names its types.
fn expand_module(
    schema: &LoadedSchema,
    attributes: &[Attribute],
    visibility: &Visibility,
    name: &Ident,
    query: &LitStr,
) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    let operation = schema.check(query)?;
    let data = format_ident!("{}", DATA_TYPE);
    let (variables, function) = if operation.variables.is_empty() {
        let function = quote! {
            /// The operation, for `ShopifyClient::execute`.
            pub fn operation() -> #krate::shopify::GraphqlOperation<#data> {
                #krate::shopify::GraphqlOperation::new(QUERY)
            }
        };
        (TokenStream::new(), function)
    } else {
        let variables = format_ident!("{}", VARIABLES_TYPE);
        let function = quote! {
            /// The operation with its variables, for `ShopifyClient::execute`.
            pub fn operation(variables: #variables) -> #krate::shopify::GraphqlOperation<#data> {
                #krate::shopify::GraphqlOperation::new(QUERY).variables(variables)
            }
        };
        (variables_types(schema.schema(), &operation, &krate), function)
    };

    let tracking = schema.tracking();
    let types = data_types(&operation, &krate);
    Ok(quote! {
        #(#attributes)*
        #visibility mod #name {
            #tracking

            /// The text of the operation.
            pub const QUERY: &str = #query;

            #types
            #variables
            #function
        }
    })
}

fn expand(input: Input) -> syn::Result<TokenStream> {
    let schema = LoadedSchema::load().map_err(|message| syn::Error::new(Span::call_site(), message))?;
    match input {
        Input::Expression { query, variables } => expand_expression(&schema, &query, &variables),
        Input::Module {
            attributes,
            visibility,
            name,
            query,
        } => expand_module(&schema, &attributes, &visibility, &name, &query),
    }
}

/// See `openshop::graphql!`.
#[proc_macro]
pub fn graphql(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as Input);
    expand(input).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// See `openshop::Model`.
#[proc_macro_derive(Model, attributes(model))]
pub fn derive_model(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    model::expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// See `openshop::ProviderConfig`.
#[proc_macro_derive(ProviderConfig, attributes(field))]
pub fn derive_provider_config(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    provider::derive_config(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// See `openshop::provider`.
#[proc_macro_attribute]
pub fn provider(arguments: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let name = match syn::parse::<LitStr>(arguments) {
        Ok(name) => name,
        Err(error) => {
            return syn::Error::new(
                error.span(),
                "expected the name of the provider: `#[provider(\"warehouse\")]`",
            )
            .into_compile_error()
            .into();
        }
    };
    let item = syn::parse_macro_input!(item as syn::ItemImpl);
    provider::provider(name, item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// See `openshop::FunctionConfig`.
#[proc_macro_derive(FunctionConfig, attributes(function, field))]
pub fn derive_function_config(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    function::derive(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// An attribute on a function that declares it as a building block.
macro_rules! block_attribute {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[proc_macro_attribute]
        pub fn $name(arguments: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
            let arguments = syn::parse_macro_input!(arguments as util::Arguments);
            let item = syn::parse_macro_input!(item as syn::ItemFn);
            block::$name(arguments, item)
                .unwrap_or_else(syn::Error::into_compile_error)
                .into()
        }
    };
}

block_attribute!(
    /// See `openshop::flow`.
    flow
);
block_attribute!(
    /// See `openshop::webhook`.
    webhook
);
block_attribute!(
    /// See `openshop::proxy`.
    proxy
);
block_attribute!(
    /// See `openshop::route`.
    route
);
block_attribute!(
    /// See `openshop::mcp_tool`.
    mcp_tool
);
block_attribute!(
    /// See `openshop::mcp_resource`.
    mcp_resource
);

/// See `openshop::admin_page`.
#[proc_macro_attribute]
pub fn admin_page(arguments: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let path = match syn::parse::<LitStr>(arguments) {
        Ok(path) => path,
        Err(error) => {
            return syn::Error::new(
                error.span(),
                "expected the path of the page: `#[admin_page(\"/reviews\")]`",
            )
            .into_compile_error()
            .into();
        }
    };
    let item = syn::parse_macro_input!(item as syn::ItemImpl);
    admin::admin_page(path, item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// See `openshop::app!`.
#[proc_macro]
pub fn app(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let declaration = syn::parse_macro_input!(input as app::Declaration);
    app::expand(declaration).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"{"apiVersion":"2025-10","query":"QueryRoot","types":{
"QueryRoot":{"o":{"f":{"ping":"String"}}},
"String":"s"
}}"#;

    /// An empty directory removed when the test ends.
    struct Project(PathBuf);

    impl Project {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("openshop-macros-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, path: &str, content: &str) -> PathBuf {
            let file = self.0.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, content).unwrap();
            file
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_project_without_a_schema_file_uses_the_bundled_schema() {
        let project = Project::new("bundled");
        assert_eq!(schema_file(Some(&project.0), None), Ok(None));
        assert_eq!(schema_file(Some(&project.0), Some(OsStr::new(""))), Ok(None));
        assert_eq!(schema_file(None, None), Ok(None));
    }

    #[test]
    fn the_project_file_and_the_environment_select_another_schema() {
        let project = Project::new("selected");
        let downloaded = project.write("graphql/admin.schema.json", SCHEMA);
        let other = project.write("schemas/unstable.json", SCHEMA);

        assert_eq!(schema_file(Some(&project.0), None), Ok(Some(downloaded)));
        // The environment wins, relative to the manifest or absolute.
        let named = OsStr::new("schemas/unstable.json");
        assert_eq!(schema_file(Some(&project.0), Some(named)), Ok(Some(other.clone())));
        assert_eq!(schema_file(None, Some(other.as_os_str())), Ok(Some(other)));

        let missing = project.0.join("missing.json");
        assert_eq!(
            schema_file(Some(&project.0), Some(OsStr::new("missing.json"))),
            Err(format!(
                "OPENSHOP_GRAPHQL_SCHEMA names `{}`, which is not a file",
                missing.display()
            ))
        );
    }

    #[test]
    fn schema_files_are_read_once_and_again_when_they_change() {
        let project = Project::new("read");
        let file = project.write("graphql/admin.schema.json", SCHEMA);

        let first = read_schema(&file).unwrap();
        assert_eq!(first.api_version, "2025-10");
        assert!(Arc::ptr_eq(&first, &read_schema(&file).unwrap()));

        // A later modification time, as after `openshop codegen`.
        std::fs::write(&file, SCHEMA.replace("2025-10", "2026-01")).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(read_schema(&file).unwrap().api_version, "2026-01");
    }

    #[test]
    fn a_malformed_schema_file_says_how_to_restore_it() {
        let project = Project::new("malformed");
        let file = project.write("graphql/admin.schema.json", "{");

        let error = read_schema(&file).unwrap_err();
        assert!(
            error.starts_with(&format!("`{}`: Invalid schema file: ", file.display())),
            "{error}"
        );
        assert!(
            error.ends_with(". Run `openshop codegen` to download it again."),
            "{error}"
        );

        let error = read_schema(&project.0.join("absent.json")).unwrap_err();
        assert!(error.starts_with("Cannot read the schema `"), "{error}");
    }

    #[test]
    fn parses_both_forms() {
        let expression: Input = syn::parse_str(r#""{ shop { name } }", first = 1, after = cursor,"#).unwrap();
        let Input::Expression { query, variables } = expression else {
            panic!("an expression");
        };
        assert_eq!(query.value(), "{ shop { name } }");
        let names: Vec<String> = variables.iter().map(|(name, _)| name.to_string()).collect();
        assert_eq!(names, ["first", "after"]);

        let module: Input = syn::parse_str("#[doc = \"Docs.\"] pub mod shop_name = \"{ shop { name } }\";").unwrap();
        let Input::Module { attributes, name, .. } = module else {
            panic!("a module");
        };
        assert_eq!(name, "shop_name");
        assert_eq!(attributes.len(), 1);

        for (source, message) in [
            ("first = 1", "expected the GraphQL operation as a string literal"),
            (r#""{ shop { name } }", 1"#, "expected a variable: `name = value`"),
            (
                r#"pub "{ shop { name } }""#,
                "a visibility needs the module form: `pub mod name = \"…\";`",
            ),
            (
                r#"mod shop = "{ shop { name } }"; 1"#,
                "expected the end of the `graphql!` module",
            ),
        ] {
            let error = syn::parse_str::<Input>(source).err().expect(source).to_string();
            assert_eq!(error, message, "{source}");
        }
    }
}
