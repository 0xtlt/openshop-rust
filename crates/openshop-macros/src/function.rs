//! `#[derive(FunctionConfig)]`: a struct as the configuration of a Shopify Function.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{DeriveInput, Expr, Ident, LitStr};

use crate::provider::{Form, form};
use crate::util::{Argument, Arguments, lower_initial, plain_name, set_once, text};

/// The option naming the type of a function, and the variant of `FunctionType`.
const TYPES: [(&str, &str); 6] = [
    ("discount", "Discount"),
    ("cart_transform", "CartTransform"),
    ("delivery_customization", "DeliveryCustomization"),
    ("payment_customization", "PaymentCustomization"),
    ("checkout_validation", "CheckoutValidation"),
    ("fulfillment_constraints", "FulfillmentConstraints"),
];

/// The flags of the Shopify owner of an instance.
const OWNER_FLAGS: [&str; 5] = ["starts_at", "ends_at", "usage_limit", "applies_on_each_item", "enabled"];

const COMBINATIONS: [&str; 3] = ["product_discounts", "order_discounts", "shipping_discounts"];

const EXPECTED: &str = "unknown function option; expected the name of the function, its type and handle (`discount = \"handle\"`), `label`, `modes(…)`, `title`, `starts_at`, `ends_at`, `usage_limit`, `applies_on_each_item`, `enabled` or `combines_with(…)`";

/// `modes(automatic, code)`.
fn modes(list: TokenStream) -> syn::Result<TokenStream> {
    let mut modes = Vec::new();
    for mode in syn::parse2::<Arguments>(list)?.0 {
        let variant = match (&mode, mode.name().as_deref()) {
            (Argument::Word(_), Some("automatic")) => "Automatic",
            (Argument::Word(_), Some("code")) => "Code",
            _ => return Err(syn::Error::new(mode.span(), "expected `automatic` or `code`")),
        };
        let variant = Ident::new(variant, mode.span());
        modes.push(quote!(::openshop::DiscountMode::#variant));
    }
    Ok(quote!(.modes([#(#modes),*])))
}

/// `combines_with(product_discounts, order_discounts = false)`.
fn combines_with(list: TokenStream) -> syn::Result<TokenStream> {
    let mut stated: [Option<TokenStream>; 3] = [None, None, None];
    for combination in syn::parse2::<Arguments>(list)?.0 {
        let name = combination.name().unwrap_or_default();
        let Some(index) = COMBINATIONS.iter().position(|known| *known == name) else {
            return Err(syn::Error::new(
                combination.span(),
                "expected `product_discounts`, `order_discounts` or `shipping_discounts`",
            ));
        };
        let value = combination.flag()?;
        set_once(
            &mut stated[index],
            &combination,
            quote!(::core::option::Option::Some(#value)),
        )?;
    }
    let fields = COMBINATIONS.iter().zip(stated).map(|(name, value)| {
        let field = Ident::new(name, Span::call_site());
        let value = value.unwrap_or_else(|| quote!(::core::option::Option::None));
        quote!(#field: #value)
    });
    Ok(quote!(.combines_with(::openshop::CombinesWith { #(#fields),* })))
}

pub fn derive(input: DeriveInput) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    let name = &input.ident;
    let Form { definitions, construct } = form(&input, "FunctionConfig", "function configuration")?;

    let mut key: Option<LitStr> = None;
    let mut function: Option<(Ident, LitStr)> = None;
    let mut title: Option<Expr> = None;
    let mut label: Option<LitStr> = None;
    let mut definition_options = Vec::new();
    let mut owner_options = Vec::new();
    let mut stated = false;
    for attribute in input
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("function"))
    {
        stated = true;
        for argument in &attribute.parse_args::<Arguments>()?.0 {
            let option = argument.name().unwrap_or_default();
            match argument {
                Argument::Text(text) => set_once(&mut key, argument, text.clone())?,
                Argument::Value(word, value) if TYPES.iter().any(|(name, _)| *name == option) => {
                    let variant = TYPES.iter().find(|(name, _)| *name == option).expect("a known type").1;
                    set_once(
                        &mut function,
                        argument,
                        (Ident::new(variant, word.span()), text(value)?),
                    )?;
                }
                Argument::Value(_, value) if option == "label" => set_once(&mut label, argument, text(value)?)?,
                Argument::Value(_, value) if option == "title" => set_once(&mut title, argument, value.clone())?,
                Argument::List(_, list) if option == "modes" => definition_options.push(modes(list.clone())?),
                Argument::List(_, list) if option == "combines_with" => {
                    owner_options.push(combines_with(list.clone())?);
                }
                _ if OWNER_FLAGS.contains(&option.as_str()) => {
                    let flag = Ident::new(&option, argument.span());
                    let value = argument.flag()?;
                    owner_options.push(quote!(.#flag(#value)));
                }
                _ => return Err(syn::Error::new(argument.span(), EXPECTED)),
            }
        }
    }
    let Some((function_type, handle)) = function else {
        let span = if stated { name.span() } else { Span::call_site() };
        return Err(syn::Error::new(
            span,
            "a function states its type and its handle: `#[function(discount = \"volume-discount\")]`",
        ));
    };
    let key = key.unwrap_or_else(|| LitStr::new(&lower_initial(&plain_name(name)), name.span()));

    // What an instance is called when its configuration cannot be read.
    let fallback = label.clone().unwrap_or_else(|| handle.clone());
    let owner = match &title {
        Some(Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(title),
            ..
        })) => Some(quote!(#krate::FunctionOwner::new(#title))),
        Some(title) => Some(quote! {
            #krate::FunctionOwner::dynamic_title(|config: &#krate::JsonMap| {
                match <Self as #krate::FunctionConfig>::from_config(::core::clone::Clone::clone(config)) {
                    ::core::result::Result::Ok(config) => (#title)(&config),
                    ::core::result::Result::Err(_) => ::std::string::String::from(#fallback),
                }
            })
        }),
        None => None,
    };
    let owner = match (owner, owner_options.is_empty()) {
        (Some(owner), _) => Some(quote!(.owner(#owner #(#owner_options)*))),
        (None, true) => None,
        (None, false) => {
            return Err(syn::Error::new(
                name.span(),
                "the options of the Shopify owner need its `title`: `title = \"…\"` or `title = Self::title`",
            ));
        }
    };
    let label = label.map(|label| quote!(.label(#label)));

    Ok(quote! {
        impl #krate::FunctionConfig for #name {
            const NAME: &'static str = #key;

            fn definition() -> #krate::Function {
                let mut fields = #krate::Fields::new();
                #(#definitions)*
                let mut function = #krate::Function::new(#krate::FunctionType::#function_type, #handle)
                    #label
                    #(#definition_options)*
                    #owner;
                function.config = fields;
                function
            }

            fn from_config(mut config: #krate::JsonMap) -> ::core::result::Result<Self, ::std::string::String> {
                ::core::result::Result::Ok(#construct)
            }
        }

        impl #krate::Declared for #name {
            type Kind = #krate::declare::kind::Function;

            fn register(app: #krate::OpenShopBuilder) -> #krate::OpenShopBuilder {
                app.function(
                    <Self as #krate::FunctionConfig>::NAME,
                    <Self as #krate::FunctionConfig>::definition(),
                )
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn refusal(input: DeriveInput) -> String {
        match derive(input) {
            Ok(_) => panic!("accepted"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn a_struct_describes_the_function_and_its_form() {
        let generated = derive(parse_quote! {
            #[function(
                "volumeDiscount",
                discount = "volume-discount",
                label = "Volume discount",
                modes(automatic, code),
                title = Self::title,
                starts_at,
                ends_at = false,
                combines_with(product_discounts, order_discounts = false)
            )]
            struct VolumeDiscount {
                #[field(label = "Minimum quantity")]
                min_quantity: u32,
            }
        })
        .unwrap()
        .to_string();

        for expected in [
            "impl :: openshop :: FunctionConfig for VolumeDiscount { const NAME : & 'static str = \"volumeDiscount\" ;",
            "String :: from (\"minQuantity\") , < u32 as :: openshop :: ProviderField > :: field (\"Minimum quantity\")",
            ":: openshop :: Function :: new (:: openshop :: FunctionType :: Discount , \"volume-discount\") . label (\"Volume discount\")",
            ". modes ([:: openshop :: DiscountMode :: Automatic , :: openshop :: DiscountMode :: Code])",
            ". owner (:: openshop :: FunctionOwner :: dynamic_title (",
            ":: core :: result :: Result :: Ok (config) => (Self :: title) (& config)",
            ":: core :: result :: Result :: Err (_) => :: std :: string :: String :: from (\"Volume discount\")",
            ". starts_at (true) . ends_at (false)",
            ". combines_with (:: openshop :: CombinesWith { product_discounts : :: core :: option :: Option :: Some (true) , order_discounts : :: core :: option :: Option :: Some (false) , shipping_discounts : :: core :: option :: Option :: None })",
            "function . config = fields ;",
            "impl :: openshop :: Declared for VolumeDiscount { type Kind = :: openshop :: declare :: kind :: Function ;",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }

        // The key defaults to the name of the struct, and the owner to none.
        let plain = derive(parse_quote! {
            #[function(cart_transform = "cart-rules")]
            struct CartRules;
        })
        .unwrap()
        .to_string();
        assert!(
            plain.contains("const NAME : & 'static str = \"cartRules\" ;"),
            "{plain}"
        );
        assert!(
            plain.contains("FunctionType :: CartTransform , \"cart-rules\") ;"),
            "{plain}"
        );
    }

    #[test]
    fn refuses_functions_without_a_type_or_with_unknown_options() {
        let stating = "a function states its type and its handle: `#[function(discount = \"volume-discount\")]`";
        assert_eq!(refusal(parse_quote! { struct F; }), stating);
        assert_eq!(refusal(parse_quote! { #[function("f")] struct F; }), stating);
        assert_eq!(
            refusal(parse_quote! { #[function(discount = "a", cart_transform = "b")] struct F; }),
            "given twice"
        );
        assert_eq!(
            refusal(parse_quote! { #[function(discount = "a", starts_at)] struct F; }),
            "the options of the Shopify owner need its `title`: `title = \"…\"` or `title = Self::title`"
        );
        assert_eq!(
            refusal(parse_quote! { #[function(discount = "a", modes(always))] struct F; }),
            "expected `automatic` or `code`"
        );
        assert_eq!(
            refusal(parse_quote! { #[function(discount = "a", title = "T", combines_with(everything))] struct F; }),
            "expected `product_discounts`, `order_discounts` or `shipping_discounts`"
        );
        assert_eq!(
            refusal(parse_quote! { #[function(discount = "a", weight = 3)] struct F; }),
            EXPECTED
        );
        assert_eq!(
            refusal(parse_quote! { #[function(discount = "a")] struct F<T> { #[field(label = "A")] a: T } }),
            "a function configuration cannot be generic"
        );
        assert_eq!(
            refusal(parse_quote! { #[function(discount = "a")] struct F(u32); }),
            "`FunctionConfig` is derived on a struct with named fields, or without fields"
        );
    }
}
