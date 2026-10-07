//! Helpers shared by the macros.

use proc_macro2::{Ident, Span, TokenStream};
use syn::ext::IdentExt;
use syn::meta::ParseNestedMeta;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Attribute, Expr, LitStr, Token};

/// Sets an option of an attribute with what `parse` reads, refusing a second value.
pub fn set<T>(
    slot: &mut Option<T>,
    meta: &ParseNestedMeta,
    parse: impl FnOnce(&ParseNestedMeta) -> syn::Result<T>,
) -> syn::Result<()> {
    if slot.is_some() {
        return Err(meta.error("given twice"));
    }
    *slot = Some(parse(meta)?);
    Ok(())
}

/// Sets an option written `name = value`.
pub fn set_value<T: Parse>(slot: &mut Option<T>, meta: &ParseNestedMeta) -> syn::Result<()> {
    set(slot, meta, |meta| meta.value()?.parse())
}

/// The name of an identifier, without the `r#` of a raw one.
pub fn plain_name(ident: &Ident) -> String {
    ident.unraw().to_string()
}

/// The constant named after a field or a method: `push_orders` → `PUSH_ORDERS`.
pub fn constant_ident(ident: &Ident) -> Ident {
    Ident::new(&plain_name(ident).to_uppercase(), ident.span())
}

/// `api_url` → `apiUrl`: the name a field or a function has outside Rust.
pub fn camel_case(name: &str) -> String {
    let mut camel = String::new();
    let mut capitalize = false;
    for character in name.chars() {
        if character == '_' {
            capitalize = !camel.is_empty();
        } else if capitalize {
            camel.extend(character.to_uppercase());
            capitalize = false;
        } else {
            camel.push(character);
        }
    }
    camel
}

/// The text of the documentation comments, as one paragraph per blank line.
pub fn doc_text(attributes: &[Attribute]) -> String {
    let lines: Vec<String> = attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("doc"))
        .filter_map(|attribute| match &attribute.meta {
            syn::Meta::NameValue(meta) => match &meta.value {
                Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(text),
                    ..
                }) => Some(text.value().trim().to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    lines
        .split(|line| line.is_empty())
        .map(|paragraph| paragraph.join(" "))
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One argument of an attribute.
pub enum Argument {
    /// `"orders/create"`.
    Text(LitStr),
    /// `liquid`.
    Word(Ident),
    /// `timeout = Duration::from_secs(60)`.
    Value(Ident, Expr),
    /// `modes(automatic, code)`.
    List(Ident, TokenStream),
}

impl Argument {
    pub fn span(&self) -> Span {
        match self {
            Self::Text(text) => text.span(),
            Self::Word(word) | Self::Value(word, _) | Self::List(word, _) => word.span(),
        }
    }

    /// The name of a word or of an option.
    pub fn name(&self) -> Option<String> {
        match self {
            Self::Text(_) => None,
            Self::Word(word) | Self::Value(word, _) | Self::List(word, _) => Some(plain_name(word)),
        }
    }

    /// A flag: `starts_at` is `true`, `starts_at = false` is what it says.
    pub fn flag(&self) -> syn::Result<TokenStream> {
        match self {
            Self::Word(_) => Ok(quote::quote!(true)),
            Self::Value(_, value) => Ok(quote::quote!(#value)),
            _ => Err(syn::Error::new(
                self.span(),
                "expected a flag, alone or `= true` / `= false`",
            )),
        }
    }
}

/// The arguments of an attribute, separated by commas.
pub struct Arguments(pub Vec<Argument>);

impl Parse for Arguments {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut arguments = Vec::new();
        while !input.is_empty() {
            if input.peek(LitStr) {
                arguments.push(Argument::Text(input.parse()?));
            } else {
                let name = input.call(Ident::parse_any).map_err(|error| {
                    syn::Error::new(error.span(), "expected a string, a word or a `name = value` option")
                })?;
                if input.peek(Token![=]) {
                    input.parse::<Token![=]>()?;
                    arguments.push(Argument::Value(name, input.parse()?));
                } else if input.peek(syn::token::Paren) {
                    let content;
                    syn::parenthesized!(content in input);
                    arguments.push(Argument::List(name, content.parse()?));
                } else {
                    arguments.push(Argument::Word(name));
                }
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(Self(arguments))
    }
}

/// The value of an option written `name = "text"`.
pub fn text(value: &Expr) -> syn::Result<LitStr> {
    match value {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(text),
            ..
        }) => Ok(text.clone()),
        _ => Err(syn::Error::new(value.span(), "expected a string")),
    }
}

/// The values of a list written `name("a", CONSTANT)`.
pub fn values(list: TokenStream) -> syn::Result<Vec<Expr>> {
    let values = syn::parse::Parser::parse2(syn::punctuated::Punctuated::<Expr, Token![,]>::parse_terminated, list)?;
    Ok(values.into_iter().collect())
}

/// Sets an option of an attribute, refusing a second value.
pub fn set_once<T>(slot: &mut Option<T>, argument: &Argument, value: T) -> syn::Result<()> {
    if slot.is_some() {
        return Err(syn::Error::new(argument.span(), "given twice"));
    }
    *slot = Some(value);
    Ok(())
}
