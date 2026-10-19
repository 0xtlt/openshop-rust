//! `#[input]`: a struct as what a flow, an MCP tool or an admin function receives.

use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

/// Whether the type states how its fields are named outside Rust: a
/// `rename_all` among the options of a `#[serde(…)]`. Not `rename_all_fields`,
/// nor the text `"rename_all"` as the value of another option.
fn names_its_fields(item: &DeriveInput) -> bool {
    let mut named = false;
    for attribute in item.attrs.iter().filter(|attribute| attribute.path().is_ident("serde")) {
        // An option serde cannot read is serde's to report.
        let _ = attribute.parse_nested_meta(|meta| {
            named |= meta.path.is_ident("rename_all");
            if meta.input.peek(syn::Token![=]) {
                meta.value()?.parse::<syn::Expr>()?;
            } else if meta.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in meta.input);
                content.parse::<TokenStream>()?;
            }
            Ok(())
        });
    }
    named
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
    fn only_a_rename_all_option_names_the_fields() {
        for (attributes, named) in [
            (quote!(#[serde(rename_all = "snake_case")]), true),
            (quote!(#[serde(deny_unknown_fields, rename_all = "kebab-case")]), true),
            (
                quote!(#[serde(rename_all(serialize = "snake_case", deserialize = "snake_case"))]),
                true,
            ),
            (quote!(#[serde(rename_all = "snake_case")] #[serde(default)]), true),
            (quote!(#[serde(rename_all_fields = "snake_case")]), false),
            (quote!(#[serde(rename = "rename_all")]), false),
            (quote!(#[serde(alias = "rename_all", default)]), false),
            (quote!(#[doc = "rename_all"]), false),
            (quote!(#[derive(Debug)]), false),
        ] {
            let item: DeriveInput = syn::parse2(quote!(#attributes struct Input;)).unwrap();
            assert_eq!(names_its_fields(&item), named, "{attributes}");
        }
    }

    #[test]
    fn refuses_arguments() {
        let error = input(quote!(strict), parse_quote! { struct Input; }).unwrap_err();
        assert_eq!(error.to_string(), "`#[input]` takes no argument");
    }
}
