//! `#[input]`: a struct as what a flow, an MCP tool or an admin function receives.

use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

/// Whether the type states how its fields are named outside Rust.
fn names_its_fields(item: &DeriveInput) -> bool {
    item.attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("serde"))
        .any(|attribute| match &attribute.meta {
            syn::Meta::List(list) => list.tokens.to_string().contains("rename_all"),
            _ => false,
        })
}

pub fn input(arguments: TokenStream, item: DeriveInput) -> syn::Result<TokenStream> {
    if !arguments.is_empty() {
        return Err(syn::Error::new_spanned(arguments, "`#[input]` takes no argument"));
    }
    // Fields are camel case outside Rust, like the names of flows and the
    // fields of a provider, unless the type says otherwise.
    let rename = (!names_its_fields(&item)).then(|| quote!(rename_all = "camelCase",));

    Ok(quote! {
        #[derive(::openshop::serde::Deserialize, ::openshop::serde::Serialize, ::openshop::schemars::JsonSchema)]
        #[serde(#rename crate = "::openshop::serde")]
        #[schemars(crate = "::openshop::schemars")]
        #item
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn generated(item: DeriveInput) -> String {
        input(TokenStream::new(), item).unwrap().to_string()
    }

    #[test]
    fn an_input_reads_writes_and_describes_itself_in_camel_case() {
        let generated = generated(parse_quote! {
            #[derive(Debug)]
            pub struct CountVariantsInput {
                #[schemars(length(min = 1))]
                pub product_id: String,
            }
        });
        for expected in [
            "# [derive (:: openshop :: serde :: Deserialize , :: openshop :: serde :: Serialize , :: openshop :: schemars :: JsonSchema)]",
            "# [serde (rename_all = \"camelCase\" , crate = \"::openshop::serde\")]",
            "# [schemars (crate = \"::openshop::schemars\")]",
            "# [derive (Debug)] pub struct CountVariantsInput { # [schemars (length (min = 1))] pub product_id : String , }",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }
    }

    #[test]
    fn a_naming_rule_of_the_type_is_kept() {
        let generated = generated(parse_quote! {
            #[serde(rename_all = "snake_case")]
            pub enum Mode { FullSync, Delta }
        });
        assert!(
            generated.contains("# [serde (crate = \"::openshop::serde\")]"),
            "{generated}"
        );
        assert!(!generated.contains("camelCase"), "{generated}");
    }

    #[test]
    fn refuses_arguments() {
        let error = input(quote!(strict), parse_quote! { struct Input; }).unwrap_err();
        assert_eq!(error.to_string(), "`#[input]` takes no argument");
    }
}
