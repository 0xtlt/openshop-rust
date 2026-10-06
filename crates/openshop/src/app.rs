//! Application configuration and the runtime handle shared by every component.

use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use indexmap::IndexMap;
use sqlx::PgPool;

use crate::admin::{AdminPage, CustomAdminPage};
use crate::config::validate::validate_openshop_config;
use crate::db::Model;
use crate::engine::dispatch::{DispatchFlowParams, DispatchFlowResult, dispatch_flow};
use crate::runtime::{HttpTransport, ReqwestTransport, RuntimeEnv};
use crate::server::connectors::{Connectors, build_connectors};
use crate::server::crypto::Crypto;
use crate::server::mcp::core::create_core_mcp_capabilities;
use crate::server::mcp::registry::{McpRegistry, build_mcp_registry};
use crate::server::shopify_apps::ShopifyApps;
use crate::server::static_ui::UiSource;
use crate::shopify::client::{ShopifyClient, create_shopify_client};
use crate::types::{
    AdminPageId, AdminPageMode, AdminPagesConfig, CronEntry, CustomAdminNavigationItem, CustomPagesConfig,
    ErrorContext, ExperimentalConfig, Flow, FunctionDefinition, McpConfig, OnErrorFn, PartialRetryPolicy,
    PartialWorkerConfig, Provider, ProxyDefinition, ServerRoute, ShopifyConfig, Webhook,
};

/// The validated definition of an OpenShop application.
#[derive(Clone, Default)]
pub struct OpenShopConfig {
    pub shopify: Option<ShopifyConfig>,
    pub providers: IndexMap<String, Provider>,
    pub flows: IndexMap<String, Flow>,
    pub functions: IndexMap<String, FunctionDefinition>,
    pub mcp: Option<McpConfig>,
    /// Keyed by topic, either `orders/create` or `ORDERS_CREATE`.
    pub webhooks: IndexMap<String, Webhook>,
    pub crons: Vec<CronEntry>,
    pub pages: Option<AdminPagesConfig>,
    pub experimental: Option<ExperimentalConfig>,
    pub worker: PartialWorkerConfig,
    pub retry_policy: PartialRetryPolicy,
    pub on_error: Option<OnErrorFn>,
    /// App proxy handlers by route path (`/reviews`, `/api/reviews/:id`), served under `/proxy` and `/ext`.
    pub proxies: IndexMap<String, ProxyDefinition>,
    /// Public server routes by route path, served under `/routes`.
    pub routes: IndexMap<String, ServerRoute>,
    /// Server side of application-defined admin pages.
    pub admin_pages: Vec<AdminPage>,
    /// Application tables created by the development schema push.
    pub models: Vec<Model>,
    /// Admin UI bundle served by `dev` and `start` instead of the built-in one,
    /// for applications with custom admin pages.
    pub admin_ui: Option<UiSource>,
    /// Maximum size of a request body. Defaults to 2 MB.
    pub max_request_body_bytes: Option<usize>,
}

impl OpenShopConfig {
    pub fn builder() -> OpenShopBuilder {
        OpenShopBuilder::default()
    }

    pub fn custom_pages_enabled(&self) -> bool {
        self.experimental
            .as_ref()
            .is_some_and(|experimental| experimental.custom_pages.is_some())
    }

    pub fn custom_navigation(&self) -> &[CustomAdminNavigationItem] {
        self.experimental
            .as_ref()
            .and_then(|experimental| experimental.custom_pages.as_ref())
            .map(|pages| pages.navigation.as_slice())
            .unwrap_or_default()
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
}

impl OpenShopBuilder {
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
        self.config.providers.insert(key.into(), provider);
        self
    }

    /// Registers a flow under its own name.
    pub fn flow(self, flow: Flow) -> Self {
        let key = flow.name.clone();
        self.flow_as(key, flow)
    }

    pub fn flow_as(mut self, key: impl Into<String>, flow: Flow) -> Self {
        self.config.flows.insert(key.into(), flow);
        self
    }

    pub fn function(mut self, key: impl Into<String>, function: FunctionDefinition) -> Self {
        self.config.functions.insert(key.into(), function);
        self
    }

    pub fn mcp(mut self, mcp: McpConfig) -> Self {
        self.config.mcp = Some(mcp);
        self
    }

    pub fn webhook(mut self, topic: impl Into<String>, webhook: Webhook) -> Self {
        self.config.webhooks.insert(topic.into(), webhook);
        self
    }

    pub fn cron(mut self, cron: CronEntry) -> Self {
        self.config.crons.push(cron);
        self
    }

    pub fn pages(mut self, pages: AdminPagesConfig) -> Self {
        self.config.pages = Some(self.config.pages.unwrap_or_default().merge(pages));
        self
    }

    pub fn page(mut self, id: AdminPageId, mode: AdminPageMode) -> Self {
        let mut pages = self.config.pages.unwrap_or_default();
        pages.set(id, mode);
        self.config.pages = Some(pages);
        self
    }

    /// Enables experimental custom admin pages and sets their sidebar entries.
    pub fn custom_pages(mut self, navigation: impl IntoIterator<Item = CustomAdminNavigationItem>) -> Self {
        self.config.experimental = Some(ExperimentalConfig {
            custom_pages: Some(CustomPagesConfig {
                navigation: navigation.into_iter().collect(),
            }),
        });
        self
    }

    pub fn experimental(mut self, experimental: ExperimentalConfig) -> Self {
        self.config.experimental = Some(experimental);
        self
    }

    pub fn worker(mut self, worker: PartialWorkerConfig) -> Self {
        self.config.worker = worker;
        self
    }

    pub fn retry_policy(mut self, policy: PartialRetryPolicy) -> Self {
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

    /// Registers an app proxy path such as `/reviews` or `/api/reviews/:id`.
    pub fn proxy(mut self, path: impl Into<String>, proxy: ProxyDefinition) -> Self {
        self.config.proxies.insert(path.into(), proxy);
        self
    }

    /// Registers a public server route such as `/ping` or `/orders/:id`.
    pub fn route(mut self, path: impl Into<String>, route: ServerRoute) -> Self {
        self.config.routes.insert(path.into(), route);
        self
    }

    pub fn admin_page(mut self, page: AdminPage) -> Self {
        self.config.admin_pages.push(page);
        self
    }

    pub fn model(mut self, model: Model) -> Self {
        self.config.models.push(model);
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

    pub fn build(self) -> Result<OpenShopConfig> {
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
        let apps = ShopifyApps::resolve(config.shopify.as_ref(), &env)?;
        let crypto = Crypto::new(env.encryption_key.clone(), env.production);
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

    pub(crate) fn custom_pages(&self) -> &[CustomAdminPage] {
        &self.inner.custom_pages
    }

    /// Core and application MCP permissions, tools and resources.
    pub fn mcp_registry(&self) -> &McpRegistry {
        &self.inner.mcp_registry
    }

    /// Queues a flow run. Fails with [`crate::engine::FlowConcurrencyError`] when the
    /// flow rejects concurrent runs and one is already active for the shop.
    pub async fn dispatch_flow(&self, params: DispatchFlowParams) -> Result<DispatchFlowResult> {
        dispatch_flow(self.db(), self.config(), params).await
    }

    /// Provider connectors bound to the shop's stored configuration.
    pub async fn connectors(&self, shop: &str, shopify_app: &str) -> Result<Connectors> {
        build_connectors(self, shop, shopify_app).await
    }

    /// Shopify Admin API client using the shop's stored access token.
    pub async fn shopify_client(&self, shop: &str, shopify_app: &str) -> Result<ShopifyClient> {
        create_shopify_client(self, shop, shopify_app, None).await
    }
}
