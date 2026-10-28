//! `#[admin_page]`: the `impl` block of a type as an admin page, its server side and its view.

use proc_macro2::{Span, TokenStream};
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{
    Expr, FnArg, GenericArgument, Ident, ImplItem, ImplItemFn, ItemImpl, LitStr, PathArguments, ReturnType, Type,
    parse_quote,
};

use crate::util::{Argument, Arguments, camel_case, constant_ident, declared, own_names, plain_name, set_once, text};

const MARKERS: [&str; 4] = ["access", "loader", "action", "view"];

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
            "a function of an admin page is one of `#[access]`, `#[loader]`, `#[action]` or `#[view]`",
        ));
    }
    Ok(Some((marker, arguments)))
}

/// The type of the input of a page function, `async fn(ctx) -> Result<…>` or
/// `async fn(ctx, input: Input) -> Result<…>`.
fn input_type<'a>(function: &'a ImplItemFn, marker: &str) -> syn::Result<Option<&'a Type>> {
    let signature = &function.sig;
    let shape = match marker {
        "access" => "async fn(ctx: AdminContext) -> Result<bool>".to_string(),
        _ => format!(
            "async fn(ctx: AdminContext) -> Result<…>` or `async fn(ctx: AdminContext, input: Input) -> Result<…>` for `#[{marker}]"
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

/// Refuses a `#[view]` that is not `fn() -> impl IntoView` or
/// `fn(params: RouteParams) -> impl IntoView`.
fn check_view(function: &ImplItemFn) -> syn::Result<()> {
    let signature = &function.sig;
    if signature.asyncness.is_some()
        || matches!(signature.output, ReturnType::Default)
        || signature.receiver().is_some()
        || !signature.generics.params.is_empty()
        || signature.inputs.len() > 1
    {
        return Err(syn::Error::new(
            signature.ident.span(),
            "expected `fn() -> impl IntoView` or `fn(params: RouteParams) -> impl IntoView` for `#[view]`",
        ));
    }
    Ok(())
}

/// `Output` in `-> Result<Output>`: what the view receives from a loader or an action.
fn output_type(function: &ImplItemFn) -> Option<&Type> {
    let ReturnType::Type(_, returned) = &function.sig.output else {
        return None;
    };
    let Type::Path(returned) = &**returned else {
        return None;
    };
    let result = returned
        .path
        .segments
        .last()
        .filter(|segment| segment.ident == "Result")?;
    let PathArguments::AngleBracketed(arguments) = &result.arguments else {
        return None;
    };
    arguments.args.iter().find_map(|argument| match argument {
        GenericArgument::Type(output) => Some(output),
        _ => None,
    })
}

/// A loader or an action as the view names it: `ReviewsPage::LIST_REVIEWS`.
struct Reference {
    function: Ident,
    kind: Ident,
    name: LitStr,
    input: Option<Type>,
    output: Option<Type>,
    /// Where the function says what it returns.
    returned: Span,
}

pub fn admin_page(arguments: Arguments, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let mut path: Option<LitStr> = None;
    let mut nav: Option<LitStr> = None;
    for argument in &arguments.0 {
        match argument {
            Argument::Text(text) => set_once(&mut path, argument, text.clone())?,
            Argument::Value(option, value) if option == "nav" => set_once(&mut nav, argument, text(value)?)?,
            _ => {
                return Err(syn::Error::new(
                    argument.span(),
                    "unknown page option; expected the path of the page or `nav = \"…\"`",
                ));
            }
        }
    }
    let Some(path) = path else {
        return Err(syn::Error::new(
            Span::call_site(),
            "expected the path of the page: `#[admin_page(\"/reviews\")]`",
        ));
    };
    if let Some((implemented, _)) = &item.trait_ {
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
    let mut view: Option<Ident> = None;
    let mut functions = Vec::new();
    let mut references: Vec<Reference> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for implemented in &mut item.items {
        let ImplItem::Fn(function) = implemented else {
            continue;
        };
        // The admin UI bundle has the view of the page, not what the server runs.
        let on_server: syn::Attribute = parse_quote!(#[cfg(not(target_arch = "wasm32"))]);
        let Some((marker, arguments)) = take_marker(function)? else {
            // Not marked: a helper of what the server runs.
            function.attrs.insert(0, on_server);
            continue;
        };
        let ident = function.sig.ident.clone();

        if marker == "view" {
            if let Some(argument) = arguments.first() {
                return Err(syn::Error::new(argument.span(), "`#[view]` takes no option"));
            }
            if view.is_some() {
                return Err(syn::Error::new(ident.span(), "an admin page has one `#[view]`"));
            }
            check_view(function)?;
            view = Some(ident);
            continue;
        }

        let input = input_type(function, &marker)?.cloned();
        let output = output_type(function).cloned();
        let returned = function.sig.output.span();
        function.attrs.insert(0, on_server);

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
            |ctx: ::openshop::AdminContext, _input: ::openshop::Value| async move {
                ::openshop::Result::<::openshop::Value>::Ok(::openshop::serde_json::to_value(Self::#ident(ctx).await?)?)
            }
        };
        let definition = match (marker.as_str(), input.is_some()) {
            ("loader", true) => quote!(::openshop::builder::AdminFunction::typed_loader(Self::#ident)),
            ("loader", false) => quote!(::openshop::builder::AdminFunction::loader(#untyped)),
            (_, true) => quote!(::openshop::builder::AdminFunction::typed_action(Self::#ident)),
            (_, false) => {
                quote!(::openshop::builder::AdminFunction::action(::openshop::builder::InputSchema::any(), #untyped))
            }
        };
        let authorize = authorize.map(|authorize| quote!(.authorize(#authorize)));
        functions.push(quote!(.function(#name, #definition #authorize)));
        references.push(Reference {
            kind: Ident::new(&marker, ident.span()),
            function: ident,
            name,
            input,
            output,
            returned,
        });
    }

    let access = access.map(|access| quote!(.access(Self::#access)));
    // The entry of the page in the menu of the admin.
    let nav = nav.map(|label| quote!(.nav(#label)));

    // The view calls the loaders and the actions of its page through a
    // constant each, which carries what they take and what they return.
    let mut constants = Vec::new();
    // The expansion declares `PATH`, and a constant per function: not twice.
    let own = own_names(&item);
    if let Some(taken) = own.iter().find(|ident| *ident == &"PATH") {
        return Err(syn::Error::new(
            taken.span(),
            "`PATH` is the path of the page, which `#[admin_page]` declares: give this item another name",
        ));
    }
    if view.is_some() {
        for reference in &references {
            let Reference {
                function,
                kind,
                name,
                input,
                output,
                returned,
            } = reference;
            let constant = constant_ident(function);
            if constant == "PATH" {
                return Err(syn::Error::new(
                    function.span(),
                    "`PATH` is the path of the page: give the function another name",
                ));
            }
            if let Some(taken) = own.iter().find(|ident| ***ident == constant && ***ident != *function) {
                return Err(syn::Error::new(
                    taken.span(),
                    format!(
                        "the view of the page calls `{function}` through the constant `{constant}`: give this item another name"
                    ),
                ));
            }
            let Some(output) = output else {
                return Err(syn::Error::new(
                    *returned,
                    format!("the view of the page reads what `{function}` returns: write it as `-> Result<Output>`"),
                ));
            };
            let input = input.as_ref().map_or_else(|| quote!(()), |input| quote!(#input));
            let doc = format!(
                "The {kind} `{}`, for the view of the page: what it takes, and what it returns.",
                name.value()
            );
            constants.push(quote! {
                #[doc = #doc]
                #[allow(dead_code)]
                pub const #constant: ::openshop::ui::AdminFunctionRef<#input, #output> =
                    ::openshop::ui::AdminFunctionRef::#kind(#name);
            });
        }
    }
    // The paths of the framework keep the span of the attribute, which an
    // error about what the view is given points at too.
    let openshop = quote!(::openshop);
    let page_view = match &view {
        // Refused with one message when the application has not enabled the feature `ui`.
        Some(view) => quote_spanned! {view.span()=>
            #openshop::__view! {
                impl #self_type {
                    #(#constants)*
                }

                impl #openshop::ui::PageView for #self_type {
                    fn page_view() -> ::core::option::Option<#openshop::ui::BundlePage> {
                        ::core::option::Option::Some(#openshop::ui::page_view(Self::PATH, Self::#view))
                    }
                }
            }
        },
        None => TokenStream::new(),
    };
    let declared = declared(
        &self_type,
        "AdminPage",
        quote!(app.admin_page(::openshop::builder::AdminPage::new(Self::PATH) #nav #access #(#functions)*)),
    );

    Ok(quote! {
        #item

        impl #self_type {
            /// The path of the page in the admin.
            pub const PATH: &'static str = #path;
        }

        #[cfg(not(target_arch = "wasm32"))]
        #declared

        #page_view
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
                    async fn staff(ctx: AdminContext) -> Result<bool> { Ok(true) }

                    /// Lists reviews.
                    #[loader]
                    async fn list_reviews(ctx: AdminContext) -> Result<Value> { todo!() }

                    #[loader("find")]
                    async fn find_review(ctx: AdminContext, input: FindInput) -> Result<Review> { todo!() }

                    #[action(authorize = Self::staff)]
                    async fn save_review(ctx: AdminContext, input: SaveInput) -> Result<Value> { todo!() }

                    #[action]
                    async fn refresh(ctx: AdminContext) -> Result<()> { todo!() }

                    fn helper() -> u32 { 1 }
                }
            },
        )
        .unwrap()
        .to_string();

        for expected in [
            // The block itself is kept, without the markers.
            "# [doc = r\" Lists reviews.\"] async fn list_reviews (ctx : AdminContext) -> Result < Value >",
            "pub const PATH : & 'static str = \"/reviews\" ;",
            "impl :: openshop :: declare :: Declared for ReviewsPage { type Kind = :: openshop :: declare :: kind :: AdminPage ;",
            "app . admin_page (:: openshop :: builder :: AdminPage :: new (Self :: PATH) . access (Self :: staff)",
            ". function (\"listReviews\" , :: openshop :: builder :: AdminFunction :: loader (",
            ":: openshop :: serde_json :: to_value (Self :: list_reviews (ctx) . await ?) ?",
            ". function (\"find\" , :: openshop :: builder :: AdminFunction :: typed_loader (Self :: find_review))",
            ". function (\"saveReview\" , :: openshop :: builder :: AdminFunction :: typed_action (Self :: save_review) . authorize (Self :: staff))",
            ". function (\"refresh\" , :: openshop :: builder :: AdminFunction :: action (:: openshop :: builder :: InputSchema :: any () ,",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }
        for marker in ["# [access]", "# [loader", "# [action"] {
            assert!(!generated.contains(marker), "{generated}");
        }
        assert!(!generated.contains("\"helper\""), "{generated}");

        // What the server runs is not compiled into the admin UI bundle.
        for expected in [
            "# [cfg (not (target_arch = \"wasm32\"))] async fn staff (ctx : AdminContext) -> Result < bool >",
            "# [cfg (not (target_arch = \"wasm32\"))] # [doc = r\" Lists reviews.\"] async fn list_reviews",
            "# [cfg (not (target_arch = \"wasm32\"))] impl :: openshop :: declare :: Declared for ReviewsPage",
            "# [cfg (not (target_arch = \"wasm32\"))] fn helper () -> u32 { 1 }",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }
        // Without a view, nothing of the page is for the bundle.
        assert!(!generated.contains(":: openshop :: ui"), "{generated}");
    }

    #[test]
    fn a_view_is_kept_with_what_it_calls() {
        let generated = admin_page(
            parse_quote!("/reviews/:id"),
            parse_quote! {
                impl ReviewPage {
                    #[loader]
                    async fn review(ctx: AdminContext) -> Result<Review> { todo!() }

                    #[action("save")]
                    async fn save_review(ctx: AdminContext, input: SaveReview) -> openshop::Result<Vec<Review>> { todo!() }

                    #[view]
                    fn view(params: RouteParams) -> impl IntoView {
                        use_loader(Self::REVIEW, || ());
                    }
                }
            },
        )
        .unwrap()
        .to_string();

        for expected in [
            // The view is compiled on both sides, without its marker.
            "{ todo ! () } fn view (params : RouteParams) -> impl IntoView {",
            // A constant per function: its kind, its name, what it takes and what it returns.
            "pub const REVIEW : :: openshop :: ui :: AdminFunctionRef < () , Review > = :: openshop :: ui :: AdminFunctionRef :: loader (\"review\") ;",
            "pub const SAVE_REVIEW : :: openshop :: ui :: AdminFunctionRef < SaveReview , Vec < Review > > = :: openshop :: ui :: AdminFunctionRef :: action (\"save\") ;",
            ":: openshop :: __view ! { impl ReviewPage { # [doc = \"The loader `review`, for the view of the page: what it takes, and what it returns.\"]",
            "impl :: openshop :: ui :: PageView for ReviewPage { fn page_view () -> :: core :: option :: Option < :: openshop :: ui :: BundlePage > { :: core :: option :: Option :: Some (:: openshop :: ui :: page_view (Self :: PATH , Self :: view)) } } }",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }
        assert!(!generated.contains("# [view]"), "{generated}");
    }

    #[test]
    fn refuses_views_a_page_cannot_show() {
        assert_eq!(
            refusal(parse_quote! { impl P { #[view] async fn view() -> impl IntoView { } } }),
            "expected `fn() -> impl IntoView` or `fn(params: RouteParams) -> impl IntoView` for `#[view]`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P { #[view] fn view(params: RouteParams, other: u8) -> impl IntoView { } } }),
            "expected `fn() -> impl IntoView` or `fn(params: RouteParams) -> impl IntoView` for `#[view]`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P { #[view] fn view() { } } }),
            "expected `fn() -> impl IntoView` or `fn(params: RouteParams) -> impl IntoView` for `#[view]`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[view] fn list() -> impl IntoView { }
                #[view] fn grid() -> impl IntoView { }
            } }),
            "an admin page has one `#[view]`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P { #[view(lazy)] fn view() -> impl IntoView { } } }),
            "`#[view]` takes no option"
        );
        // The view names its functions by a constant, typed after what they return.
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[loader] async fn list(ctx: AdminContext) -> Listed { todo!() }
                #[view] fn view() -> impl IntoView { }
            } }),
            "the view of the page reads what `list` returns: write it as `-> Result<Output>`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[loader] async fn path(ctx: AdminContext) -> Result<String> { todo!() }
                #[view] fn view() -> impl IntoView { }
            } }),
            "`PATH` is the path of the page: give the function another name"
        );
    }

    #[test]
    fn refuses_what_the_expansion_would_declare_twice() {
        assert_eq!(
            refusal(parse_quote! { impl P { const PATH: &str = "/x"; } }),
            "`PATH` is the path of the page, which `#[admin_page]` declares: give this item another name"
        );
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[loader] async fn list(ctx: AdminContext) -> Result<String> { todo!() }
                const LIST: u32 = 1;
                #[view] fn view() -> impl IntoView { }
            } }),
            "the view of the page calls `list` through the constant `LIST`: give this item another name"
        );
        // Without a view there is no constant to collide with.
        assert!(
            admin_page(
                parse_quote!("/reviews"),
                parse_quote! { impl P {
                    #[loader] async fn list(ctx: AdminContext) -> Result<String> { todo!() }
                    const LIST: u32 = 1;
                } }
            )
            .is_ok()
        );
    }

    #[test]
    fn refuses_functions_a_page_cannot_call() {
        assert_eq!(
            refusal(parse_quote! { impl P { #[loader] fn list(ctx: AdminContext) -> Result<Value> { todo!() } } }),
            "expected `async fn(ctx: AdminContext) -> Result<…>` or `async fn(ctx: AdminContext, input: Input) -> Result<…>` for `#[loader]`"
        );
        assert_eq!(
            refusal(
                parse_quote! { impl P { #[action] async fn save(&self, ctx: AdminContext) -> Result<Value> { todo!() } } }
            ),
            "expected `async fn(ctx: AdminContext) -> Result<…>` or `async fn(ctx: AdminContext, input: Input) -> Result<…>` for `#[action]`"
        );
        assert_eq!(
            refusal(
                parse_quote! { impl P { #[access] async fn staff(ctx: AdminContext, input: Value) -> Result<bool> { todo!() } } }
            ),
            "expected `async fn(ctx: AdminContext) -> Result<bool>`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[access] async fn a(ctx: AdminContext) -> Result<bool> { todo!() }
                #[access] async fn b(ctx: AdminContext) -> Result<bool> { todo!() }
            } }),
            "an admin page has one `#[access]`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[loader] async fn list(ctx: AdminContext) -> Result<Value> { todo!() }
                #[action("list")] async fn save(ctx: AdminContext) -> Result<Value> { todo!() }
            } }),
            "two functions of the page are named `list`"
        );
        assert_eq!(
            refusal(
                parse_quote! { impl P { #[loader] #[action] async fn list(ctx: AdminContext) -> Result<Value> { todo!() } } }
            ),
            "a function of an admin page is one of `#[access]`, `#[loader]`, `#[action]` or `#[view]`"
        );
        assert_eq!(
            refusal(
                parse_quote! { impl P { #[loader(cache = true)] async fn list(ctx: AdminContext) -> Result<Value> { todo!() } } }
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
