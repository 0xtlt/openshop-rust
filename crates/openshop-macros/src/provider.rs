//! `#[derive(ProviderConfig)]` and `#[provider]`: a struct as a provider.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Expr, Fields, FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, LitStr, Pat,
    ReceiverKind, ReturnType, Type, Visibility,
};

use crate::util::{
    camel_case, constant_ident, declared, humanize, lower_initial, own_names, plain_name, set, set_value,
};

// ─── #[derive(ProviderConfig)] ───────────────────────────────────────

#[derive(Default)]
struct FieldOptions {
    label: Option<LitStr>,
    name: Option<LitStr>,
    placeholder: Option<LitStr>,
    description: Option<LitStr>,
    password: bool,
    /// Label and value of each choice.
    select: Option<Vec<(LitStr, LitStr)>>,
    /// The constraints stated by name: `url`, `non_empty`, `range(…)`, `length(…)`.
    constraints: Vec<TokenStream>,
    validate: Option<Expr>,
}

/// `range(min = 1, max = 1000)` or `length(min = 1)`: the bounds given, as `Option`s.
fn parse_bounds(meta: &syn::meta::ParseNestedMeta) -> syn::Result<(TokenStream, TokenStream)> {
    const MISSING: &str = "expected a bound: `min = …`, `max = …` or both";
    if !meta.input.peek(syn::token::Paren) {
        return Err(meta.error(MISSING));
    }
    let ahead = meta.input.fork();
    let bounds;
    syn::parenthesized!(bounds in ahead);
    if bounds.is_empty() {
        return Err(meta.error(MISSING));
    }
    let (mut min, mut max): (Option<Expr>, Option<Expr>) = (None, None);
    meta.parse_nested_meta(|bound| {
        if bound.path.is_ident("min") {
            set_value(&mut min, &bound)
        } else if bound.path.is_ident("max") {
            set_value(&mut max, &bound)
        } else {
            Err(bound.error("expected `min = …` or `max = …`"))
        }
    })?;
    let bound = |value: Option<Expr>| match value {
        Some(value) => quote!(::core::option::Option::Some((#value) as _)),
        None => quote!(::core::option::Option::None),
    };
    Ok((bound(min), bound(max)))
}

/// `select("Europe" = "eu", "United States" = "us")`.
fn parse_choices(meta: &syn::meta::ParseNestedMeta) -> syn::Result<Vec<(LitStr, LitStr)>> {
    let content;
    syn::parenthesized!(content in meta.input);
    let mut choices = Vec::new();
    while !content.is_empty() {
        let label: LitStr = content
            .parse()
            .map_err(|error| syn::Error::new(error.span(), "expected a choice: `\"Label\" = \"value\"`"))?;
        content.parse::<syn::Token![=]>()?;
        choices.push((label, content.parse()?));
        if content.parse::<Option<syn::Token![,]>>()?.is_none() {
            break;
        }
    }
    if !content.is_empty() {
        return Err(content.error("expected `,` between the choices"));
    }
    if choices.is_empty() {
        return Err(meta.error("a select field lists its choices: `select(\"Europe\" = \"eu\")`"));
    }
    Ok(choices)
}

fn field_options(field: &syn::Field) -> syn::Result<FieldOptions> {
    let mut options = FieldOptions::default();
    for attribute in field
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("field"))
    {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("label") {
                set_value(&mut options.label, &meta)
            } else if meta.path.is_ident("name") {
                set_value(&mut options.name, &meta)
            } else if meta.path.is_ident("placeholder") {
                set_value(&mut options.placeholder, &meta)
            } else if meta.path.is_ident("description") {
                set_value(&mut options.description, &meta)
            } else if meta.path.is_ident("password") {
                options.password = true;
                Ok(())
            } else if meta.path.is_ident("select") {
                set(&mut options.select, &meta, parse_choices)
            } else if meta.path.is_ident("url") {
                options.constraints.push(quote!(::openshop::validators::url()));
                Ok(())
            } else if meta.path.is_ident("non_empty") {
                options.constraints.push(quote!(::openshop::validators::non_empty()));
                Ok(())
            } else if meta.path.is_ident("range") {
                let (min, max) = parse_bounds(&meta)?;
                options.constraints.push(quote!(::openshop::validators::range(#min, #max)));
                Ok(())
            } else if meta.path.is_ident("length") {
                let (min, max) = parse_bounds(&meta)?;
                options.constraints.push(quote!(::openshop::validators::length(#min, #max)));
                Ok(())
            } else if meta.path.is_ident("validate") {
                set_value(&mut options.validate, &meta)
            } else {
                Err(meta.error(
                    "unknown field option; expected `label`, `name`, `placeholder`, `description`, `password`, `select(…)`, `url`, `non_empty`, `range(…)`, `length(…)` or `validate`",
                ))
            }
        })?;
    }
    Ok(options)
}

/// The form a struct describes: one field of the form per field of the struct.
pub struct Form {
    /// Statements adding each field to `fields`, a `Fields`.
    pub definitions: Vec<TokenStream>,
    /// The struct, read from `config`, a mutable `JsonMap`.
    pub construct: TokenStream,
    /// `("api_url", "apiUrl")`: the name of each field and its key.
    pub keys: Vec<TokenStream>,
    /// Statements inserting each field of `self` in `config`, a mutable `JsonMap`.
    pub writers: Vec<TokenStream>,
}

/// Reads the form of a struct. `derive` and `what` name the derive and what
/// the struct is, for the refusals.
pub fn form(input: &DeriveInput, derive: &str, what: &str) -> syn::Result<Form> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            format!("a {what} cannot be generic"),
        ));
    }
    let not_a_struct = format!("`{derive}` is derived on a struct with named fields, or without fields");
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(name.span(), not_a_struct));
    };
    let named: Vec<&syn::Field> = match &data.fields {
        Fields::Named(named) => named.named.iter().collect(),
        Fields::Unit => Vec::new(),
        Fields::Unnamed(_) => return Err(syn::Error::new(name.span(), not_a_struct)),
    };

    let mut keys: Vec<String> = Vec::new();
    let mut definitions = Vec::new();
    let mut readers = Vec::new();
    let mut named_keys = Vec::new();
    let mut writers = Vec::new();
    for field in named {
        let ident = field.ident.as_ref().expect("a named field");
        let ty = &field.ty;
        let options = field_options(field)?;
        // Without a label, the field is called by its name: `batch_size` is "Batch size".
        let label = &options
            .label
            .clone()
            .unwrap_or_else(|| LitStr::new(&humanize(&plain_name(ident)), ident.span()));
        if options.password && options.select.is_some() {
            return Err(syn::Error::new(
                ident.span(),
                "a field is `password` or `select(…)`, not both",
            ));
        }
        let key = match &options.name {
            Some(key) => key.value(),
            None => camel_case(&plain_name(ident)),
        };
        if keys.contains(&key) {
            return Err(syn::Error::new(
                ident.span(),
                format!("two fields are saved as `{key}`"),
            ));
        }

        // A password or a choice is a string in the form; the type says whether it can be left blank.
        let stated = if options.password {
            Some(quote!(::openshop::builder::Field::password(#label)))
        } else {
            options.select.as_ref().map(|choices| {
                let choices = choices
                    .iter()
                    .map(|(label, value)| quote!(::openshop::builder::FieldOption::new(#label, #value)));
                quote!(::openshop::builder::Field::select(#label, [#(#choices),*]))
            })
        };
        let base = match stated {
            Some(field) => quote! {{
                let field = #field;
                if <#ty as ::openshop::builder::ProviderField>::OPTIONAL { field.optional() } else { field }
            }},
            None => quote!(<#ty as ::openshop::builder::ProviderField>::field(#label)),
        };
        let placeholder = options.placeholder.as_ref().map(|text| quote!(.placeholder(#text)));
        let description = options.description.as_ref().map(|text| quote!(.description(#text)));
        // The named constraints first, in the order they are written.
        let constraints = options
            .constraints
            .iter()
            .map(|constraint| quote!(.validate(#constraint)));
        let validate = options.validate.as_ref().map(|validator| quote!(.validate(#validator)));
        definitions.push(quote! {
            fields.insert(::std::string::String::from(#key), #base #placeholder #description #(#constraints)* #validate);
        });
        readers.push(quote!(#ident: ::openshop::builder::provider_field_value::<#ty>(&mut config, #key)?));
        let field_name = plain_name(ident);
        named_keys.push(quote!((#field_name, #key)));
        writers.push(quote! {
            config.insert(
                ::std::string::String::from(#key),
                ::openshop::serde_json::to_value(&self.#ident).unwrap_or(::openshop::Value::Null),
            );
        });
        keys.push(key);
    }

    let construct = match &data.fields {
        Fields::Unit => quote!({
            let _ = &mut config;
            Self
        }),
        _ => quote!(Self { #(#readers,)* }),
    };
    Ok(Form {
        definitions,
        construct,
        keys: named_keys,
        writers,
    })
}

pub fn derive_config(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let Form {
        definitions,
        construct,
        keys,
        writers,
    } = form(&input, "ProviderConfig", "provider configuration")?;

    Ok(quote! {
        impl ::openshop::ProviderConfig for #name {
            const FIELD_KEYS: &'static [(&'static str, &'static str)] = &[#(#keys),*];

            fn fields() -> ::openshop::builder::Fields {
                let mut fields = ::openshop::builder::Fields::new();
                #(#definitions)*
                fields
            }

            fn from_config(mut config: ::openshop::JsonMap) -> ::core::result::Result<Self, ::std::string::String> {
                ::core::result::Result::Ok(#construct)
            }

            #[allow(unused_mut)]
            fn to_config(&self) -> ::openshop::JsonMap {
                let mut config = ::openshop::JsonMap::new();
                #(#writers)*
                config
            }
        }

        // The saved configuration is the only thing the struct is read from.
        impl<'de> ::openshop::serde::Deserialize<'de> for #name {
            fn deserialize<D>(deserializer: D) -> ::core::result::Result<Self, D::Error>
            where
                D: ::openshop::serde::Deserializer<'de>,
            {
                let config = <::openshop::JsonMap as ::openshop::serde::Deserialize>::deserialize(deserializer)?;
                <Self as ::openshop::ProviderConfig>::from_config(config).map_err(::openshop::serde::de::Error::custom)
            }
        }
    })
}

// ─── #[provider("name")] ─────────────────────────────────────────────

/// A method of the `impl` block that callers reach through the connectors.
struct Method<'a> {
    function: &'a ImplItemFn,
    /// Names and types of the arguments after `&self`.
    arguments: Vec<(Ident, &'a Type)>,
    output: &'a Type,
}

fn has_attribute(attributes: &[Attribute], name: &str) -> bool {
    attributes.iter().any(|attribute| attribute.path().is_ident(name))
}

fn takes_shared_self(function: &ImplItemFn) -> bool {
    matches!(
        function.sig.inputs.first(),
        Some(FnArg::Receiver(receiver)) if matches!(receiver.kind, ReceiverKind::Reference(_, _, None))
    )
}

fn output(function: &ImplItemFn) -> syn::Result<&Type> {
    match &function.sig.output {
        ReturnType::Type(_, ty) => Ok(ty),
        ReturnType::Default => Err(syn::Error::new(
            function.sig.ident.span(),
            "a provider method returns a `Result`: `-> openshop::Result<…>`",
        )),
    }
}

fn method(function: &ImplItemFn) -> syn::Result<Method<'_>> {
    let signature = &function.sig;
    if !signature.generics.params.is_empty() {
        return Err(syn::Error::new(
            signature.generics.span(),
            "a provider method cannot be generic: its arguments travel as JSON",
        ));
    }
    let mut arguments = Vec::new();
    for (index, input) in signature.inputs.iter().skip(1).enumerate() {
        let FnArg::Typed(argument) = input else {
            continue;
        };
        if let Type::Reference(reference) = &*argument.ty {
            return Err(syn::Error::new(
                reference.span(),
                "a provider method takes its arguments by value: they are read from the call",
            ));
        }
        let name = match &*argument.pat {
            Pat::Ident(pattern) => pattern.ident.clone(),
            _ => format_ident!("argument_{}", index),
        };
        arguments.push((name, &*argument.ty));
    }
    Ok(Method {
        function,
        arguments,
        output: output(function)?,
    })
}

fn docs(function: &ImplItemFn) -> Vec<&Attribute> {
    function
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("doc"))
        .collect()
}

/// What the block declares next to the items of the user: the name of the
/// provider, its definition, and its connectors.
const DECLARED: [&str; 4] = ["NAME", "provider", "on", "config_of"];

/// Refuses an item of the block, or the constant of a method, that would be
/// declared twice by the expansion. Rustc would point at the attribute.
fn check_names(item: &ItemImpl, methods: &[Method]) -> syn::Result<()> {
    let own = own_names(item);
    for ident in &own {
        let name = plain_name(ident);
        if DECLARED.contains(&name.as_str()) {
            return Err(syn::Error::new(
                ident.span(),
                format!(
                    "`{name}` is declared by `#[provider]` (the name, the definition, the connectors and the configuration of the provider): give this item another name"
                ),
            ));
        }
    }
    let mut constants: Vec<String> = Vec::new();
    for method in methods {
        let ident = &method.function.sig.ident;
        let constant = constant_ident(ident).to_string();
        if DECLARED.contains(&constant.as_str()) {
            return Err(syn::Error::new(
                ident.span(),
                format!(
                    "the call of this method would be the constant `{constant}`, which `#[provider]` declares: give the method another name"
                ),
            ));
        }
        let taken = constants.contains(&constant) || own.iter().any(|own| own.to_string() == constant);
        if taken {
            return Err(syn::Error::new(
                ident.span(),
                format!(
                    "the call of this method would be the constant `{constant}`, which is already in this block: give the method another name"
                ),
            ));
        }
        constants.push(constant);
    }
    Ok(())
}

pub fn provider(name: Option<LitStr>, mut item: ItemImpl) -> syn::Result<TokenStream> {
    if let Some((path, _)) = &item.trait_ {
        return Err(syn::Error::new(
            path.span(),
            "`#[provider]` goes on the `impl` block of the configuration struct itself",
        ));
    }
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new(item.generics.span(), "a provider cannot be generic"));
    }
    let self_type = (*item.self_ty).clone();
    let Type::Path(path) = &self_type else {
        return Err(syn::Error::new(
            self_type.span(),
            "`#[provider]` goes on the `impl` block of a struct",
        ));
    };
    let struct_name = &path.path.segments.last().expect("a path has a segment").ident;
    // Without a name, the provider is named after the struct: `Warehouse` is "warehouse".
    let name = name.unwrap_or_else(|| LitStr::new(&lower_initial(&plain_name(struct_name)), struct_name.span()));
    if name.value().trim().is_empty() {
        return Err(syn::Error::new(
            name.span(),
            "a provider needs a name: `#[provider(\"warehouse\")]`",
        ));
    }
    let connector = format_ident!("{}Connector", struct_name);

    // The marker of the connection check is ours: it does not survive the expansion.
    let mut checker: Option<Ident> = None;
    for implemented in &mut item.items {
        let ImplItem::Fn(function) = implemented else {
            continue;
        };
        if !has_attribute(&function.attrs, "checker") {
            continue;
        }
        function.attrs.retain(|attribute| !attribute.path().is_ident("checker"));
        let signature = &function.sig;
        if checker.is_some() {
            return Err(syn::Error::new(
                signature.ident.span(),
                "a provider has one `#[checker]`",
            ));
        }
        if signature.asyncness.is_none() || !takes_shared_self(function) || signature.inputs.len() != 1 {
            return Err(syn::Error::new(
                signature.ident.span(),
                "the connection check is `async fn(&self) -> Result<bool>`",
            ));
        }
        output(function)?;
        checker = Some(signature.ident.clone());
    }

    let mut methods = Vec::new();
    for implemented in &item.items {
        let ImplItem::Fn(function) = implemented else {
            continue;
        };
        let signature = &function.sig;
        let exposed = !matches!(function.vis, Visibility::Inherited);
        if !exposed || checker.as_ref() == Some(&signature.ident) {
            continue;
        }
        // An associated function, such as a constructor, is not a method of the provider.
        if signature.receiver().is_none() {
            continue;
        }
        if signature.asyncness.is_none() || !takes_shared_self(function) {
            return Err(syn::Error::new(
                signature.ident.span(),
                "a provider method is `async fn(&self, …) -> Result<…>`; a private method stays out of the provider",
            ));
        }
        methods.push(method(function)?);
    }

    check_names(&item, &methods)?;

    let mut constants = Vec::new();
    let mut handlers = Vec::new();
    let mut calls = Vec::new();
    for method in &methods {
        let function = method.function;
        let ident = &function.sig.ident;
        let method_name = plain_name(ident);
        let constant = constant_ident(ident);
        let visibility = &function.vis;
        let docs = docs(function);
        let output = method.output;
        let ok = quote!(<#output as ::openshop::builder::MethodResult>::Ok);
        let names: Vec<&Ident> = method.arguments.iter().map(|(name, _)| name).collect();
        let types: Vec<&Type> = method.arguments.iter().map(|(_, ty)| *ty).collect();

        // One argument travels as itself, several as a list, none as `null`.
        let (arguments_type, pattern, sent) = match types.as_slice() {
            [] => (quote!(()), quote!(()), quote!(())),
            [ty] => {
                let name = names[0];
                (
                    quote!(#ty),
                    quote!(#name),
                    quote!(<_ as ::core::borrow::Borrow<#ty>>::borrow(&#name)),
                )
            }
            _ => (
                quote!((#(#types,)*)),
                quote!((#(#names,)*)),
                quote!((#(<_ as ::core::borrow::Borrow<#types>>::borrow(&#names),)*)),
            ),
        };

        let constant_doc =
            format!(" The call of [`{struct_name}::{ident}`]: it names the fake of the method in a test.");
        constants.push(quote! {
            #[doc = #constant_doc]
            #visibility const #constant: ::openshop::builder::ProviderCall<#arguments_type, #ok> =
                ::openshop::builder::ProviderCall::new(#name, #method_name);
        });
        handlers.push(quote! {
            .handle(Self::#constant, |config: Self, #pattern: #arguments_type| async move {
                ::openshop::builder::MethodResult::into_result(config.#ident(#(#names),*).await)
            })
        });
        calls.push(quote! {
            #(#docs)*
            #visibility async fn #ident(
                &self,
                #(#names: impl ::core::borrow::Borrow<#types>),*
            ) -> ::openshop::Result<#ok> {
                self.connectors.call_as(#name, #method_name, #sent).await
            }
        });
    }

    let checker = checker.map(|check| {
        quote! {
            .typed_checker(|config: Self| async move {
                ::openshop::builder::MethodResult::into_result(config.#check().await)
            })
        }
    });
    let provider_doc = format!(
        " The provider `{}`: the fields of `{struct_name}` and the methods of this block.",
        name.value()
    );
    let connector_doc = format!(
        " The methods of the provider `{}`, called with the configuration a shop saved.",
        name.value()
    );

    let declared = declared(&self_type, "Provider", quote!(app.provider(Self::provider())));

    Ok(quote! {
        #item

        #[allow(private_interfaces)]
        impl #self_type {
            /// The name of the provider.
            pub const NAME: &'static str = #name;

            #(#constants)*

            #[doc = #provider_doc]
            pub fn provider() -> ::openshop::builder::Provider {
                ::openshop::builder::Provider::new(#name)
                    .fields(<Self as ::openshop::ProviderConfig>::fields())
                    #checker
                    #(#handlers)*
            }

            /// The methods of the provider for a shop: on `ctx.connectors` in a
            /// flow, where they run with the configuration that shop saved.
            pub fn on(connectors: &::openshop::Connectors) -> #connector<'_> {
                #connector { connectors }
            }

            /// The configuration the shop of a context saved for the provider:
            /// read, saved, checked and removed by the code of the
            /// application, with the validation and the encryption of the
            /// built-in providers page.
            pub fn config_of(
                scope: &(impl ::openshop::ShopScope + ?::core::marker::Sized),
            ) -> ::openshop::providers::ProviderStore<Self> {
                ::openshop::providers::ProviderStore::new(scope, #name)
            }
        }

        #declared

        #[doc = #connector_doc]
        #[allow(private_interfaces)]
        pub struct #connector<'a> {
            connectors: &'a ::openshop::Connectors,
        }

        #[allow(private_interfaces)]
        impl #connector<'_> {
            #(#calls)*
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn saves_fields_under_camel_case_keys() {
        for (field, key) in [
            ("api_url", "apiUrl"),
            ("key", "key"),
            ("batch_size_max", "batchSizeMax"),
            ("_private", "private"),
        ] {
            assert_eq!(camel_case(field), key);
        }
    }

    fn config_refusal(input: DeriveInput) -> String {
        match derive_config(input) {
            Ok(_) => panic!("accepted"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn a_struct_describes_the_form() {
        let generated = derive_config(parse_quote! {
            struct Warehouse {
                #[field(label = "API URL", placeholder = "https://…", validate = validators::url())]
                api_url: String,
                #[field(label = "API key", password, description = "From the dashboard")]
                api_key: String,
                #[field(label = "Region", select("Europe" = "eu", "United States" = "us"))]
                region: Option<String>,
                #[field(label = "Batch size", name = "batch")]
                batch_size: u32,
            }
        })
        .unwrap()
        .to_string();

        for expected in [
            "impl :: openshop :: ProviderConfig for Warehouse",
            "String :: from (\"apiUrl\") , < String as :: openshop :: builder :: ProviderField > :: field (\"API URL\") . placeholder (\"https://…\") . validate (validators :: url ())",
            "let field = :: openshop :: builder :: Field :: password (\"API key\") ; if < String as :: openshop :: builder :: ProviderField > :: OPTIONAL { field . optional () } else { field } } . description (\"From the dashboard\")",
            ":: openshop :: builder :: Field :: select (\"Region\" , [:: openshop :: builder :: FieldOption :: new (\"Europe\" , \"eu\") , :: openshop :: builder :: FieldOption :: new (\"United States\" , \"us\")])",
            "String :: from (\"batch\") , < u32 as :: openshop :: builder :: ProviderField > :: field (\"Batch size\")",
            "batch_size : :: openshop :: builder :: provider_field_value :: < u32 > (& mut config , \"batch\") ?",
            "impl < 'de > :: openshop :: serde :: Deserialize < 'de > for Warehouse",
            "const FIELD_KEYS : & 'static [(& 'static str , & 'static str)] = & [(\"api_url\" , \"apiUrl\") , (\"api_key\" , \"apiKey\") , (\"region\" , \"region\") , (\"batch_size\" , \"batch\")] ;",
            "config . insert (:: std :: string :: String :: from (\"batch\") , :: openshop :: serde_json :: to_value (& self . batch_size)",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }

        // A provider without configuration.
        let empty = derive_config(parse_quote! { struct Clock; }).unwrap().to_string();
        assert!(
            empty.contains("Result :: Ok ({ let _ = & mut config ; Self })"),
            "{empty}"
        );
    }

    #[test]
    fn a_field_is_labeled_after_its_name_and_states_its_constraints() {
        let generated = derive_config(parse_quote! {
            struct Warehouse {
                #[field(url, non_empty)]
                api_url: String,
                #[field(range(min = 1, max = 1000), validate = even)]
                batch_size: u32,
                #[field(label = "Code", length(max = 8))]
                code: Option<String>,
                plain: bool,
            }
        })
        .unwrap()
        .to_string();

        for expected in [
            "field (\"Api url\") . validate (:: openshop :: validators :: url ()) . validate (:: openshop :: validators :: non_empty ())",
            // The named constraints run before the validator of the field.
            "field (\"Batch size\") . validate (:: openshop :: validators :: range (:: core :: option :: Option :: Some ((1) as _) , :: core :: option :: Option :: Some ((1000) as _))) . validate (even)",
            "field (\"Code\") . validate (:: openshop :: validators :: length (:: core :: option :: Option :: None , :: core :: option :: Option :: Some ((8) as _)))",
            "< bool as :: openshop :: builder :: ProviderField > :: field (\"Plain\")",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }
        assert_eq!(humanize("batch_size_max"), "Batch size max");
    }

    #[test]
    fn refuses_fields_the_form_cannot_show() {
        assert_eq!(
            config_refusal(parse_quote! { struct W { #[field(range())] a: u32 } }),
            "expected a bound: `min = …`, `max = …` or both"
        );
        assert_eq!(
            config_refusal(parse_quote! { struct W { #[field(length(exactly = 3))] a: String } }),
            "expected `min = …` or `max = …`"
        );
        assert_eq!(
            config_refusal(parse_quote! { struct W { #[field(label = "A", password, select("a" = "a"))] a: String } }),
            "a field is `password` or `select(…)`, not both"
        );
        assert_eq!(
            config_refusal(
                parse_quote! { struct W { #[field(label = "A")] api_url: String, #[field(label = "B", name = "apiUrl")] other: String } }
            ),
            "two fields are saved as `apiUrl`"
        );
        assert_eq!(
            config_refusal(parse_quote! { struct W { #[field(label = "A", secret)] a: String } }),
            "unknown field option; expected `label`, `name`, `placeholder`, `description`, `password`, `select(…)`, `url`, `non_empty`, `range(…)`, `length(…)` or `validate`"
        );
        assert_eq!(
            config_refusal(parse_quote! { struct W { #[field(label = "A", select())] a: String } }),
            "a select field lists its choices: `select(\"Europe\" = \"eu\")`"
        );
        assert_eq!(
            config_refusal(parse_quote! { struct W(String); }),
            "`ProviderConfig` is derived on a struct with named fields, or without fields"
        );
        assert_eq!(
            config_refusal(parse_quote! { struct W<T> { #[field(label = "A")] a: T } }),
            "a provider configuration cannot be generic"
        );
    }

    fn provider_refusal(item: ItemImpl) -> String {
        match provider(parse_quote!("warehouse"), item) {
            Ok(_) => panic!("accepted"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn public_async_methods_become_the_provider() {
        let generated = provider(
            parse_quote!("warehouse"),
            parse_quote! {
                impl Warehouse {
                    #[checker]
                    async fn check(&self) -> Result<bool> { Ok(true) }

                    /// Sends orders.
                    pub async fn push(&self, orders: Vec<Order>) -> Result<usize> { Ok(orders.len()) }

                    pub async fn move_stock(&self, sku: String, quantity: i64) -> Result<()> { Ok(()) }

                    pub async fn ping(&self) -> Result<String> { Ok(self.endpoint()) }

                    pub fn new() -> Self { todo!() }

                    async fn request(&self) -> Result<Response> { todo!() }

                    fn endpoint(&self) -> String { todo!() }
                }
            },
        )
        .unwrap()
        .to_string();

        for expected in [
            // The block itself is kept, without the marker.
            "async fn check (& self) -> Result < bool >",
            "pub const PUSH : :: openshop :: builder :: ProviderCall < Vec < Order > , < Result < usize > as :: openshop :: builder :: MethodResult > :: Ok > = :: openshop :: builder :: ProviderCall :: new (\"warehouse\" , \"push\")",
            "pub const MOVE_STOCK : :: openshop :: builder :: ProviderCall < (String , i64 ,) ,",
            "pub const PING : :: openshop :: builder :: ProviderCall < () ,",
            ":: openshop :: builder :: Provider :: new (\"warehouse\") . fields (< Self as :: openshop :: ProviderConfig > :: fields ())",
            ". typed_checker (| config : Self | async move { :: openshop :: builder :: MethodResult :: into_result (config . check () . await) })",
            ". handle (Self :: PUSH , | config : Self , orders : Vec < Order > | async move { :: openshop :: builder :: MethodResult :: into_result (config . push (orders) . await) })",
            ". handle (Self :: MOVE_STOCK , | config : Self , (sku , quantity ,) : (String , i64 ,) | async move",
            ". handle (Self :: PING , | config : Self , () : () | async move",
            "pub fn on (connectors : & :: openshop :: Connectors) -> WarehouseConnector < '_ >",
            "pub fn config_of (scope : & (impl :: openshop :: ShopScope + ? :: core :: marker :: Sized) ,) -> :: openshop :: providers :: ProviderStore < Self > { :: openshop :: providers :: ProviderStore :: new (scope , \"warehouse\") }",
            "pub struct WarehouseConnector < 'a >",
            "# [doc = r\" Sends orders.\"] pub async fn push (& self , orders : impl :: core :: borrow :: Borrow < Vec < Order > >)",
            "call_as (\"warehouse\" , \"move_stock\" , (< _ as :: core :: borrow :: Borrow < String >> :: borrow (& sku) , < _ as :: core :: borrow :: Borrow < i64 >> :: borrow (& quantity) ,))",
            "call_as (\"warehouse\" , \"ping\" , ())",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }
        assert!(!generated.contains("# [checker]"), "{generated}");
        // Private methods and associated functions are not provider methods.
        assert!(!generated.contains("const REQUEST"), "{generated}");
        assert!(!generated.contains("const ENDPOINT"), "{generated}");
        assert!(!generated.contains("const NEW"), "{generated}");
        assert!(!generated.contains("const CHECK"), "{generated}");
    }

    #[test]
    fn refuses_what_the_expansion_would_declare_twice() {
        let declared = |name: &str| {
            format!(
                "`{name}` is declared by `#[provider]` (the name, the definition, the connectors and the configuration of the provider): give this item another name"
            )
        };
        assert_eq!(
            provider_refusal(parse_quote! { impl W { pub fn config_of() -> Self { W } } }),
            declared("config_of")
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl W { pub fn on() -> Self { W } } }),
            declared("on")
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl W { pub fn provider() -> Self { W } } }),
            declared("provider")
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl W { const NAME: &str = "w"; } }),
            declared("NAME")
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl W { pub async fn name(&self) -> Result<String> { todo!() } } }),
            "the call of this method would be the constant `NAME`, which `#[provider]` declares: give the method another name"
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl W {
                const PING: u32 = 1;
                pub async fn ping(&self) -> Result<String> { todo!() }
            } }),
            "the call of this method would be the constant `PING`, which is already in this block: give the method another name"
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl W {
                pub async fn ping(&self) -> Result<String> { todo!() }
                pub async fn Ping(&self) -> Result<String> { todo!() }
            } }),
            "the call of this method would be the constant `PING`, which is already in this block: give the method another name"
        );
        // A private constant of another name, and private helpers, are fine.
        assert!(
            provider(
                None,
                parse_quote! { impl W {
                    const LIMIT: u32 = 1;
                    fn new() -> Self { W }
                    pub async fn ping(&self) -> Result<String> { todo!() }
                } }
            )
            .is_ok()
        );
    }

    #[test]
    fn refuses_methods_that_cannot_be_called_through_connectors() {
        assert_eq!(
            provider_refusal(parse_quote! { impl W { pub fn push(&self) -> Result<()> { Ok(()) } } }),
            "a provider method is `async fn(&self, …) -> Result<…>`; a private method stays out of the provider"
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl W { pub async fn push(&mut self) -> Result<()> { Ok(()) } } }),
            "a provider method is `async fn(&self, …) -> Result<…>`; a private method stays out of the provider"
        );
        assert_eq!(
            provider_refusal(
                parse_quote! { impl W { pub async fn push(&self, orders: &[Order]) -> Result<()> { Ok(()) } } }
            ),
            "a provider method takes its arguments by value: they are read from the call"
        );
        assert_eq!(
            provider_refusal(
                parse_quote! { impl W { pub async fn push<T>(&self, orders: T) -> Result<()> { Ok(()) } } }
            ),
            "a provider method cannot be generic: its arguments travel as JSON"
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl W { pub async fn push(&self) { } } }),
            "a provider method returns a `Result`: `-> openshop::Result<…>`"
        );
        assert_eq!(
            provider_refusal(
                parse_quote! { impl W { #[checker] async fn check(&self, deep: bool) -> Result<bool> { Ok(true) } } }
            ),
            "the connection check is `async fn(&self) -> Result<bool>`"
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl W {
                #[checker] async fn a(&self) -> Result<bool> { Ok(true) }
                #[checker] async fn b(&self) -> Result<bool> { Ok(true) }
            } }),
            "a provider has one `#[checker]`"
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl Display for W { } }),
            "`#[provider]` goes on the `impl` block of the configuration struct itself"
        );
        assert_eq!(
            provider_refusal(parse_quote! { impl<T> W<T> { } }),
            "a provider cannot be generic"
        );
    }
}
