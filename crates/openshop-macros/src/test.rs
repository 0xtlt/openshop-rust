//! `#[openshop::test(APP)]`: a test that runs against the application.

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Expr, FnArg, ItemFn, Type};

const SHAPE: &str =
    "`#[openshop::test]` goes on `async fn(ctx: &TestContext)`, or `&mut TestContext` to create resources";

pub fn test(arguments: TokenStream, item: ItemFn) -> syn::Result<TokenStream> {
    let krate = quote!(::openshop);
    // An application, or an expression that resolves to a test context.
    let subject: Expr = syn::parse2(arguments).map_err(|error| {
        syn::Error::new(
            error.span(),
            "expected what the test starts from: `#[openshop::test(APP)]`, or an expression that gives a test context",
        )
    })?;

    let signature = &item.sig;
    if signature.asyncness.is_none() || !signature.generics.params.is_empty() {
        return Err(syn::Error::new(signature.ident.span(), SHAPE));
    }
    let mut inputs = signature.inputs.iter();
    let (Some(FnArg::Typed(context)), None) = (inputs.next(), inputs.next()) else {
        return Err(syn::Error::new(signature.paren_token.span.join(), SHAPE));
    };
    let Type::Reference(reference) = &*context.ty else {
        return Err(syn::Error::new(context.ty.span(), SHAPE));
    };
    let borrow = match reference.mutability {
        Some(_) => quote!(&mut context),
        None => quote!(&context),
    };

    // The attributes of the test (`#[ignore]`, documentation…) go on the test
    // the harness sees; the function itself runs inside it.
    let attributes = &item.attrs;
    let visibility = &item.vis;
    let name = &signature.ident;
    let mut body = item.clone();
    body.attrs.clear();
    body.vis = syn::Visibility::Inherited;

    Ok(quote! {
        #(#attributes)*
        #[#krate::testing::tokio::test(crate = "::openshop::testing::tokio")]
        #visibility async fn #name() {
            #body

            #[allow(unused_mut)]
            let mut context = {
                use #krate::testing::IntoTestContext as _;
                (#subject).into_test_context().await.expect("the test context starts")
            };
            let outcome = #name(#borrow).await;
            context.shutdown().await.expect("the test context shuts down");
            #krate::testing::TestOutcome::check(outcome);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn refusal(arguments: TokenStream, item: ItemFn) -> String {
        match test(arguments, item) {
            Ok(_) => panic!("accepted"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn a_test_starts_the_application_and_shuts_it_down() {
        let generated = test(
            quote!(crate::APP),
            parse_quote! {
                /// Runs the flow.
                #[ignore = "slow"]
                async fn runs_the_flow(ctx: &TestContext) -> Result<()> {
                    ctx.run_flow("sync", json!({})).await?;
                    Ok(())
                }
            },
        )
        .unwrap()
        .to_string();
        for expected in [
            "# [doc = r\" Runs the flow.\"] # [ignore = \"slow\"] # [:: openshop :: testing :: tokio :: test (crate = \"::openshop::testing::tokio\")] async fn runs_the_flow ()",
            "async fn runs_the_flow (ctx : & TestContext) -> Result < () >",
            "use :: openshop :: testing :: IntoTestContext as _ ; (crate :: APP) . into_test_context () . await",
            "let outcome = runs_the_flow (& context) . await ;",
            "context . shutdown () . await",
            ":: openshop :: testing :: TestOutcome :: check (outcome) ;",
        ] {
            assert!(generated.contains(expected), "missing `{expected}` in:\n{generated}");
        }

        let mutable = test(quote!(APP), parse_quote! { async fn creates(ctx: &mut TestContext) {} })
            .unwrap()
            .to_string();
        assert!(mutable.contains("creates (& mut context) . await"), "{mutable}");
    }

    #[test]
    fn refuses_what_is_not_an_async_function_of_a_borrowed_context() {
        assert_eq!(
            refusal(quote!(), parse_quote! { async fn runs(ctx: &TestContext) {} }),
            "expected what the test starts from: `#[openshop::test(APP)]`, or an expression that gives a test context"
        );
        for item in [
            parse_quote! { fn runs(ctx: &TestContext) {} },
            parse_quote! { async fn runs() {} },
            parse_quote! { async fn runs(ctx: TestContext) {} },
            parse_quote! { async fn runs(ctx: &TestContext, more: u32) {} },
        ] {
            assert_eq!(refusal(quote!(APP), item), SHAPE);
        }
    }
}
