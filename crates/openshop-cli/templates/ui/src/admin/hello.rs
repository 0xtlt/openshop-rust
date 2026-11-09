//! The `/hello` admin page: what the server answers, and the view it renders
//! with it.

use maud::html;
use openshop::prelude::*;
use serde::{Deserialize, Serialize};

/// What the loader returns, and the view shows.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Greeting {
    pub message: String,
}

#[input]
pub struct SayHello {
    #[schemars(length(min = 1, max = 40))]
    pub name: String,
}

pub struct HelloPage;

#[admin_page("/hello", nav = "Hello")]
impl HelloPage {
    #[loader]
    async fn greeting(ctx: AdminContext) -> Result<Greeting> {
        Ok(Greeting {
            message: format!("Hello {}", ctx.shop),
        })
    }

    /// The form of the page: the page is rendered again, with a toast. A name
    /// the input schema refuses is shown under its field.
    #[action]
    async fn say_hello(_ctx: AdminContext, input: SayHello) -> Result<Outcome> {
        Ok(Outcome::toast(format!("Hello {}", input.name)))
    }

    /// Rendered by the server with what `greeting` returns, and the form the
    /// page refused, if it refused one.
    #[view]
    fn view(greeting: Greeting, form: FormState) -> Markup {
        html! {
            s-page heading="Hello" {
                s-section {
                    s-paragraph { (greeting.message) }
                }
                s-section heading="Say hello" {
                    form method="post" {
                        input type="hidden" name="_action" value=(Self::SAY_HELLO);
                        s-stack gap="base" {
                            s-text-field name="name" label="Name"
                                value=(form.value_or("name", "")) error=[form.error("name")] {}
                            s-button type="submit" variant="primary" { "Say hello" }
                        }
                    }
                }
            }
        }
    }
}
