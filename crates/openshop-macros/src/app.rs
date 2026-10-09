//! `app!`: the declaration of an application.

use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Expr, Ident, Token, Type};

use crate::util::plain_name;

/// The lists of blocks, with the kind of block each one takes.
const LISTS: [(&str, &str); 10] = [
    ("providers", "Provider"),
    ("flows", "Flow"),
    ("functions", "Function"),
    ("models", "Model"),
    ("admin_pages", "AdminPage"),
    ("webhooks", "Webhook"),
    ("proxies", "Proxy"),
    ("routes", "Route"),
    ("mcp_tools", "McpTool"),
    ("mcp_resources", "McpResource"),
];

enum Entry {
    /// `flows: [sync_orders, count_variants]`.
    Blocks { kind: &'static str, blocks: Vec<Type> },
    /// `on_error: report`: a setting of the builder.
    Setting { key: Ident, value: Box<Expr> },
}

pub struct Declaration(Vec<Entry>);

impl Parse for Declaration {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut entries = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        while !input.is_empty() {
            let key: Ident = input
                .parse()
                .map_err(|error| syn::Error::new(error.span(), "expected an entry: `flows: [sync_orders]`"))?;
            let name = plain_name(&key);
            if keys.contains(&name) {
                return Err(syn::Error::new(key.span(), format!("`{name}` is given twice")));
            }
            input.parse::<Token![:]>()?;

            match LISTS.iter().find(|(list, _)| *list == name) {
                Some((_, kind)) => {
                    if !input.peek(syn::token::Bracket) {
                        return Err(input.error(format!("expected the list of blocks: `{name}: [first, second]`")));
                    }
                    let content;
                    syn::bracketed!(content in input);
                    let blocks = content.parse_terminated(Type::parse, Token![,])?;
                    entries.push(Entry::Blocks {
                        kind,
                        blocks: blocks.into_iter().collect(),
                    });
                }
                None => entries.push(Entry::Setting {
                    key,
                    value: input.parse()?,
                }),
            }
            keys.push(name);

            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(Self(entries))
    }
}

pub fn expand(declaration: Declaration) -> TokenStream {
    let krate = quote!(::openshop);
    let calls = declaration.0.iter().map(|entry| match entry {
        Entry::Blocks { kind, blocks } => {
            let kind = format_ident!("{}", kind);
            let calls = blocks
                .iter()
                .map(|block| quote_spanned!(block.span()=> .add_as::<#krate::declare::kind::#kind, #block>()));
            quote!(#(#calls)*)
        }
        Entry::Setting { key, value } => quote_spanned!(key.span()=> .#key(#value)),
    });
    // A setting can fail: `shopify: shopify_apps()?`.
    quote! {
        #krate::App::new(|| #krate::Result::Ok(#krate::OpenShopConfig::builder() #(#calls)*))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expanded(tokens: TokenStream) -> String {
        expand(syn::parse2(tokens).unwrap()).to_string()
    }

    fn refusal(tokens: TokenStream) -> String {
        match syn::parse2::<Declaration>(tokens) {
            Ok(_) => panic!("accepted"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn lists_add_blocks_of_their_kind_and_the_rest_sets_the_builder() {
        let generated = expanded(quote! {
            providers: [Warehouse],
            flows: [sync_orders, flows::count_variants],
            webhooks: [],
            crons: [Cron::new("0 2 * * *", sync_orders::NAME)],
            on_error: report,
        });
        assert_eq!(
            generated,
            quote! {
                ::openshop::App::new(|| ::openshop::Result::Ok(::openshop::OpenShopConfig::builder()
                    .add_as::<::openshop::declare::kind::Provider, Warehouse>()
                    .add_as::<::openshop::declare::kind::Flow, sync_orders>()
                    .add_as::<::openshop::declare::kind::Flow, flows::count_variants>()
                    .crons([Cron::new("0 2 * * *", sync_orders::NAME)])
                    .on_error(report)))
            }
            .to_string()
        );
        assert_eq!(
            expanded(quote!()),
            quote!(::openshop::App::new(|| ::openshop::Result::Ok(
                ::openshop::OpenShopConfig::builder()
            )))
            .to_string()
        );
    }

    #[test]
    fn refuses_what_is_not_an_entry() {
        assert_eq!(
            refusal(quote!(flows: sync_orders)),
            "expected the list of blocks: `flows: [first, second]`"
        );
        assert_eq!(refusal(quote!(flows: [a], flows: [b])), "`flows` is given twice");
        assert_eq!(refusal(quote!([a])), "expected an entry: `flows: [sync_orders]`");
    }
}
