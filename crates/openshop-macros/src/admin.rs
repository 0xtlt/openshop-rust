//! `#[admin_page]`: the `impl` block of a type as the server side of an admin page.

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Expr, FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, LitStr, ReturnType, Type};

use crate::util::{Argument, Arguments, camel_case, plain_name, set_once};

const MARKERS: [&str; 3] = ["access", "loader", "action"];

/// Takes the marker of a function out of its attributes, with its arguments.
fn take_marker(function: &mut ImplItemFn) -> syn::Result<Option<(String, Vec<Argument>)>> {
    let Some(index) = function
        .attrs
        .iter()
        .position(|attribute| MARKERS.iter().any(|marker| attribute.path().is_ident(marker)))
    else {
        return Ok(None);
    };
    let attribute = function.attrs.remove(index);
    let marker = attribute.path().get_ident().map(plain_name).unwrap_or_default();
    let arguments = match &attribute.meta {
        syn::Meta::Path(_) => Vec::new(),
        _ => attribute.parse_args::<Arguments>()?.0,
    };
    if let Some(second) = function
        .attrs
        .iter()
        .find(|attribute| MARKERS.iter().any(|marker| attribute.path().is_ident(marker)))
    {
        return Err(syn::Error::new(
            second.span(),
            "a function of an admin page is one of `#[access]`, `#[loader]` or `#[action]`",
        ));
    }
    Ok(Some((marker, arguments)))
}

/// The type of the input of a page function, `async fn(ctx) -> Result<…>` or
/// `async fn(ctx, input: Input) -> Result<…>`.
fn input_type<'a>(function: &'a ImplItemFn, marker: &str) -> syn::Result<Option<&'a Type>> {
    let signature = &function.sig;
    let shape = match marker {
        "access" => "async fn(ctx: AdminServerContext) -> Result<bool>".to_string(),
        _ => format!(
            "async fn(ctx: AdminServerContext) -> Result<…>` or `async fn(ctx: AdminServerContext, input: Input) -> Result<…>` for `#[{marker}]"
        ),
    };
    let refusal = || syn::Error::new(signature.ident.span(), format!("expected `{shape}`"));
    let allowed = if marker == "access" { 1 } else { 2 };
    if signature.asyncness.is_none()
        || matches!(signature.output, ReturnType::Default)
        || signature.receiver().is_some()
        || !signature.generics.params.is_empty()
        || signature.inputs.is_empty()
        || signature.inputs.len() > allowed
    {
        return Err(refusal());
    }
    Ok(match signature.inputs.iter().nth(1) {
        Some(FnArg::Typed(argument)) => Some(&*argument.ty),
        _ => None,
    })
}

pub fn admin_page(path: LitStr, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    if let Some((_, implemented, _)) = &item.trait_ {
        return Err(syn::Error::new(
            implemented.span(),
            "`#[admin_page]` goes on the `impl` block of the page type itself",
        ));
    }
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new(item.generics.span(), "an admin page cannot be generic"));
    }
    if !path.value().starts_with('/') {
        return Err(syn::Error::new(path.span(), "the path of a page starts with `/`"));
    }
    let self_type = (*item.self_ty).clone();

    let mut access: Option<Ident> = None;
    let mut functions = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for implemented in &mut item.items {
        let ImplItem::Fn(function) = implemented else {
            continue;
        };
        let Some((marker, arguments)) = take_marker(function)? else {
            continue;
        };
        let input = input_type(function, &marker)?;
        let ident = function.sig.ident.clone();

        if marker == "access" {
            if let Some(argument) = arguments.first() {
                return Err(syn::Error::new(argument.span(), "`#[access]` takes no option"));
            }
            if access.is_some() {
                return Err(syn::Error::new(ident.span(), "an admin page has one `#[access]`"));
            }
            access = Some(ident);
            continue;
        }

        let mut name: Option<LitStr> = None;
        let mut authorize: Option<&Expr> = None;
        for argument in &arguments {
            match argument {
                Argument::Text(text) => set_once(&mut name, argument, text.clone())?,
                Argument::Value(option, value) if option == "authorize" => set_once(&mut authorize, argument, value)?,
                _ => {
                    return Err(syn::Error::new(
                        argument.span(),
                        format!(
                            "unknown `#[{marker}]` option; expected the name of the function or `authorize = <function>`"
                        ),
                    ));
                }
            }
        }
        let name = name.unwrap_or_else(|| LitStr::new(&camel_case(&plain_name(&ident)), ident.span()));
        if names.contains(&name.value()) {
            return Err(syn::Error::new(
                name.span(),
                format!("two functions of the page are named `{}`", name.value()),
            ));
        }
        names.push(name.value());

        // Without an input the function answers whatever it is sent.
        let untyped = quote! {
            |ctx: #krate::AdminServerContext, _input: #krate::Value| async move {
                #krate::Result::<#krate::Value>::Ok(#krate::serde_json::to_value(Self::#ident(ctx).await?)?)
            }
        };
        let definition = match (marker.as_str(), input.is_some()) {
            ("loader", true) => quote!(#krate::define_typed_admin_loader(Self::#ident)),
            ("loader", false) => quote!(#krate::define_admin_loader(#untyped)),
            (_, true) => quote!(#krate::define_typed_admin_action(Self::#ident)),
            (_, false) => quote!(#krate::define_admin_action(#krate::InputSchema::any(), #untyped)),
        };
        let authorize = authorize.map(|authorize| quote!(.authorize(#krate::define_admin_page_access(#authorize))));
        functions.push(quote!(.function(#name, #definition #authorize)));
    }

    let access = access.map(|access| quote!(.access(#krate::define_admin_page_access(Self::#access))));

    Ok(quote! {
        #item

        impl #self_type {
            /// The path of the page in the admin.
            pub const PATH: &'static str = #path;
        }

        impl #krate::Declared for #self_type {
            type Kind = #krate::declare::kind::AdminPage;

            fn register(app: #krate::OpenShopBuilder) -> #krate::OpenShopBuilder {
                app.admin_page(#krate::AdminPage::new(Self::PATH) #access #(#functions)*)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn refusal(item: ItemImpl) -> String {
        match admin_page(parse_quote!("/reviews"), item) {
            Ok(_) => panic!("accepted"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn marked_functions_become_the_page() {
        let generated = admin_page(
            parse_quote!("/reviews"),
            parse_quote! {
                impl ReviewsPage {
                    #[access]
                    async fn staff(ctx: AdminServerContext) -> Result<bool> { Ok(true) }

                    /// Lists reviews.
                    #[loader]
                    async fn list_reviews(ctx: AdminServerContext) -> Result<Value> { todo!() }

                    #[loader("find")]
                    async fn find_review(ctx: AdminServerContext, input: FindInput) -> Result<Review> { todo!() }

                    #[action(authorize = Self::staff)]
                    async fn save_review(ctx: AdminServerContext, input: SaveInput) -> Result<Value> { todo!() }

                    #[action]
                    async fn refresh(ctx: AdminServerContext) -> Result<()> { todo!() }

                    fn helper() -> u32 { 1 }
                }
            },
        )
        .unwrap()
        .to_string();

        for expected in [
            // The block itself is kept, without the markers.
            "# [doc = r\" Lists reviews.\"] async fn list_reviews (ctx : AdminServerContext) -> Result < Value >",
            "pub const PATH : & 'static str = \"/reviews\" ;",
            "impl :: openshop :: Declared for ReviewsPage { type Kind = :: openshop :: declare :: kind :: AdminPage ;",
            "app . admin_page (:: openshop :: AdminPage :: new (Self :: PATH) . access (:: openshop :: define_admin_page_access (Self :: staff))",
            ". function (\"listReviews\" , :: openshop :: define_admin_loader (",
            ":: openshop :: serde_json :: to_value (Self :: list_reviews (ctx) . await ?) ?",
            ". function (\"find\" , :: openshop :: define_typed_admin_loader (Self :: find_review))",
            ". function (\"saveReview\" , :: openshop :: define_typed_admin_action (Self :: save_review) . authorize (:: openshop :: define_admin_page_access (Self :: staff)))",
            ". function (\"refresh\" , :: openshop :: define_admin_action (:: openshop :: InputSchema :: any () ,",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }
        for marker in ["# [access]", "# [loader", "# [action"] {
            assert!(!generated.contains(marker), "{generated}");
        }
        assert!(!generated.contains("\"helper\""), "{generated}");
    }

    #[test]
    fn refuses_functions_a_page_cannot_call() {
        assert_eq!(
            refusal(
                parse_quote! { impl P { #[loader] fn list(ctx: AdminServerContext) -> Result<Value> { todo!() } } }
            ),
            "expected `async fn(ctx: AdminServerContext) -> Result<…>` or `async fn(ctx: AdminServerContext, input: Input) -> Result<…>` for `#[loader]`"
        );
        assert_eq!(
            refusal(
                parse_quote! { impl P { #[action] async fn save(&self, ctx: AdminServerContext) -> Result<Value> { todo!() } } }
            ),
            "expected `async fn(ctx: AdminServerContext) -> Result<…>` or `async fn(ctx: AdminServerContext, input: Input) -> Result<…>` for `#[action]`"
        );
        assert_eq!(
            refusal(
                parse_quote! { impl P { #[access] async fn staff(ctx: AdminServerContext, input: Value) -> Result<bool> { todo!() } } }
            ),
            "expected `async fn(ctx: AdminServerContext) -> Result<bool>`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[access] async fn a(ctx: AdminServerContext) -> Result<bool> { todo!() }
                #[access] async fn b(ctx: AdminServerContext) -> Result<bool> { todo!() }
            } }),
            "an admin page has one `#[access]`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[loader] async fn list(ctx: AdminServerContext) -> Result<Value> { todo!() }
                #[action("list")] async fn save(ctx: AdminServerContext) -> Result<Value> { todo!() }
            } }),
            "two functions of the page are named `list`"
        );
        assert_eq!(
            refusal(
                parse_quote! { impl P { #[loader] #[action] async fn list(ctx: AdminServerContext) -> Result<Value> { todo!() } } }
            ),
            "a function of an admin page is one of `#[access]`, `#[loader]` or `#[action]`"
        );
        assert_eq!(
            refusal(
                parse_quote! { impl P { #[loader(cache = true)] async fn list(ctx: AdminServerContext) -> Result<Value> { todo!() } } }
            ),
            "unknown `#[loader]` option; expected the name of the function or `authorize = <function>`"
        );
        assert_eq!(
            refusal(parse_quote! { impl Display for P { } }),
            "`#[admin_page]` goes on the `impl` block of the page type itself"
        );
        assert_eq!(
            admin_page(parse_quote!("reviews"), parse_quote! { impl P { } })
                .unwrap_err()
                .to_string(),
            "the path of a page starts with `/`"
        );
    }
}
