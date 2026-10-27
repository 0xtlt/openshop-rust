//! Procedural macros of the OpenShop framework. Use them through the
//! `openshop` crate, which re-exports them and documents them.
#![forbid(unsafe_code)]

mod admin;
mod app;
mod block;
mod function;
mod input;
mod model;
mod provider;
mod test;
mod util;

use syn::LitStr;

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
    let name = match syn::parse::<Option<LitStr>>(arguments) {
        Ok(name) => name,
        Err(error) => {
            return syn::Error::new(
                error.span(),
                "expected the name of the provider, `#[provider(\"warehouse\")]`, or nothing to name it after the struct",
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
    let arguments = syn::parse_macro_input!(arguments as util::Arguments);
    let item = syn::parse_macro_input!(item as syn::ItemImpl);
    admin::admin_page(arguments, item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// See `openshop::input`.
#[proc_macro_attribute]
pub fn input(arguments: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let item = syn::parse_macro_input!(item as syn::DeriveInput);
    input::input(arguments.into(), item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// See `openshop::test`.
#[proc_macro_attribute]
pub fn test(arguments: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let item = syn::parse_macro_input!(item as syn::ItemFn);
    test::test(arguments.into(), item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// See `openshop::app!`.
#[proc_macro]
pub fn app(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let declaration = syn::parse_macro_input!(input as app::Declaration);
    app::expand(declaration).into()
}
