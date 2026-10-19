//! Functions declared as building blocks: `#[flow]`, `#[webhook]`, `#[proxy]`,
//! `#[route]`, `#[mcp_tool]` and `#[mcp_resource]`.
//!
//! The function stays as it is written. Next to it, a type of the same name
//! says what the function is, for the application that lists it.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Expr, FnArg, Ident, ItemFn, LitStr, ReturnType, Type};

use crate::util::{Argument, Arguments, camel_case, doc_text, plain_name, set_once, text, values};

/// Checks that a block is `async fn(context, …) -> Result<…>` and returns the
/// types of the arguments after the context, of which there are at most `extra`.
fn extra_arguments<'a>(item: &'a ItemFn, attribute: &str, shape: &str, extra: usize) -> syn::Result<Vec<&'a Type>> {
    let signature = &item.sig;
    let refusal = |span: Span| syn::Error::new(span, format!("`#[{attribute}]` goes on `{shape}`"));
    if signature.asyncness.is_none() || matches!(signature.output, ReturnType::Default) {
        return Err(refusal(signature.ident.span()));
    }
    if !signature.generics.params.is_empty() {
        return Err(syn::Error::new(
            signature.generics.span(),
            format!("a `#[{attribute}]` function cannot be generic"),
        ));
    }
    let mut types = Vec::new();
    for input in &signature.inputs {
        match input {
            FnArg::Typed(argument) => types.push(&*argument.ty),
            FnArg::Receiver(receiver) => return Err(refusal(receiver.span())),
        }
    }
    if types.is_empty() || types.len() > extra + 1 {
        return Err(refusal(signature.paren_token.span.join()));
    }
    Ok(types.split_off(1))
}

/// The function, and the type of the same name that declares it as a block.
fn declare(item: &ItemFn, kind: &str, doc: String, constants: TokenStream, register: TokenStream) -> TokenStream {
    let krate = quote!(::openshop);
    let name = &item.sig.ident;
    let visibility = &item.vis;
    let kind = format_ident!("{}", kind);
    quote! {
        #item

        #[doc = #doc]
        #[allow(non_camel_case_types)]
        #visibility enum #name {}

        impl #name {
            #constants
        }

        impl #krate::declare::Declared for #name {
            type Kind = #krate::declare::kind::#kind;

            fn register(app: #krate::builder::OpenShopBuilder) -> #krate::builder::OpenShopBuilder {
                #register
            }
        }
    }
}

// ─── #[flow] ─────────────────────────────────────────────────────────

const FLOW_OPTIONS: [&str; 4] = ["timeout", "step_timeout", "concurrency", "retry_policy"];

const DURATION: &str =
    "expected a duration such as \"30s\", \"10m\" or \"1h30m\" (units: ms, s, m, h, d), or an expression";

/// The milliseconds of `"90s"`, `"10m"`, `"1h30m"`: numbers, each followed by its unit.
fn parse_duration(text: &str) -> Option<u64> {
    let mut rest = text.trim();
    let mut total: u64 = 0;
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let (number, after) = rest.split_at(digits);
        let number: u64 = number.parse().ok()?;
        let unit = after.chars().take_while(char::is_ascii_alphabetic).count();
        let (unit, after) = after.split_at(unit);
        let factor: u64 = match unit {
            "ms" => 1,
            "s" => 1_000,
            "m" => 60_000,
            "h" => 3_600_000,
            "d" => 86_400_000,
            _ => return None,
        };
        total = total.checked_add(number.checked_mul(factor)?)?;
        rest = after.trim_start();
    }
    (total > 0).then_some(total)
}

/// `timeout = "10m"`: a duration written as text, checked when the crate
/// compiles. Any other value is an expression of type `Duration`.
fn duration(value: &Expr) -> syn::Result<TokenStream> {
    let Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(text),
        ..
    }) = value
    else {
        return Ok(quote!(#value));
    };
    match parse_duration(&text.value()) {
        Some(millis) => Ok(quote!(::core::time::Duration::from_millis(#millis))),
        None => Err(syn::Error::new(text.span(), DURATION)),
    }
}

/// `concurrency = allow` or `reject`, as words; any other value is an expression.
fn concurrency(value: &Expr) -> TokenStream {
    let word = match value {
        Expr::Path(path) => path.path.get_ident().map(plain_name),
        _ => None,
    };
    match word.as_deref() {
        Some("allow") => quote!(.concurrency(::openshop::Concurrency::Allow)),
        Some("reject") => quote!(.concurrency(::openshop::Concurrency::Reject)),
        _ => quote!(.concurrency(#value)),
    }
}

pub fn flow(arguments: Arguments, item: ItemFn) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    let extra = extra_arguments(
        &item,
        "flow",
        "async fn(ctx: FlowContext) -> Result<()>` or `async fn(ctx: FlowContext, input: Input) -> Result<()>",
        1,
    )?;
    let ident = &item.sig.ident;

    let mut name: Option<LitStr> = None;
    let mut options = Vec::new();
    let mut given: Vec<String> = Vec::new();
    for argument in &arguments.0 {
        // A second value would silently win over the first.
        if let Some(option) = argument.name().filter(|option| FLOW_OPTIONS.contains(&option.as_str())) {
            if given.contains(&option) {
                return Err(syn::Error::new(argument.span(), format!("`{option}` is given twice")));
            }
            given.push(option);
        }
        match argument {
            Argument::Text(text) => set_once(&mut name, argument, text.clone())?,
            Argument::Value(option, value) if option == "concurrency" => {
                options.push(concurrency(value));
            }
            Argument::Value(option, value) if option == "timeout" || option == "step_timeout" => {
                let duration = duration(value)?;
                options.push(quote!(.#option(#duration)));
            }
            Argument::Value(option, value) if FLOW_OPTIONS.contains(&plain_name(option).as_str()) => {
                options.push(quote!(.#option(#value)));
            }
            _ => {
                return Err(syn::Error::new(
                    argument.span(),
                    "unknown flow option; expected the name of the flow, `timeout`, `step_timeout`, `concurrency` or `retry_policy`",
                ));
            }
        }
    }
    let name = name.unwrap_or_else(|| LitStr::new(&camel_case(&plain_name(ident)), ident.span()));
    if name.value().trim().is_empty() {
        return Err(syn::Error::new(name.span(), "a flow needs a name"));
    }

    let flow = match extra.first() {
        Some(input) => quote! {
            #krate::builder::Flow::new(Self::NAME, |ctx: #krate::FlowContext| async move {
                let input = ctx.input::<#input>()?;
                ::core::result::Result::map_err(#ident(ctx, input).await, ::core::convert::Into::into)
            })
            .input::<#input>()
        },
        None => quote! {
            #krate::builder::Flow::new(Self::NAME, |ctx: #krate::FlowContext| async move {
                ::core::result::Result::map_err(#ident(ctx).await, ::core::convert::Into::into)
            })
        },
    };
    // The input is serialized only where a run is dispatched: a flow that is
    // never dispatched from code does not need `Serialize` on its input.
    let dispatch = match extra.first() {
        Some(input) => quote! {
            /// Queues a run of the flow for the shop of `scope`: the context
            /// of a webhook, of a proxy request, of another flow…
            pub async fn dispatch<OpenshopFlowInput>(
                scope: &impl #krate::ShopScope,
                input: OpenshopFlowInput,
            ) -> #krate::Result<#krate::QueuedRun>
            where
                OpenshopFlowInput: #krate::serde::Serialize + #krate::declare::Is<#input>,
            {
                let input = #krate::serde_json::to_value(&input)?;
                #krate::declare::dispatch(scope, Self::NAME, ::core::option::Option::Some(input)).await
            }
        },
        None => quote! {
            /// Queues a run of the flow for the shop of `scope`: the context
            /// of a webhook, of a proxy request, of another flow…
            pub async fn dispatch(scope: &impl #krate::ShopScope) -> #krate::Result<#krate::QueuedRun> {
                #krate::declare::dispatch(scope, Self::NAME, ::core::option::Option::None).await
            }
        },
    };

    Ok(declare(
        &item,
        "Flow",
        format!(" The flow `{}`, for the application that lists it.", name.value()),
        quote! {
            /// The name of the flow, as crons and dispatches refer to it.
            pub const NAME: &'static str = #name;

            #dispatch
        },
        quote!(app.flow(#flow #(#options)*)),
    ))
}

// ─── #[webhook] ──────────────────────────────────────────────────────

pub fn webhook(arguments: Arguments, item: ItemFn) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    let extra = extra_arguments(
        &item,
        "webhook",
        "async fn(ctx: WebhookContext) -> Result<()>` or `async fn(ctx: WebhookContext, payload: Payload) -> Result<()>",
        1,
    )?;
    let ident = &item.sig.ident;

    let mut topics = Vec::new();
    for argument in &arguments.0 {
        match argument {
            Argument::Text(topic) => topics.push(topic),
            _ => {
                return Err(syn::Error::new(
                    argument.span(),
                    "expected a topic: `#[webhook(\"orders/create\")]`",
                ));
            }
        }
    }
    if topics.is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "a webhook names its topics: `#[webhook(\"orders/create\")]`",
        ));
    }

    let run = match extra.first() {
        Some(payload) => quote! {
            |ctx: #krate::WebhookContext| async move {
                let payload: #payload = #krate::serde_json::from_value(ctx.payload.clone())?;
                ::core::result::Result::map_err(#ident(ctx, payload).await, ::core::convert::Into::into)
            }
        },
        None => quote! {
            |ctx: #krate::WebhookContext| async move {
                ::core::result::Result::map_err(#ident(ctx).await, ::core::convert::Into::into)
            }
        },
    };
    let listed = topics
        .iter()
        .map(|topic| topic.value())
        .collect::<Vec<_>>()
        .join("`, `");

    Ok(declare(
        &item,
        "Webhook",
        format!(" The webhook of `{listed}`, for the application that lists it."),
        quote! {
            /// The topics the webhook handles.
            pub const TOPICS: &'static [&'static str] = &[#(#topics),*];
        },
        quote!(app #(.webhook(#topics, #krate::builder::Webhook::new(#run)))*),
    ))
}

// ─── #[proxy] and #[route] ───────────────────────────────────────────

const PROXY_METHODS: [&str; 5] = ["GET", "POST", "PUT", "DELETE", "PATCH"];
const ROUTE_METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// What opens `#[proxy]` and `#[route]`: `GET, "/path"`.
struct Endpoint<'a> {
    /// `GET`.
    method: String,
    /// The method of the definition that takes the handler: `get`.
    setter: Ident,
    path: &'a LitStr,
    options: &'a [Argument],
}

fn endpoint<'a>(arguments: &'a [Argument], attribute: &str, methods: &[&str]) -> syn::Result<Endpoint<'a>> {
    let expected = format!("expected a method and a path: `#[{attribute}(GET, \"/path\")]`");
    let [Argument::Word(word), Argument::Text(path), options @ ..] = arguments else {
        let span = arguments.first().map_or_else(Span::call_site, Argument::span);
        return Err(syn::Error::new(span, expected));
    };
    let method = plain_name(word).to_uppercase();
    if !methods.contains(&method.as_str()) {
        return Err(syn::Error::new(
            word.span(),
            format!("unknown method; expected {}", methods.join(", ")),
        ));
    }
    if !path.value().starts_with('/') {
        return Err(syn::Error::new(path.span(), "a path starts with `/`"));
    }
    Ok(Endpoint {
        setter: Ident::new(&method.to_lowercase(), word.span()),
        method,
        path,
        options,
    })
}

impl Endpoint<'_> {
    /// Whether a typed input is read from the query string rather than from the body.
    fn reads_query(&self) -> bool {
        !matches!(self.method.as_str(), "POST" | "PUT" | "PATCH")
    }
}

fn endpoint_constants(endpoint: &Endpoint) -> TokenStream {
    let (method, path) = (&endpoint.method, endpoint.path);
    quote! {
        /// The HTTP method the function handles.
        pub const METHOD: &'static str = #method;
        /// The path the function is served at.
        pub const PATH: &'static str = #path;
    }
}

pub fn proxy(arguments: Arguments, item: ItemFn) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    let extra = extra_arguments(
        &item,
        "proxy",
        "async fn(ctx: ProxyContext) -> Result<…>` or `async fn(ctx: ProxyContext, input: Input) -> Result<…>",
        1,
    )?;
    let ident = &item.sig.ident;
    let endpoint = endpoint(&arguments.0, "proxy", &PROXY_METHODS)?;

    let mut response: Option<Ident> = None;
    for option in endpoint.options {
        let variant = match option.name().as_deref() {
            Some("json") => "Json",
            Some("liquid") => "Liquid",
            Some("html") => "Html",
            _ => "",
        };
        if variant.is_empty() || !matches!(option, Argument::Word(_)) {
            return Err(syn::Error::new(
                option.span(),
                "unknown proxy option; expected the response type: `json`, `liquid` or `html`",
            ));
        }
        set_once(&mut response, option, Ident::new(variant, option.span()))?;
    }
    let response = response.unwrap_or_else(|| format_ident!("Json"));
    let setter = &endpoint.setter;
    let call = match extra.first() {
        Some(input) => {
            let from_query = endpoint.reads_query();
            quote! {
                let input: #input = #krate::declare::proxy_input(&ctx, #from_query)?;
                #ident(ctx, input).await?
            }
        }
        None => quote!(#ident(ctx).await?),
    };

    Ok(declare(
        &item,
        "Proxy",
        format!(
            " The app proxy handler of `{} {}`, for the application that lists it.",
            endpoint.method,
            endpoint.path.value()
        ),
        endpoint_constants(&endpoint),
        quote! {
            app.proxy(
                Self::PATH,
                #krate::builder::Proxy::new(#krate::builder::ProxyResponseType::#response).#setter(
                    |ctx: #krate::ProxyContext| async move {
                        #krate::Result::<#krate::Value>::Ok(#krate::serde_json::to_value({ #call })?)
                    },
                ),
            )
        },
    ))
}

pub fn route(arguments: Arguments, item: ItemFn) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    let extra = extra_arguments(
        &item,
        "route",
        "async fn(ctx: RouteContext) -> Result<…>` or `async fn(ctx: RouteContext, input: Input) -> Result<…>",
        1,
    )?;
    let ident = &item.sig.ident;
    let endpoint = endpoint(&arguments.0, "route", &ROUTE_METHODS)?;

    let mut auth: Option<&Expr> = None;
    let mut json = false;
    for option in endpoint.options {
        match option {
            Argument::Value(name, value) if name == "auth" => set_once(&mut auth, option, value)?,
            Argument::Word(word) if word == "json" && !json => json = true,
            _ => {
                return Err(syn::Error::new(
                    option.span(),
                    "unknown route option; expected `json` or `auth = <function>`",
                ));
            }
        }
    }
    let input = extra.first().map(|input| {
        let from_query = endpoint.reads_query();
        quote! {
            let input: #input = match #krate::declare::route_input(&ctx, #from_query) {
                ::core::result::Result::Ok(input) => input,
                ::core::result::Result::Err(refusal) => return #krate::Result::Ok(*refusal),
            };
        }
    });
    let call = match extra.first() {
        Some(_) => quote!(#ident(ctx, input).await?),
        None => quote!(#ident(ctx).await?),
    };
    // With `json`, the function returns anything that serializes, like a proxy.
    let answer = if json {
        quote!(#krate::response::json(#call))
    } else {
        quote!(#krate::response::IntoResponse::into_response(#call))
    };
    let definition = match auth {
        Some(auth) => quote!(#krate::builder::Route::authenticated(#auth)),
        None => quote!(#krate::builder::Route::public()),
    };
    let setter = &endpoint.setter;

    Ok(declare(
        &item,
        "Route",
        format!(
            " The server route `{} {}`, for the application that lists it.",
            endpoint.method,
            endpoint.path.value()
        ),
        endpoint_constants(&endpoint),
        quote! {
            app.route(
                Self::PATH,
                // The authentication value has the type the handler takes: `auth` must return it.
                #definition.#setter(|ctx: #krate::RouteContext<_>| async move {
                    #input
                    #krate::Result::<#krate::response::Response>::Ok(#answer)
                }),
            )
        },
    ))
}

// ─── #[mcp_tool] and #[mcp_resource] ─────────────────────────────────

/// `risk = low`.
fn risk_level(value: &Expr) -> syn::Result<TokenStream> {
    let level = match value {
        Expr::Path(path) => path.path.get_ident().map(plain_name),
        _ => None,
    };
    let variant = match level.as_deref() {
        Some("low") => "Low",
        Some("medium") => "Medium",
        Some("high") => "High",
        _ => return Err(syn::Error::new(value.span(), "expected `low`, `medium` or `high`")),
    };
    let variant = Ident::new(variant, value.span());
    Ok(quote!(.risk(::openshop::McpRiskLevel::#variant)))
}

/// The options MCP tools and resources share. `None` when the option is not one of them.
fn capability_option(argument: &Argument, description: &mut Option<LitStr>) -> syn::Result<Option<TokenStream>> {
    match argument {
        Argument::Value(name, value) if name == "description" => {
            set_once(description, argument, text(value)?)?;
            Ok(Some(TokenStream::new()))
        }
        Argument::Value(name, value) if name == "risk" => risk_level(value).map(Some),
        Argument::List(name, permissions) if name == "permissions" => {
            let permissions = values(permissions.clone())?;
            // A key or a `McpPermission`, in any mix.
            Ok(Some(quote! {
                .permissions([#(::core::convert::Into::<::openshop::builder::RequiredPermission>::into(#permissions)),*])
            }))
        }
        _ => Ok(None),
    }
}

/// The text of `description = "…"`, else of the documentation comment.
fn description(stated: Option<LitStr>, item: &ItemFn) -> Option<LitStr> {
    stated.or_else(|| {
        let text = doc_text(&item.attrs);
        (!text.is_empty()).then(|| LitStr::new(&text, item.sig.ident.span()))
    })
}

pub fn mcp_tool(arguments: Arguments, item: ItemFn) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    let extra = extra_arguments(
        &item,
        "mcp_tool",
        "async fn(ctx: McpContext) -> Result<…>` or `async fn(ctx: McpContext, input: Input) -> Result<…>",
        1,
    )?;
    let ident = &item.sig.ident;

    let mut name: Option<LitStr> = None;
    let mut stated: Option<LitStr> = None;
    let mut options = Vec::new();
    for argument in &arguments.0 {
        if let Argument::Text(text) = argument {
            set_once(&mut name, argument, text.clone())?;
        } else if let Some(option) = capability_option(argument, &mut stated)? {
            options.push(option);
        } else if let Argument::Value(option, value) = argument
            && option == "confirmation_hint"
        {
            let hint = text(value)?;
            options.push(quote!(.confirmation_hint(#hint)));
        } else {
            return Err(syn::Error::new(
                argument.span(),
                "unknown tool option; expected the name of the tool, `description`, `permissions(…)`, `risk` or `confirmation_hint`",
            ));
        }
    }
    let Some(name) = name else {
        return Err(syn::Error::new(
            Span::call_site(),
            "an MCP tool needs a name: `#[mcp_tool(\"reviews.list\")]`",
        ));
    };
    let Some(description) = description(stated, &item) else {
        return Err(syn::Error::new(
            ident.span(),
            "an MCP tool needs a description: a documentation comment, or `description = \"…\"`",
        ));
    };

    let tool = match extra.first() {
        Some(input) => quote! {
            #krate::builder::McpTool::new(
                #description,
                |ctx: #krate::McpContext, input: #krate::JsonMap| async move {
                    let input: #input = #krate::serde_json::from_value(#krate::Value::Object(input))?;
                    #krate::Result::<#krate::Value>::Ok(#krate::serde_json::to_value(#ident(ctx, input).await?)?)
                },
            )
            .input_schema(#krate::builder::InputSchema::of::<#input>().json_schema)
        },
        None => quote! {
            #krate::builder::McpTool::new(
                #description,
                |ctx: #krate::McpContext, _input: #krate::JsonMap| async move {
                    #krate::Result::<#krate::Value>::Ok(#krate::serde_json::to_value(#ident(ctx).await?)?)
                },
            )
            .input_schema(#krate::json!({ "type": "object", "additionalProperties": false, "properties": {} }))
        },
    };

    Ok(declare(
        &item,
        "McpTool",
        format!(" The MCP tool `{}`, for the application that lists it.", name.value()),
        quote! {
            /// The name of the tool.
            pub const NAME: &'static str = #name;
        },
        quote!(app.mcp_tool(Self::NAME, #tool #(#options)*)),
    ))
}

pub fn mcp_resource(arguments: Arguments, item: ItemFn) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    extra_arguments(&item, "mcp_resource", "async fn(ctx: McpContext) -> Result<…>", 0)?;
    let ident = &item.sig.ident;

    let mut uri: Option<LitStr> = None;
    let mut name: Option<LitStr> = None;
    let mut stated: Option<LitStr> = None;
    let mut options = Vec::new();
    for argument in &arguments.0 {
        if let Argument::Text(text) = argument {
            set_once(&mut uri, argument, text.clone())?;
        } else if let Some(option) = capability_option(argument, &mut stated)? {
            options.push(option);
        } else if let Argument::Value(option, value) = argument
            && option == "name"
        {
            set_once(&mut name, argument, text(value)?)?;
        } else if let Argument::Value(option, value) = argument
            && option == "mime_type"
        {
            let mime_type = text(value)?;
            options.push(quote!(.mime_type(#mime_type)));
        } else {
            return Err(syn::Error::new(
                argument.span(),
                "unknown resource option; expected the URI of the resource, `name`, `description`, `mime_type`, `permissions(…)` or `risk`",
            ));
        }
    }
    let (Some(uri), Some(name)) = (uri, name) else {
        return Err(syn::Error::new(
            Span::call_site(),
            "an MCP resource needs a URI and a name: `#[mcp_resource(\"app://guide\", name = \"Guide\")]`",
        ));
    };
    let description = description(stated, &item).map(|description| quote!(.description(#description)));

    Ok(declare(
        &item,
        "McpResource",
        format!(
            " The MCP resource `{}`, for the application that lists it.",
            uri.value()
        ),
        quote! {
            /// The URI of the resource.
            pub const URI: &'static str = #uri;
        },
        quote! {
            app.mcp_resource(
                Self::URI,
                #krate::builder::McpResource::new(#name, |ctx: #krate::McpContext| async move {
                    #krate::Result::<#krate::McpResourceContent>::Ok(::core::convert::Into::into(#ident(ctx).await?))
                })
                #description
                #(#options)*,
            )
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    type Block = fn(Arguments, ItemFn) -> syn::Result<TokenStream>;

    fn generated(block: Block, arguments: TokenStream, item: ItemFn) -> String {
        block(syn::parse2(arguments).unwrap(), item).unwrap().to_string()
    }

    fn refusal(block: Block, arguments: TokenStream, item: ItemFn) -> String {
        match block(syn::parse2(arguments).unwrap(), item) {
            Ok(_) => panic!("accepted"),
            Err(error) => error.to_string(),
        }
    }

    fn assert_contains(generated: &str, expected: &[&str]) {
        for expected in expected {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }
    }

    #[test]
    fn a_flow_keeps_the_function_and_declares_a_type_of_its_name() {
        let flow_with_input = generated(
            flow,
            quote!(
                "syncOrders",
                timeout = Duration::from_secs(60),
                concurrency = Concurrency::Allow
            ),
            parse_quote! {
                /// Sends orders to the warehouse.
                pub async fn sync_orders(ctx: FlowContext, input: SyncOrdersInput) -> Result<()> { Ok(()) }
            },
        );
        assert_contains(
            &flow_with_input,
            &[
                "pub async fn sync_orders (ctx : FlowContext , input : SyncOrdersInput) -> Result < () > { Ok (()) }",
                "# [allow (non_camel_case_types)] pub enum sync_orders { }",
                "pub const NAME : & 'static str = \"syncOrders\" ;",
                "impl :: openshop :: declare :: Declared for sync_orders { type Kind = :: openshop :: declare :: kind :: Flow ;",
                "let input = ctx . input :: < SyncOrdersInput > () ? ;",
                "sync_orders (ctx , input) . await",
                ". input :: < SyncOrdersInput > () . timeout (Duration :: from_secs (60)) . concurrency (Concurrency :: Allow)",
                "pub async fn dispatch < OpenshopFlowInput > (scope : & impl :: openshop :: ShopScope , input : OpenshopFlowInput ,)",
                "OpenshopFlowInput : :: openshop :: serde :: Serialize + :: openshop :: declare :: Is < SyncOrdersInput >",
            ],
        );

        // Without a name the flow is named after the function; without an input it has no schema.
        let plain = generated(
            flow,
            quote!(),
            parse_quote! { async fn nightly_cleanup(ctx: FlowContext) -> Result<()> { Ok(()) } },
        );
        assert_contains(
            &plain,
            &[
                "enum nightly_cleanup { }",
                "= \"nightlyCleanup\" ;",
                "nightly_cleanup (ctx) . await",
                "pub async fn dispatch (scope : & impl :: openshop :: ShopScope)",
            ],
        );
        // A duration is text checked here, or any expression.
        let timed = generated(
            flow,
            quote!(timeout = "1h30m", step_timeout = "500ms"),
            parse_quote! { async fn nightly_cleanup(ctx: FlowContext) -> Result<()> { Ok(()) } },
        );
        assert_contains(
            &timed,
            &[
                ". timeout (:: core :: time :: Duration :: from_millis (5400000u64))",
                ". step_timeout (:: core :: time :: Duration :: from_millis (500u64))",
            ],
        );
        for (text, millis) in [
            ("90s", 90_000),
            ("10m", 600_000),
            ("2h", 7_200_000),
            ("1d", 86_400_000),
            ("1h 30m", 5_400_000),
        ] {
            assert_eq!(parse_duration(text), Some(millis), "{text}");
        }
        for text in ["", "10", "m", "10 minutes", "1.5h", "-5s", "0s", "5x"] {
            assert_eq!(parse_duration(text), None, "{text}");
            let refused = refusal(
                flow,
                quote!(timeout = #text),
                parse_quote! { async fn nightly_cleanup(ctx: FlowContext) -> Result<()> { Ok(()) } },
            );
            assert_eq!(refused, DURATION);
        }
        // A concurrency is a word, or any expression.
        let allowed = generated(
            flow,
            quote!(concurrency = allow),
            parse_quote! { async fn nightly_cleanup(ctx: FlowContext) -> Result<()> { Ok(()) } },
        );
        assert_contains(&allowed, &[". concurrency (:: openshop :: Concurrency :: Allow)"]);
        assert!(!plain.contains(". input ::"), "{plain}");
    }

    #[test]
    fn refuses_flows_that_are_not_async_functions_of_a_context() {
        let shape = "`#[flow]` goes on `async fn(ctx: FlowContext) -> Result<()>` or `async fn(ctx: FlowContext, input: Input) -> Result<()>`";
        for item in [
            parse_quote! { fn sync(ctx: FlowContext) -> Result<()> { Ok(()) } },
            parse_quote! { async fn sync(ctx: FlowContext) { } },
            parse_quote! { async fn sync() -> Result<()> { Ok(()) } },
            parse_quote! { async fn sync(ctx: FlowContext, input: Input, more: u32) -> Result<()> { Ok(()) } },
        ] {
            assert_eq!(refusal(flow, quote!(), item), shape);
        }
        let item: ItemFn = parse_quote! { async fn sync(ctx: FlowContext) -> Result<()> { Ok(()) } };
        assert_eq!(
            refusal(
                flow,
                quote!(),
                parse_quote! { async fn sync<T>(ctx: FlowContext, input: T) -> Result<()> { Ok(()) } }
            ),
            "a `#[flow]` function cannot be generic"
        );
        assert_eq!(
            refusal(flow, quote!("sync", retries = 3), item.clone()),
            "unknown flow option; expected the name of the flow, `timeout`, `step_timeout`, `concurrency` or `retry_policy`"
        );
        assert_eq!(refusal(flow, quote!("a", "b"), item.clone()), "given twice");
        for (arguments, option) in [
            (quote!(timeout = "1m", timeout = "2m"), "timeout"),
            (quote!(step_timeout = "1m", "sync", step_timeout = "2m"), "step_timeout"),
            (quote!(concurrency = allow, concurrency = reject), "concurrency"),
            (quote!(retry_policy = a, retry_policy = b), "retry_policy"),
        ] {
            assert_eq!(
                refusal(flow, arguments, item.clone()),
                format!("`{option}` is given twice")
            );
        }
        assert_eq!(refusal(flow, quote!(" "), item), "a flow needs a name");
    }

    #[test]
    fn a_webhook_names_its_topics() {
        let item: ItemFn =
            parse_quote! { async fn changed(ctx: WebhookContext, order: Order) -> Result<()> { Ok(()) } };
        assert_contains(
            &generated(webhook, quote!("orders/create", "orders/updated"), item.clone()),
            &[
                "pub const TOPICS : & 'static [& 'static str] = & [\"orders/create\" , \"orders/updated\"] ;",
                "type Kind = :: openshop :: declare :: kind :: Webhook ;",
                "let payload : Order = :: openshop :: serde_json :: from_value (ctx . payload . clone ()) ? ;",
                "app . webhook (\"orders/create\" , :: openshop :: builder :: Webhook :: new (",
                ". webhook (\"orders/updated\" , :: openshop :: builder :: Webhook :: new (",
            ],
        );
        assert_eq!(
            refusal(webhook, quote!(), item.clone()),
            "a webhook names its topics: `#[webhook(\"orders/create\")]`"
        );
        assert_eq!(
            refusal(webhook, quote!(orders), item),
            "expected a topic: `#[webhook(\"orders/create\")]`"
        );
    }

    #[test]
    fn a_proxy_and_a_route_state_their_method_and_their_path() {
        let handler: ItemFn = parse_quote! { pub async fn reviews(ctx: ProxyContext) -> Result<String> { todo!() } };
        assert_contains(
            &generated(proxy, quote!(GET, "/reviews", liquid), handler.clone()),
            &[
                "pub const METHOD : & 'static str = \"GET\" ;",
                "pub const PATH : & 'static str = \"/reviews\" ;",
                "type Kind = :: openshop :: declare :: kind :: Proxy ;",
                "app . proxy (Self :: PATH , :: openshop :: builder :: Proxy :: new (:: openshop :: builder :: ProxyResponseType :: Liquid) . get (",
                ":: openshop :: serde_json :: to_value ({ reviews (ctx) . await ? }) ?",
            ],
        );
        // JSON unless stated, and the method in any case.
        assert_contains(
            &generated(proxy, quote!(post, "/reviews"), handler.clone()),
            &["\"POST\"", "ProxyResponseType :: Json) . post ("],
        );
        assert_eq!(
            refusal(proxy, quote!("/reviews"), handler.clone()),
            "expected a method and a path: `#[proxy(GET, \"/path\")]`"
        );
        assert_eq!(
            refusal(proxy, quote!(HEAD, "/reviews"), handler.clone()),
            "unknown method; expected GET, POST, PUT, DELETE, PATCH"
        );
        assert_eq!(
            refusal(proxy, quote!(GET, "reviews"), handler.clone()),
            "a path starts with `/`"
        );
        assert_eq!(
            refusal(proxy, quote!(GET, "/reviews", xml), handler.clone()),
            "unknown proxy option; expected the response type: `json`, `liquid` or `html`"
        );
        // A second argument is the typed input: the query of a GET, the body of a POST.
        let typed: ItemFn =
            parse_quote! { async fn reviews(ctx: ProxyContext, input: Search) -> Result<String> { todo!() } };
        assert_contains(
            &generated(proxy, quote!(GET, "/reviews"), typed.clone()),
            &[
                "let input : Search = :: openshop :: declare :: proxy_input (& ctx , true) ? ;",
                "reviews (ctx , input) . await ?",
            ],
        );
        assert_contains(
            &generated(proxy, quote!(POST, "/reviews"), typed),
            &[":: openshop :: declare :: proxy_input (& ctx , false) ? ;"],
        );
        assert_eq!(
            refusal(
                proxy,
                quote!(GET, "/reviews"),
                parse_quote! { async fn reviews(ctx: ProxyContext, a: A, b: B) -> Result<String> { todo!() } }
            ),
            "`#[proxy]` goes on `async fn(ctx: ProxyContext) -> Result<…>` or `async fn(ctx: ProxyContext, input: Input) -> Result<…>`"
        );

        let handler: ItemFn = parse_quote! { async fn ping(ctx: RouteContext) -> Result<Response> { todo!() } };
        assert_contains(
            &generated(route, quote!(GET, "/ping"), handler.clone()),
            &[
                "type Kind = :: openshop :: declare :: kind :: Route ;",
                "app . route (Self :: PATH , :: openshop :: builder :: Route :: public () . get (",
                ":: openshop :: response :: IntoResponse :: into_response (ping (ctx) . await ?)",
            ],
        );
        assert_contains(
            &generated(route, quote!(OPTIONS, "/ping", auth = partner::verify), handler.clone()),
            &[":: openshop :: builder :: Route :: authenticated (partner :: verify) . options ("],
        );
        // `json` serializes the result; a second argument is the typed input.
        assert_contains(
            &generated(
                route,
                quote!(POST, "/orders", json),
                parse_quote! { async fn create(ctx: RouteContext, order: NewOrder) -> Result<Value> { todo!() } },
            ),
            &[
                "let input : NewOrder = match :: openshop :: declare :: route_input (& ctx , false)",
                ":: openshop :: response :: json (create (ctx , input) . await ?)",
            ],
        );
        assert_eq!(
            refusal(route, quote!(GET, "/ping", public), handler.clone()),
            "unknown route option; expected `json` or `auth = <function>`"
        );
        assert_eq!(
            refusal(route, quote!(TRACE, "/ping"), handler),
            "unknown method; expected GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS"
        );
    }

    #[test]
    fn mcp_capabilities_read_their_description_from_the_documentation() {
        let tool: ItemFn = parse_quote! {
            /// Counts reviews.
            ///
            /// Reads only.
            pub async fn count(ctx: McpContext, input: CountInput) -> Result<Value> { todo!() }
        };
        assert_contains(
            &generated(
                mcp_tool,
                quote!(
                    "reviews.count",
                    permissions("reviews:read", OTHER),
                    risk = high,
                    confirmation_hint = "Sure?"
                ),
                tool.clone(),
            ),
            &[
                "pub const NAME : & 'static str = \"reviews.count\" ;",
                "type Kind = :: openshop :: declare :: kind :: McpTool ;",
                ":: openshop :: builder :: McpTool :: new (\"Counts reviews.\\n\\nReads only.\" ,",
                "let input : CountInput = :: openshop :: serde_json :: from_value (:: openshop :: Value :: Object (input)) ? ;",
                ". input_schema (:: openshop :: builder :: InputSchema :: of :: < CountInput > () . json_schema)",
                ". permissions ([:: core :: convert :: Into :: < :: openshop :: builder :: RequiredPermission > :: into (\"reviews:read\") , :: core :: convert :: Into :: < :: openshop :: builder :: RequiredPermission > :: into (OTHER)])",
                ". risk (:: openshop :: McpRiskLevel :: High)",
                ". confirmation_hint (\"Sure?\")",
            ],
        );
        let undocumented: ItemFn = parse_quote! { async fn count(ctx: McpContext) -> Result<Value> { todo!() } };
        assert_contains(
            &generated(
                mcp_tool,
                quote!("reviews.count", description = "Stated."),
                undocumented.clone(),
            ),
            &[
                ":: openshop :: builder :: McpTool :: new (\"Stated.\" ,",
                "\"additionalProperties\" : false",
            ],
        );
        assert_eq!(
            refusal(mcp_tool, quote!("reviews.count"), undocumented.clone()),
            "an MCP tool needs a description: a documentation comment, or `description = \"…\"`"
        );
        assert_eq!(
            refusal(mcp_tool, quote!(risk = low), tool.clone()),
            "an MCP tool needs a name: `#[mcp_tool(\"reviews.list\")]`"
        );
        assert_eq!(
            refusal(mcp_tool, quote!("reviews.count", risk = extreme), tool.clone()),
            "expected `low`, `medium` or `high`"
        );
        assert_eq!(
            refusal(mcp_tool, quote!("reviews.count", mime_type = "text/plain"), tool),
            "unknown tool option; expected the name of the tool, `description`, `permissions(…)`, `risk` or `confirmation_hint`"
        );

        let resource: ItemFn = parse_quote! {
            /// The guide.
            async fn guide(ctx: McpContext) -> Result<String> { todo!() }
        };
        assert_contains(
            &generated(
                mcp_resource,
                quote!("app://guide", name = "Guide", mime_type = "text/markdown"),
                resource.clone(),
            ),
            &[
                "pub const URI : & 'static str = \"app://guide\" ;",
                "type Kind = :: openshop :: declare :: kind :: McpResource ;",
                ":: openshop :: builder :: McpResource :: new (\"Guide\" ,",
                ":: core :: convert :: Into :: into (guide (ctx) . await ?)",
                ". description (\"The guide.\") . mime_type (\"text/markdown\")",
            ],
        );
        assert_eq!(
            refusal(mcp_resource, quote!("app://guide"), resource.clone()),
            "an MCP resource needs a URI and a name: `#[mcp_resource(\"app://guide\", name = \"Guide\")]`"
        );
        assert_eq!(
            refusal(
                mcp_resource,
                quote!("app://guide", name = "Guide", confirmation_hint = "x"),
                resource
            ),
            "unknown resource option; expected the URI of the resource, `name`, `description`, `mime_type`, `permissions(…)` or `risk`"
        );
    }
}
