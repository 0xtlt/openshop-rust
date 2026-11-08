//! Building blocks declared with an attribute, and the application made of them.

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::app::{OpenShopBuilder, OpenShopConfig};
use crate::engine::dispatch::{DispatchOptions, QueuedRun};
use crate::types::{ProxyContext, RouteContext, ShopScope};

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

/// A type that is `T`. Lets generated code take a value of a known type
/// through a generic parameter, so that a bound on it is only checked where
/// the function is called.
#[doc(hidden)]
pub trait Is<T> {}

impl<T> Is<T> for T {}

/// Queues a run of a flow for the shop of a context. What the `dispatch` of a
/// `#[flow]` calls.
#[doc(hidden)]
pub async fn dispatch(scope: &impl ShopScope, flow: &str, input: Option<Value>) -> Result<QueuedRun> {
    let mut options = DispatchOptions::for_scope(flow, scope);
    options.input = input;
    scope.app().dispatch_flow(options).await
}

/// The input of a request does not fit what its handler takes. A proxy or a
/// route answers it with `422 Unprocessable Entity` and the message, whether
/// the typed input of the handler refused the request or the handler returned
/// the error itself: `bail!(InvalidInput("rating must be 1 to 5".into()))`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidInput(pub String);

impl std::fmt::Display for InvalidInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Invalid input: {}", self.0)
    }
}

impl std::error::Error for InvalidInput {}

/// What Shopify adds to the query of an app proxy request to sign it. The
/// typed input of a handler is made of the parameters of the request itself;
/// the verified identity is in the context.
const PROXY_SIGNING_PARAMETERS: [&str; 5] = ["shop", "path_prefix", "timestamp", "signature", "logged_in_customer_id"];

/// A query string as the type a handler takes: numbers and booleans are read from their text.
fn from_query<T: DeserializeOwned>(pairs: impl IntoIterator<Item = (String, String)>) -> Result<T, InvalidInput> {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    serde_urlencoded::from_str(&query).map_err(|error| InvalidInput(error.to_string()))
}

/// The typed input of a `#[proxy]`: the query string, or the JSON body.
#[doc(hidden)]
pub fn proxy_input<T: DeserializeOwned>(ctx: &ProxyContext, query: bool) -> Result<T> {
    let input = if query {
        from_query(
            ctx.query
                .iter()
                .filter(|(name, _)| !PROXY_SIGNING_PARAMETERS.contains(&name.as_str()))
                .map(|(name, value)| (name.clone(), value.clone())),
        )
    } else {
        serde_json::from_value(ctx.body.clone().unwrap_or(Value::Null)).map_err(|error| InvalidInput(error.to_string()))
    };
    Ok(input?)
}

/// The typed input of a `#[route]`: the query string, or the JSON body. A
/// request that does not fit is answered with `422`.
#[doc(hidden)]
pub fn route_input<T: DeserializeOwned, A>(
    ctx: &RouteContext<A>,
    query: bool,
) -> Result<T, Box<crate::response::Response>> {
    let input = if query {
        from_query(ctx.request.query())
    } else if ctx.request.body.is_empty() {
        // No body is `null`, as for a proxy: an `Option` takes it.
        serde_json::from_value(Value::Null).map_err(|error| InvalidInput(error.to_string()))
    } else {
        serde_json::from_slice(&ctx.request.body).map_err(|error| InvalidInput(error.to_string()))
    };
    input.map_err(|invalid| {
        Box::new(crate::response::json_with_status(
            crate::response::StatusCode::UNPROCESSABLE_ENTITY,
            serde_json::json!({ "error": invalid.to_string() }),
        ))
    })
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
        self.run_with(|app| app).await
    }

    /// [`App::run`] with settings decided at startup, on top of the
    /// declaration: `configure` takes its builder once `.env` is loaded, and
    /// what it sets replaces what the declaration says.
    pub async fn run_with(&self, configure: impl FnOnce(OpenShopBuilder) -> OpenShopBuilder) -> Result<()> {
        crate::cli::run(|| configure(self.builder()?).build()).await
    }

    /// [`App::run`] for a `main` that is not `async`: loads `.env` for `dev`,
    /// starts the runtime and runs the command given on the command line.
    ///
    /// ```no_run
    /// # static APP: openshop::App = openshop::app! {};
    /// fn main() -> openshop::Result<()> {
    ///     APP.main()
    /// }
    /// ```
    pub fn main(&self) -> Result<()> {
        self.main_with(|app| app)
    }

    /// [`App::main`] with settings decided at startup, on top of the
    /// declaration, for what `app!` cannot know: `configure` takes its builder
    /// once `.env` is loaded, and what it sets replaces what the declaration
    /// says. The Shopify endpoints of a simulator in CI, for instance:
    ///
    /// ```no_run
    /// # use openshop::prelude::*;
    /// # static APP: App = app! {};
    /// fn main() -> Result<()> {
    ///     let simulator = std::env::args().any(|arg| arg == "--simulator");
    ///     APP.main_with(|app| {
    ///         if simulator {
    ///             app.shopify_endpoints(ShopifyEndpoints::new().admin_origin("http://localhost:9393"))
    ///         } else {
    ///             app
    ///         }
    ///     })
    /// }
    /// ```
    pub fn main_with(&self, configure: impl FnOnce(OpenShopBuilder) -> OpenShopBuilder) -> Result<()> {
        // `dev` reads `.env` into the environment, which no thread may read meanwhile:
        // before the runtime and its threads exist.
        let args: Vec<String> = std::env::args().skip(1).collect();
        crate::cli::env::load_dev_env(&args, &std::env::current_dir()?);
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(self.run_with(configure))
    }
}
