//! Application configuration and the runtime handle shared by every component.

use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use indexmap::IndexMap;
use sqlx::PgPool;

use crate::admin::{AdminPage, CustomAdminPage};
use crate::config::validate::validate_openshop_config;
use crate::db::Table;
use crate::declare::Declared;
use crate::engine::dispatch::{DispatchOptions, QueuedRun, dispatch_flow};
#[cfg(feature = "sentry")]
use crate::runtime::SentryConfig;
use crate::runtime::{HttpTransport, ReqwestTransport, RuntimeEnv};
use crate::server::connectors::Connectors;
use crate::server::crypto::Crypto;
use crate::server::mcp::core::create_core_mcp_capabilities;
use crate::server::mcp::registry::{McpRegistry, build_mcp_registry};
use crate::server::shopify_apps::ShopifyApps;
use crate::server::static_ui::UiSource;
use crate::shopify::client::ShopifyClient;
use crate::shopify::client_credentials::ClientCredentials;
use crate::types::{
    AdminMenuItem, AdminPageId, AdminPageMode, AdminPagesConfig, Cron, ErrorContext, Flow, Function, McpConfig,
    McpResource, McpTool, OnErrorFn, Provider, Proxy, RetryPolicy, Route, ShopifyConfig, Webhook, WorkerConfig,
};

/// The validated definition of an OpenShop application.
#[derive(Clone, Default)]
pub struct OpenShopConfig {
    pub shopify: Option<ShopifyConfig>,
    pub providers: IndexMap<String, Provider>,
    pub flows: IndexMap<String, Flow>,
    pub functions: IndexMap<String, Function>,
    pub mcp: Option<McpConfig>,
    /// Keyed by topic, either `orders/create` or `ORDERS_CREATE`.
    pub webhooks: IndexMap<String, Webhook>,
    pub crons: Vec<Cron>,
    /// Which built-in pages of the admin are visible, hidden or disabled.
    pub builtin_pages: Option<AdminPagesConfig>,
    /// The entries of the admin menu for the pages of the application, when
    /// set by hand. `None` takes them from the pages that declare a `nav` label.
    pub admin_menu: Option<Vec<AdminMenuItem>>,
    pub worker: WorkerConfig,
    pub retry_policy: RetryPolicy,
    pub on_error: Option<OnErrorFn>,
    /// App proxy handlers by route path (`/reviews`, `/api/reviews/:id`), served under `/proxy` and `/ext`.
    pub proxies: IndexMap<String, Proxy>,
    /// Public server routes by route path, served under `/routes`.
    pub routes: IndexMap<String, Route>,
    /// Server side of application-defined admin pages.
    pub admin_pages: Vec<AdminPage>,
    /// Application tables created by the development schema push.
    pub models: Vec<Table>,
    /// Admin UI bundle served by `dev` and `start` instead of the built-in one,
    /// for applications with custom admin pages.
    pub admin_ui: Option<UiSource>,
    /// Maximum size of a request body. Defaults to 2 MB.
    pub max_request_body_bytes: Option<usize>,
    /// The password of the operator dashboard, served under `/ops` when set.
    pub ops_password: Option<String>,
    /// The development store whose token work no Shopify request vouches for
    /// may get with the client credentials grant.
    pub client_credentials_shop: Option<String>,
    /// Where failures are reported, when the application gives a Sentry DSN.
    #[cfg(feature = "sentry")]
    pub sentry: Option<SentryConfig>,
}

impl OpenShopConfig {
    pub fn builder() -> OpenShopBuilder {
        OpenShopBuilder::default()
    }

    /// Whether the application has admin pages of its own: it lists some,
    /// or sets `admin_menu`.
    pub fn custom_pages_enabled(&self) -> bool {
        !self.admin_pages.is_empty() || self.admin_menu.is_some()
    }

    /// The entries of the admin menu for the pages of the application: the
    /// ones set with `admin_menu`, else one per page that declares a `nav`
    /// label, in the order the pages are listed.
    pub fn resolved_admin_menu(&self) -> Vec<AdminMenuItem> {
        match &self.admin_menu {
            Some(menu) => menu.clone(),
            None => self
                .admin_pages
                .iter()
                .filter_map(|page| Some(AdminMenuItem::new(page.nav.clone()?, page.path.clone())))
                .collect(),
        }
    }

    /// Custom admin pages sorted by specificity. Fails on reserved or ambiguous routes.
    pub fn resolved_admin_pages(&self) -> Result<Vec<CustomAdminPage>> {
        crate::admin::resolve_custom_admin_pages(&self.admin_pages)
    }
}

/// Collects the pieces of an application, then validates them as a whole.
#[derive(Clone, Default)]
pub struct OpenShopBuilder {
    config: OpenShopConfig,
    /// What cannot be registered, reported by [`OpenShopBuilder::build`].
    problems: Vec<String>,
    /// The last `sentry` setting, checked by [`OpenShopBuilder::build`].
    #[cfg(feature = "sentry")]
    sentry: Option<SentryConfig>,
}

/// Registers `value` under `key`, once: a second registration is a problem.
fn insert_once<V>(map: &mut IndexMap<String, V>, problems: &mut Vec<String>, section: &str, key: String, value: V) {
    if map.contains_key(&key) {
        problems.push(format!("{section}.{key} is registered twice"));
    } else {
        map.insert(key, value);
    }
}

impl OpenShopBuilder {
    /// Adds a building block declared with an attribute: a `#[provider]`, a
    /// `#[flow]`, a `#[derive(Model)]` struct…
    pub fn add<T: Declared>(self) -> Self {
        T::register(self)
    }

    /// [`OpenShopBuilder::add`] for a block of a given kind: what a list of `app!` expands to.
    #[doc(hidden)]
    pub fn add_as<K, T: Declared<Kind = K>>(self) -> Self {
        T::register(self)
    }

    pub fn shopify(mut self, shopify: ShopifyConfig) -> Self {
        self.config.shopify = Some(shopify);
        self
    }

    /// Registers a provider under its own name.
    pub fn provider(self, provider: Provider) -> Self {
        let key = provider.name.clone();
        self.provider_as(key, provider)
    }

    pub fn provider_as(mut self, key: impl Into<String>, provider: Provider) -> Self {
        insert_once(
            &mut self.config.providers,
            &mut self.problems,
            "providers",
            key.into(),
            provider,
        );
        self
    }

    /// Registers several providers, each under its own name.
    pub fn providers(self, providers: impl IntoIterator<Item = Provider>) -> Self {
        providers.into_iter().fold(self, Self::provider)
    }

    /// Registers a flow under its own name.
    pub fn flow(self, flow: Flow) -> Self {
        let key = flow.name.clone();
        self.flow_as(key, flow)
    }

    pub fn flow_as(mut self, key: impl Into<String>, flow: Flow) -> Self {
        insert_once(&mut self.config.flows, &mut self.problems, "flows", key.into(), flow);
        self
    }

    /// Registers several flows, each under its own name.
    pub fn flows(self, flows: impl IntoIterator<Item = Flow>) -> Self {
        flows.into_iter().fold(self, Self::flow)
    }

    /// Applies a function to the builder, so that a module can register
    /// everything it defines (flows, routes, pages…) in one place:
    /// `.with(reviews::register)`.
    pub fn with(self, register: impl FnOnce(Self) -> Self) -> Self {
        register(self)
    }

    pub fn function(mut self, key: impl Into<String>, function: Function) -> Self {
        insert_once(
            &mut self.config.functions,
            &mut self.problems,
            "functions",
            key.into(),
            function,
        );
        self
    }

    /// Adds MCP permissions, tools and resources to the ones already registered.
    pub fn mcp(mut self, mcp: McpConfig) -> Self {
        let McpConfig {
            enabled,
            custom_permissions,
            tools,
            resources,
        } = mcp;
        let problems = &mut self.problems;
        let registered = self.config.mcp.get_or_insert_default();
        if enabled.is_some() {
            registered.enabled = enabled;
        }
        for (key, permission) in custom_permissions {
            insert_once(
                &mut registered.custom_permissions,
                problems,
                "mcp.permissions",
                key,
                permission,
            );
        }
        for (name, tool) in tools {
            insert_once(&mut registered.tools, problems, "mcp.tools", name, tool);
        }
        for (uri, resource) in resources {
            insert_once(&mut registered.resources, problems, "mcp.resources", uri, resource);
        }
        self
    }

    pub fn mcp_tool(self, name: impl Into<String>, tool: McpTool) -> Self {
        self.mcp(McpConfig::new().tool(name, tool))
    }

    pub fn mcp_resource(self, uri: impl Into<String>, resource: McpResource) -> Self {
        self.mcp(McpConfig::new().resource(uri, resource))
    }

    pub fn webhook(mut self, topic: impl Into<String>, webhook: Webhook) -> Self {
        insert_once(
            &mut self.config.webhooks,
            &mut self.problems,
            "webhooks",
            topic.into(),
            webhook,
        );
        self
    }

    pub fn cron(mut self, cron: Cron) -> Self {
        self.config.crons.push(cron);
        self
    }

    pub fn crons(mut self, crons: impl IntoIterator<Item = Cron>) -> Self {
        self.config.crons.extend(crons);
        self
    }

    /// Sets the mode of several built-in pages of the admin: flows,
    /// providers, crons, functions, MCP. Merges with earlier calls.
    pub fn builtin_pages(mut self, pages: AdminPagesConfig) -> Self {
        self.config.builtin_pages = Some(self.config.builtin_pages.unwrap_or_default().merge(pages));
        self
    }

    /// Sets the mode of one built-in page of the admin.
    pub fn builtin_page(mut self, id: AdminPageId, mode: AdminPageMode) -> Self {
        let mut pages = self.config.builtin_pages.unwrap_or_default();
        pages.set(id, mode);
        self.config.builtin_pages = Some(pages);
        self
    }

    /// Sets the entries of the admin menu for the pages of the application,
    /// in place of the ones the pages declare (`#[admin_page("/path", nav =
    /// "Label")]`): for another order, or another label.
    pub fn admin_menu(mut self, menu: impl IntoIterator<Item = AdminMenuItem>) -> Self {
        self.config.admin_menu = Some(menu.into_iter().collect());
        self
    }

    pub fn worker(mut self, worker: WorkerConfig) -> Self {
        self.config.worker = worker;
        self
    }

    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.config.retry_policy = policy;
        self
    }

    pub fn on_error<F, Fut>(mut self, on_error: F) -> Self
    where
        F: Fn(Arc<anyhow::Error>, ErrorContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.config.on_error = Some(Arc::new(move |error, context| Box::pin(on_error(error, context))));
        self
    }

    /// Registers handlers of an app proxy path such as `/reviews` or
    /// `/api/reviews/:id`. The methods of a path can be registered separately.
    pub fn proxy(mut self, path: impl Into<String>, proxy: Proxy) -> Self {
        let path = path.into();
        match self.config.proxies.get_mut(&path) {
            Some(registered) => {
                if let Err(problem) = registered.merge(proxy) {
                    self.problems.push(format!("proxies.{path} {problem}"));
                }
            }
            None => {
                self.config.proxies.insert(path, proxy);
            }
        }
        self
    }

    /// Registers handlers of a public server route such as `/ping` or
    /// `/orders/:id`. The methods of a path can be registered separately, each
    /// with its own authentication.
    pub fn route<A: Send + 'static>(mut self, path: impl Into<String>, route: Route<A>) -> Self {
        let path = path.into();
        let route = route.erased();
        match self.config.routes.get_mut(&path) {
            Some(registered) => {
                if let Err(problem) = registered.merge(route) {
                    self.problems.push(format!("routes.{path} {problem}"));
                }
            }
            None => {
                self.config.routes.insert(path, route);
            }
        }
        self
    }

    pub fn admin_page(mut self, page: AdminPage) -> Self {
        self.config.admin_pages.push(page);
        self
    }

    pub fn model(mut self, model: Table) -> Self {
        if self.config.models.iter().any(|known| known.table == model.table) {
            self.problems
                .push(format!("models.{} is registered twice", model.table));
        } else {
            self.config.models.push(model);
        }
        self
    }

    /// Serves a custom admin UI bundle instead of the built-in one.
    pub fn admin_ui(mut self, ui: UiSource) -> Self {
        self.config.admin_ui = Some(ui);
        self
    }

    pub fn max_request_body_bytes(mut self, bytes: usize) -> Self {
        self.config.max_request_body_bytes = Some(bytes);
        self
    }

    /// Serves the operator dashboard under `/ops`, behind this password: what
    /// happens in every shop, read-only, outside Shopify admin. Without a
    /// password, or with an empty one, `/ops` does not exist.
    ///
    /// The application gives the password, from wherever it keeps its
    /// secrets: `ops_password: std::env::var("OPS_PASSWORD").ok()`.
    pub fn ops_password(mut self, password: impl Into<Option<String>>) -> Self {
        self.config.ops_password = password.into().filter(|password| !password.trim().is_empty());
        self
    }

    /// Names the one development store of the organization that owns the app
    /// for which work no Shopify request vouches for (flows, crons, routes,
    /// proxies, webhook handlers, the MCP tool) may get an access token with
    /// the client credentials grant, without an installation row. Without it,
    /// or with an empty value, only a signed launch from Shopify admin or a
    /// verified session token asks for a token, and background work only
    /// renews the token a shop's row holds. The application refuses to start
    /// when it is not a `*.myshopify.com` domain.
    ///
    /// The application gives the shop, from wherever it keeps its settings:
    /// `client_credentials_shop: std::env::var("SHOPIFY_CLIENT_CREDENTIALS_SHOP").ok()`.
    pub fn client_credentials_shop(mut self, shop: impl Into<Option<String>>) -> Self {
        self.config.client_credentials_shop = shop.into().filter(|shop| !shop.trim().is_empty());
        self
    }

    /// Reports to Sentry the runs that fail, the handlers that fail and the
    /// panics. A DSN is enough; [`SentryConfig`] also names the environment
    /// and the release. Without a DSN, or with an empty one, nothing is
    /// reported.
    ///
    /// The application gives the DSN, from wherever it keeps its settings:
    /// `sentry: std::env::var("SENTRY_DSN").ok()`.
    #[cfg(feature = "sentry")]
    pub fn sentry(mut self, sentry: impl Into<SentryConfig>) -> Self {
        self.sentry = Some(sentry.into());
        self
    }

    #[cfg_attr(not(feature = "sentry"), allow(unused_mut))]
    pub fn build(mut self) -> Result<OpenShopConfig> {
        if let Some(problem) = self.problems.first() {
            anyhow::bail!("[openshop] Invalid config: {problem}");
        }
        #[cfg(feature = "sentry")]
        if let Some(sentry) = self.sentry.take() {
            self.config.sentry = sentry
                .resolve()
                .map_err(|problem| anyhow::anyhow!("[openshop] Invalid config: {problem}"))?;
        }
        validate_openshop_config(&self.config)?;
        Ok(self.config)
    }
}

struct Inner {
    config: OpenShopConfig,
    db: PgPool,
    env: RuntimeEnv,
    crypto: Crypto,
    apps: ShopifyApps,
    transport: Arc<dyn HttpTransport>,
    client_credentials: ClientCredentials,
    custom_pages: Vec<CustomAdminPage>,
    mcp_registry: McpRegistry,
}

/// A running application: configuration plus the database pool and environment.
/// Cheap to clone; every handler context carries one.
#[derive(Clone)]
pub struct OpenShop {
    inner: Arc<Inner>,
}

impl OpenShop {
    pub fn new(config: OpenShopConfig, db: PgPool, env: RuntimeEnv) -> Result<Self> {
        Self::with_transport(config, db, env, Arc::new(ReqwestTransport::new()))
    }

    /// Like [`OpenShop::new`] with a custom transport for Shopify HTTP calls.
    pub fn with_transport(
        config: OpenShopConfig,
        db: PgPool,
        env: RuntimeEnv,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self> {
        validate_openshop_config(&config)?;
        crate::server::ops::check_password(config.ops_password.as_deref(), env.production)?;
        let apps = ShopifyApps::resolve(config.shopify.as_ref(), &env)?;
        let crypto = Crypto::new(env.encryption_key.clone(), env.production);
        let client_credentials = ClientCredentials::new(config.client_credentials_shop.as_deref())?;
        let custom_pages = config.resolved_admin_pages()?;
        let mcp_registry = build_mcp_registry(config.mcp.as_ref(), &create_core_mcp_capabilities())?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                db,
                env,
                crypto,
                apps,
                transport,
                client_credentials,
                custom_pages,
                mcp_registry,
            }),
        })
    }

    /// Connects to `DATABASE_URL` and reads settings from the environment.
    pub async fn connect(config: OpenShopConfig) -> Result<Self> {
        Self::new(config, crate::db::connect().await?, RuntimeEnv::from_env())
    }

    pub fn config(&self) -> &OpenShopConfig {
        &self.inner.config
    }

    pub fn db(&self) -> &PgPool {
        &self.inner.db
    }

    pub fn env(&self) -> &RuntimeEnv {
        &self.inner.env
    }

    pub fn crypto(&self) -> &Crypto {
        &self.inner.crypto
    }

    pub fn apps(&self) -> &ShopifyApps {
        &self.inner.apps
    }

    pub fn transport(&self) -> &Arc<dyn HttpTransport> {
        &self.inner.transport
    }

    /// The client credentials grant, and the shop of the
    /// `client_credentials_shop` setting background work may use it for.
    pub(crate) fn client_credentials(&self) -> &ClientCredentials {
        &self.inner.client_credentials
    }

    pub(crate) fn custom_pages(&self) -> &[CustomAdminPage] {
        &self.inner.custom_pages
    }

    /// Core and application MCP permissions, tools and resources.
    pub fn mcp_registry(&self) -> &McpRegistry {
        &self.inner.mcp_registry
    }

    /// Queues a flow run. Fails with [`crate::engine::FlowConcurrencyError`] when the
    /// flow rejects concurrent runs and one is already active for the shop.
    pub async fn dispatch_flow(&self, options: DispatchOptions) -> Result<QueuedRun> {
        dispatch_flow(self.db(), self.config(), options).await
    }

    /// The providers of a shop, bound to the configuration it saved. The
    /// configuration is read at the first call.
    pub fn connectors(&self, shop: &str, shopify_app: &str) -> Connectors {
        Connectors::for_shop(self, shop, shopify_app)
    }

    /// The Shopify Admin API of a shop. The access token of the installation
    /// is read at the first request.
    pub fn shopify_client(&self, shop: &str, shopify_app: &str) -> ShopifyClient {
        ShopifyClient::new(self, shop, shopify_app)
    }
}
