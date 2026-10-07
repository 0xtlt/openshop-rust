//! Building blocks declared with an attribute, and the application made of them.

use anyhow::Result;

use crate::app::{OpenShopBuilder, OpenShopConfig};

/// A building block declared with an attribute: a provider, a flow, a model,
/// a webhook… The attribute implements this; an application lists the blocks
/// it is made of, in [`app!`](crate::app!) or with [`OpenShopBuilder::add`].
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a declared building block",
    note = "declare it with `#[provider]`, `#[flow]`, `#[webhook]`, `#[proxy]`, `#[route]`, `#[admin_page]`, `#[mcp_tool]`, `#[mcp_resource]`, `#[derive(Model)]` or `#[derive(FunctionConfig)]`"
)]
pub trait Declared {
    /// What the block is: one of the types of [`kind`].
    type Kind;

    /// Adds the block to an application.
    fn register(app: OpenShopBuilder) -> OpenShopBuilder;
}

/// What a declared block is. A list of `app!` accepts the blocks of its kind.
pub mod kind {
    macro_rules! kinds {
        ($($(#[$doc:meta])* $name:ident),* $(,)?) => {
            $($(#[$doc])* pub enum $name {})*
        };
    }

    kinds! {
        /// A `#[provider]`.
        Provider,
        /// A `#[flow]`.
        Flow,
        /// A `#[derive(FunctionConfig)]` struct.
        Function,
        /// A `#[derive(Model)]` struct.
        Model,
        /// An `#[admin_page]`.
        AdminPage,
        /// A `#[webhook]`.
        Webhook,
        /// A `#[proxy]`.
        Proxy,
        /// A `#[route]`.
        Route,
        /// An `#[mcp_tool]`.
        McpTool,
        /// An `#[mcp_resource]`.
        McpResource,
    }
}

/// The declaration of an application, as [`app!`](crate::app!) writes it.
///
/// It is a value: a binary runs it, a test reads its configuration.
pub struct App {
    define: fn() -> Result<OpenShopBuilder>,
}

impl App {
    pub const fn new(define: fn() -> Result<OpenShopBuilder>) -> Self {
        Self { define }
    }

    /// The blocks and settings of the declaration, to add to them. Fails when
    /// a setting of the declaration does.
    pub fn builder(&self) -> Result<OpenShopBuilder> {
        (self.define)()
    }

    /// The validated configuration.
    pub fn config(&self) -> Result<OpenShopConfig> {
        self.builder()?.build()
    }

    /// Runs the command given on the command line: `dev`, `start`, `worker`, `migrate`…
    pub async fn run(&self) -> Result<()> {
        crate::cli::run(|| self.config()).await
    }
}
