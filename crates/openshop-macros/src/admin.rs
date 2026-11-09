//! `#[admin_page]`: the `impl` block of a type as an admin page, its server side and its view.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::spanned::Spanned;
use syn::{Expr, FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, LitStr, ReturnType, Type};

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

/// Whether a `#[view]` returns `Markup`: the server renders it.
fn returns_markup(function: &ImplItemFn) -> bool {
    match &function.sig.output {
        ReturnType::Type(_, returned) => matches!(
            &**returned,
            Type::Path(returned) if returned.path.segments.last().is_some_and(|segment| segment.ident == "Markup")
        ),
        ReturnType::Default => false,
    }
}

/// A parameter of a server-rendered `#[view]`: its name, and whether it is
/// taken by reference (`form: &FormState`).
struct ViewParameter {
    name: Ident,
    by_reference: bool,
}

/// The name of the parameter of a server-rendered `#[view]` that receives the
/// form the page refused, `form: FormState`.
const FORM_PARAMETER: &str = "form";

/// The parameters of a server-rendered `#[view]`, `fn(list: List, …) -> Markup`:
/// each one is a loader of the page, by the name of its function, or `form`.
fn server_view_parameters(function: &ImplItemFn) -> syn::Result<Vec<ViewParameter>> {
    let signature = &function.sig;
    let refusal = |span: Span| {
        syn::Error::new(
            span,
            "expected `fn(loader: Output, …) -> Markup` for a server-rendered `#[view]`: each parameter is named after a loader of the page, or is `form: FormState`",
        )
    };
    if signature.asyncness.is_some() || signature.receiver().is_some() || !signature.generics.params.is_empty() {
        return Err(refusal(signature.ident.span()));
    }
    signature
        .inputs
        .iter()
        .map(|input| match input {
            FnArg::Typed(typed) => match &*typed.pat {
                syn::Pat::Ident(pattern) if pattern.by_ref.is_none() && pattern.subpat.is_none() => Ok(ViewParameter {
                    name: pattern.ident.clone(),
                    by_reference: matches!(&*typed.ty, Type::Reference(_)),
                }),
                other => Err(refusal(other.span())),
            },
            FnArg::Receiver(receiver) => Err(refusal(receiver.span())),
        })
        .collect()
}

/// A loader or an action of the page: what the view reads, or what a form of
/// the view names with a constant, `ReviewsPage::SAVE_REVIEW`.
struct Reference {
    function: Ident,
    kind: Ident,
    name: LitStr,
    input: Option<Type>,
    /// Its own access rule.
    authorize: Option<Expr>,
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
    // The loaders the view reads, and its form.
    let mut server_view: Option<Vec<ViewParameter>> = None;
    let mut functions = Vec::new();
    let mut references: Vec<Reference> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for implemented in &mut item.items {
        let ImplItem::Fn(function) = implemented else {
            continue;
        };
        let Some((marker, arguments)) = take_marker(function)? else {
            // Not marked: a helper of the page.
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
            if !returns_markup(function) {
                return Err(syn::Error::new(
                    function.sig.ident.span(),
                    "a `#[view]` is rendered by the server: `fn(loader: Output, …) -> Markup`, each parameter named after a loader of the page, or `form: FormState`",
                ));
            }
            server_view = Some(server_view_parameters(function)?);
            view = Some(ident);
            continue;
        }

        let input = input_type(function, &marker)?.cloned();

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
        // An `Outcome` it returns is kept for the form that called it.
        let untyped = quote! {
            |ctx: ::openshop::AdminContext, _input: ::openshop::Value| async move {
                ::openshop::admin::output(Self::#ident(ctx).await?)
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
        let own_rule = authorize.cloned();
        let authorize = authorize.map(|authorize| quote!(.authorize(#authorize)));
        functions.push(quote!(.function(#name, #definition #authorize)));
        references.push(Reference {
            kind: Ident::new(&marker, ident.span()),
            function: ident,
            name,
            input,
            authorize: own_rule,
        });
    }

    let access = access.map(|access| quote!(.access(Self::#access)));
    // A view the server renders: the loaders it names, each after the
    // access rule of its own, then the view, a function of what they return
    // and of the form the page refused, when it reads it.
    let server_render = match (&view, &server_view) {
        (Some(view), Some(parameters)) => {
            let mut loads = Vec::new();
            let mut arguments = Vec::new();
            let mut reads_form = false;
            for ViewParameter {
                name: parameter,
                by_reference,
            } in parameters
            {
                let loader = references
                    .iter()
                    .find(|reference| reference.function == *parameter && reference.kind == "loader");
                if parameter == FORM_PARAMETER {
                    if let Some(loader) = loader {
                        return Err(syn::Error::new(
                            loader.function.span(),
                            "`form` is the parameter of a server-rendered `#[view]` that receives the form the page refused: give the loader another name",
                        ));
                    }
                    reads_form = true;
                    arguments.push(if *by_reference { quote!(&form) } else { quote!(form) });
                    continue;
                }
                let Some(loader) = loader else {
                    return Err(syn::Error::new(
                        parameter.span(),
                        format!(
                            "`{parameter}` is not a loader of the page: each parameter of a server-rendered `#[view]` is named after a loader, and receives what it returns, or is `form`, the form the page refused"
                        ),
                    ));
                };
                let function = &loader.function;
                if let Some(authorize) = &loader.authorize {
                    loads.push(quote!(request.authorize(#authorize).await?;));
                }
                let load = if loader.input.is_some() {
                    quote!(load_with)
                } else {
                    quote!(load)
                };
                loads.push(quote!(let #parameter = request.#load(Self::#function).await?;));
                arguments.push(if *by_reference {
                    quote!(&#parameter)
                } else {
                    quote!(#parameter)
                });
            }
            let (register, form) = if reads_form {
                (quote!(view_with_form), quote!(let form = request.form;))
            } else {
                (quote!(view), TokenStream::new())
            };
            quote! {
                .#register(|request: ::openshop::admin::PageRequest| async move {
                    #(#loads)*
                    #form
                    ::openshop::Result::<::openshop::Markup>::Ok(Self::#view(#(#arguments),*))
                })
            }
        }
        _ => TokenStream::new(),
    };
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
    if view.is_some() && server_view.is_some() {
        // The forms of the view name their action with its constant:
        // `input type="hidden" name="_action" value=(Self::SAVE_REVIEW);`.
        for Reference {
            function, kind, name, ..
        } in &references
        {
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
                        "the view of the page names `{function}` with the constant `{constant}`: give this item another name"
                    ),
                ));
            }
            let doc = format!(
                "The name of the {kind} `{}`, what a form of the view sends in `_action`.",
                name.value()
            );
            constants.push(quote! {
                #[doc = #doc]
                #[allow(dead_code)]
                pub const #constant: &'static str = #name;
            });
        }
    }
    // The forms of the view name the functions of the page by their constant.
    let page_view = match &view {
        Some(_) => quote! {
            impl #self_type {
                #(#constants)*
            }
        },
        None => TokenStream::new(),
    };
    let declared = declared(
        &self_type,
        "AdminPage",
        quote!(app.admin_page(::openshop::builder::AdminPage::new(Self::PATH) #nav #access #(#functions)* #server_render)),
    );

    Ok(quote! {
        #item

        impl #self_type {
            /// The path of the page in the admin.
            pub const PATH: &'static str = #path;
        }

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
            ":: openshop :: admin :: output (Self :: list_reviews (ctx) . await ?)",
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

        // An unmarked function stays as it is.
        assert!(generated.contains("fn helper () -> u32 { 1 }"), "{generated}");
        // Without a view, no constant names a function for its forms.
        assert!(!generated.contains("pub const LIST_REVIEWS"), "{generated}");
    }

    #[test]
    fn a_view_that_returns_markup_is_rendered_by_the_server() {
        let generated = admin_page(
            parse_quote!("/"),
            parse_quote! {
                impl HomePage {
                    #[loader]
                    async fn home(ctx: AdminContext) -> Result<Home> { todo!() }

                    #[loader(authorize = Self::staff)]
                    async fn filter(ctx: AdminContext, input: Filter) -> Result<Vec<Row>> { todo!() }

                    #[action(authorize = openshop::billing::any_plan)]
                    async fn set_hidden(ctx: AdminContext, input: SetHidden) -> Result<Outcome> { todo!() }

                    #[view]
                    fn view(home: Home, filter: Vec<Row>) -> Markup { todo!() }
                }
            },
        )
        .unwrap()
        .to_string();

        for expected in [
            // The view is kept, without its marker.
            "{ todo ! () } fn view (home : Home , filter : Vec < Row >) -> Markup",
            // Each parameter is a loader, run with its own rule, its input read from the query.
            ". view (| request : :: openshop :: admin :: PageRequest | async move { let home = request . load (Self :: home) . await ? ; request . authorize (Self :: staff) . await ? ; let filter = request . load_with (Self :: filter) . await ? ; :: openshop :: Result :: < :: openshop :: Markup > :: Ok (Self :: view (home , filter)) })",
            // The forms name an action by its constant.
            "pub const SET_HIDDEN : & 'static str = \"setHidden\" ;",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }
        assert!(!generated.contains("# [view]"), "{generated}");
    }

    #[test]
    fn a_server_view_reads_the_form_the_page_refused() {
        let generated = admin_page(
            parse_quote!("/reviews/:id"),
            parse_quote! {
                impl ReviewPage {
                    #[loader]
                    async fn review(ctx: AdminContext) -> Result<Review> { todo!() }

                    #[action]
                    async fn save(ctx: AdminContext, input: SaveReview) -> Result<Outcome> { todo!() }

                    #[view]
                    fn view(form: FormState, review: &Review) -> Markup { todo!() }
                }
            },
        )
        .unwrap()
        .to_string();
        assert!(
            generated.contains(". view_with_form (| request : :: openshop :: admin :: PageRequest | async move { let review = request . load (Self :: review) . await ? ; let form = request . form ; :: openshop :: Result :: < :: openshop :: Markup > :: Ok (Self :: view (form , & review)) })"),
            "{generated}"
        );

        // A view without `form` keeps the page as it was when a form is refused.
        let without = admin_page(
            parse_quote!("/reviews/:id"),
            parse_quote! { impl ReviewPage {
                #[loader] async fn review(ctx: AdminContext) -> Result<Review> { todo!() }
                #[view] fn view(review: Review) -> Markup { todo!() }
            } },
        )
        .unwrap()
        .to_string();
        assert!(without.contains(". view (| request"), "{without}");
        assert!(!without.contains("request . form"), "{without}");

        assert_eq!(
            refusal(parse_quote! { impl P {
                #[loader] async fn form(ctx: AdminContext) -> Result<String> { todo!() }
                #[view] fn view(form: String) -> Markup { todo!() }
            } }),
            "`form` is the parameter of a server-rendered `#[view]` that receives the form the page refused: give the loader another name"
        );
    }

    #[test]
    fn a_server_view_reads_loaders_only() {
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[loader] async fn list(ctx: AdminContext) -> Result<String> { todo!() }
                #[view] fn view(lists: String) -> Markup { todo!() }
            } }),
            "`lists` is not a loader of the page: each parameter of a server-rendered `#[view]` is named after a loader, and receives what it returns, or is `form`, the form the page refused"
        );
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[action] async fn save(ctx: AdminContext) -> Result<String> { todo!() }
                #[view] fn view(save: String) -> Markup { todo!() }
            } }),
            "`save` is not a loader of the page: each parameter of a server-rendered `#[view]` is named after a loader, and receives what it returns, or is `form`, the form the page refused"
        );
        assert_eq!(
            refusal(parse_quote! { impl P { #[view] async fn view() -> Markup { todo!() } } }),
            "expected `fn(loader: Output, …) -> Markup` for a server-rendered `#[view]`: each parameter is named after a loader of the page, or is `form: FormState`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P { #[view] fn view((a, b): (u8, u8)) -> maud::Markup { todo!() } } }),
            "expected `fn(loader: Output, …) -> Markup` for a server-rendered `#[view]`: each parameter is named after a loader of the page, or is `form: FormState`"
        );
    }

    #[test]
    fn refuses_views_a_page_cannot_show() {
        let not_rendered = "a `#[view]` is rendered by the server: `fn(loader: Output, …) -> Markup`, each parameter named after a loader of the page, or `form: FormState`";
        // A Leptos view, which no bundle shows any more.
        assert_eq!(
            refusal(parse_quote! { impl P { #[view] fn view() -> impl IntoView { } } }),
            not_rendered
        );
        assert_eq!(
            refusal(parse_quote! { impl P { #[view] fn view(params: RouteParams) -> impl IntoView { } } }),
            not_rendered
        );
        assert_eq!(refusal(parse_quote! { impl P { #[view] fn view() { } } }), not_rendered);
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[view] fn list() -> Markup { todo!() }
                #[view] fn grid() -> Markup { todo!() }
            } }),
            "an admin page has one `#[view]`"
        );
        assert_eq!(
            refusal(parse_quote! { impl P { #[view(lazy)] fn view() -> Markup { todo!() } } }),
            "`#[view]` takes no option"
        );
        assert_eq!(
            refusal(parse_quote! { impl P {
                #[loader] async fn path(ctx: AdminContext) -> Result<String> { todo!() }
                #[view] fn view() -> Markup { todo!() }
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
                #[view] fn view() -> Markup { todo!() }
            } }),
            "the view of the page names `list` with the constant `LIST`: give this item another name"
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
