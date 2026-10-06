//! Helpers shared by the macros.

use proc_macro2::Ident;
use syn::ext::IdentExt;
use syn::meta::ParseNestedMeta;
use syn::parse::Parse;

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
