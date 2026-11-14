//! Application configuration and the runtime handle shared by every component.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use indexmap::IndexMap;
use sqlx::PgPool;

use crate::admin::{AdminPage, AdminText, AdminTextsFn, CustomAdminPage};
use crate::billing::{BillingSource, PlanCache};
use crate::config::validate::validate_openshop_config;
use crate::db::Table;
use crate::declare::Declared;
use crate::engine::dispatch::{DispatchOptions, QueuedRun, dispatch_flow};
use crate::i18n::Locales;
#[cfg(feature = "sentry")]
use crate::runtime::SentryConfig;
use crate::runtime::{ReqwestTransport, RuntimeEnv, Transport};
use crate::server::UiSource;
use crate::server::connectors::Connectors;
use crate::server::crypto::Crypto;
use crate::server::locale::StaffLocales;
use crate::server::mcp::core::create_core_mcp_capabilities;
use crate::server::mcp::registry::{McpRegistry, build_mcp_registry};
use crate::server::shopify_apps::ShopifyApps;
use crate::server::webhooks::WebhookDeliveries;
use crate::shopify::client::ShopifyClient;
use crate::shopify::client_credentials::ClientCredentials;
use crate::shopify::endpoints::{Endpoints, ShopifyEndpoints};
use crate::shopify::partner::{PartnerApi, PartnerApiClient};
use crate::types::{
    AdminMenuItem, AdminPageId, AdminPageMode, AdminPagesConfig, Cron, ErrorContext, Flow, Function, McpConfig,
    McpResource, McpTool, OnErrorFn, Provider, Proxy, RetryPolicy, Route, ShopifyConfig, Webhook, WorkerConfig,
};

/// The validated definition of an OpenShop application.
#[derive(Clone, Default)]
pub struct OpenShopConfig {
    pub shopify: Option<ShopifyConfig>,
    /// Where the application reaches Shopify, field by field: a field that is
    /// `None` is read from its environment variable, then is Shopify's.
    pub shopify_endpoints: ShopifyEndpoints,
    /// The Partner API billing reads the plans from, field by field: a field
    /// that is `None` is read from its environment variable. Neither a
    /// Partner ID nor a token: the plans are read from the Admin API.
    pub partner_api: PartnerApi,
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
    /// How the Shopify client retries a request Shopify did not run (a `5xx`,
    /// a network failure, a throttled request). `None` is `shopify_api`'s
    /// [`RetryPolicy::default`](shopify_api::RetryPolicy): four retries,
    /// one second apart and doubling.
    pub shopify_retry: Option<shopify_api::RetryPolicy>,
    pub on_error: Option<OnErrorFn>,
    /// App proxy handlers by route path (`/reviews`, `/api/reviews/:id`), served under `/proxy` and `/ext`.
    pub proxies: IndexMap<String, Proxy>,
    /// Public server routes by route path, served under `/routes`.
    pub routes: IndexMap<String, Route>,
    /// Server side of application-defined admin pages.
    pub admin_pages: Vec<AdminPage>,
    /// Application tables created by the development schema push.
    pub models: Vec<Table>,
    /// The admin `dev` and `start` serve: the one the server renders
    /// (`UiSource::Server`) unless it is `UiSource::None`, an API-only server.
    pub admin_ui: Option<UiSource>,
    /// Maximum size of a request body. Defaults to 2 MB.
    pub max_request_body_bytes: Option<usize>,
    /// How long a request may take to get the head of its response. Defaults
    /// to 60 seconds.
    pub request_timeout: Option<std::time::Duration>,
    /// How many requests are served at once; the ones above are answered with
    /// `503`. Defaults to 1024.
    pub max_concurrent_requests: Option<usize>,
    /// Origins allowed to call `/proxy` and `/api` cross-origin besides
    /// Shopify's, the app's URL and, in development, the local hosts (`/ext`
    /// allows any origin): `https://shop.example`,
    /// or `null` in development. Normalized by [`OpenShopBuilder::cors_origins`].
    pub cors_origins: Vec<String>,
    /// The language of the admin pages, a language tag (`fr`, `fr-CA`): their
    /// `<html lang>`, and the texts openshop writes on them. `None` is
    /// English. Set by [`OpenShopBuilder::admin_locale`].
    pub admin_locale: Option<String>,
    /// The application's own texts for what openshop writes on the admin
    /// pages, before the built-in table of `admin_locale`.
    pub admin_texts: Option<AdminTextsFn>,
    /// The languages of the `locales/` folder of the application, which
    /// `app!` reads: the staff member's language, among them, is the language
    /// of their admin pages, `ctx.locale`. `None`: the pages are in
    /// `admin_locale`. Set by [`OpenShopBuilder::locales`].
    pub locales: Option<Locales>,
    /// The password of the operator dashboard, served under `/ops` when set.
    pub ops_password: Option<String>,
    /// The development store whose token work no Shopify request vouches for
    /// may get with the client credentials grant.
    pub client_credentials_shop: Option<String>,
    /// How long the log lines of the runs are kept. `None` keeps them forever.
    pub log_retention: Option<Duration>,
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

    /// The language of the admin pages, as `<html lang>` says it: the
    /// `admin_locale`, `en` without one.
    pub fn admin_lang(&self) -> &str {
        self.admin_locale.as_deref().unwrap_or("en")
    }

    /// A text openshop writes on the admin pages, in their language: the
    /// application's `admin_texts`, then the built-in table of `admin_locale`
    /// (English and French), then English.
    pub fn admin_text(&self, text: &AdminText) -> String {
        self.admin_text_in(self.admin_lang(), text)
    }

    /// A text openshop writes on the admin pages, in `language`, the language
    /// of a request ([`admin_language`](Self::admin_language)): the built-in
    /// table of that language (English and French), or `admin_texts` when it
    /// is the language of `admin_locale`; else the texts of `admin_locale`, as
    /// [`admin_text`](Self::admin_text) gives them.
    pub fn admin_text_in(&self, language: &str, text: &AdminText) -> String {
        crate::admin::admin_text(language, self.admin_lang(), self.admin_texts.as_ref(), text)
    }

    /// The language of the admin pages for a staff member whose admin is in
    /// `staff_locale` (`de-CH`), `ctx.locale`: with `locales/`, the same
    /// language, then its primary subtag (`de`), else the default file's;
    /// without, `admin_locale`.
    pub fn admin_language(&self, staff_locale: Option<&str>) -> &str {
        match &self.locales {
            Some(locales) => locales.resolve(staff_locale),
            None => self.admin_lang(),
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

    /// Where the application reaches Shopify: the Admin API and OAuth, and
    /// the App Bridge and Polaris of the admin UI, for a simulator, a proxy
    /// or an API of one's own. Merges with earlier calls field by field: a
    /// field set here replaces the earlier one, a field left `None` keeps it.
    /// So a call at startup, on the builder of the declaration
    /// ([`App::main_with`](crate::App::main_with)), wins over the
    /// `shopify_endpoints` of `app!`. A field no call sets is read from its
    /// environment variable when the application starts, then is Shopify's.
    /// It does not change the Shopify app(s) served: see
    /// [`shopify`](Self::shopify).
    pub fn shopify_endpoints(mut self, endpoints: ShopifyEndpoints) -> Self {
        self.config.shopify_endpoints = endpoints.or(std::mem::take(&mut self.config.shopify_endpoints));
        self
    }

    /// The Partner API of the organization that owns the app: with it,
    /// [`billing`](crate::billing) reads the plans of the shops from the
    /// Partner API's `activeSubscription`, as Shopify App Pricing asks,
    /// instead of the Admin API. Merges with earlier calls field by field, as
    /// [`shopify_endpoints`](Self::shopify_endpoints) does; a field no call
    /// sets is read from `SHOPIFY_PARTNER_ID` or `SHOPIFY_PARTNER_API_TOKEN`
    /// when the application starts. The application refuses to start with a
    /// Partner ID and no token, or a token and no Partner ID.
    ///
    /// ```
    /// use openshop::PartnerApi;
    ///
    /// let config = openshop::OpenShopConfig::builder()
    ///     .partner_api(PartnerApi::new().partner_id("1234567"))
    ///     .build()
    ///     .unwrap();
    /// assert_eq!(config.partner_api.partner_id.as_deref(), Some("1234567"));
    /// ```
    pub fn partner_api(mut self, partner_api: PartnerApi) -> Self {
        self.config.partner_api = partner_api.or(std::mem::take(&mut self.config.partner_api));
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

    /// Registers a flow under its own name.
    pub fn flow(mut self, flow: Flow) -> Self {
        let key = flow.name.clone();
        insert_once(&mut self.config.flows, &mut self.problems, "flows", key, flow);
        self
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

    /// How the Shopify client of every handler retries a request Shopify did
    /// not run: a query after a `5xx` or a network failure, any request
    /// Shopify throttled. Not the retries of a flow run, which
    /// [`retry_policy`](Self::retry_policy) sets. Without it, `shopify_api`'s
    /// default: four retries, from one second apart and doubling, up to
    /// thirty seconds.
    ///
    /// ```
    /// use std::time::Duration;
    /// use openshop::shopify::RetryPolicy;
    ///
    /// let config = openshop::OpenShopConfig::builder()
    ///     .shopify_retry(RetryPolicy::new(2).with_max_delay(Duration::from_secs(5)))
    ///     .build()
    ///     .unwrap();
    /// assert_eq!(config.shopify_retry.unwrap().max_retries(), 2);
    /// ```
    pub fn shopify_retry(mut self, policy: shopify_api::RetryPolicy) -> Self {
        self.config.shopify_retry = Some(policy);
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

    /// The admin to serve: `UiSource::None` for an API-only server.
    pub fn admin_ui(mut self, ui: UiSource) -> Self {
        self.config.admin_ui = Some(ui);
        self
    }

    pub fn max_request_body_bytes(mut self, bytes: usize) -> Self {
        self.config.max_request_body_bytes = Some(bytes);
        self
    }

    /// Answers `503` to a request whose response has not begun after this
    /// long. The time the handler takes to read the body counts; the time a
    /// response already begun takes to stream does not. Defaults to 60 seconds.
    pub fn request_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.config.request_timeout = Some(timeout);
        self
    }

    /// Answers `503` to a request that comes while this many are being
    /// served. `/health` is always answered. Defaults to 1024.
    pub fn max_concurrent_requests(mut self, requests: usize) -> Self {
        self.config.max_concurrent_requests = Some(requests);
        self
    }

    /// Origins allowed to call `/proxy` cross-origin, and `/api` where the
    /// server enables CORS on it (in development, and on an API-only server),
    /// besides Shopify admin and storefronts,
    /// `https://extensions.shopifycdn.com`, the app's URL (`HOST`,
    /// `SHOPIFY_APP_URL`) and, in development, the local hosts. A headless
    /// storefront on its own domain is one: `https://shop.example`. `/ext`
    /// does not read them: it answers `Access-Control-Allow-Origin: *`, as
    /// Shopify asks of a backend that UI extensions call.
    ///
    /// Each value is an origin, a scheme, a host and an optional port, or
    /// `null`, the origin of a sandboxed frame or a local file. `null` is
    /// refused in production, where any sandboxed
    /// page or local file could send it. An empty value is ignored, another
    /// value that is not an origin is a configuration error. A second call
    /// replaces the first.
    ///
    /// The application gives them, from wherever it keeps its settings:
    /// `cors_origins: std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default().split(',')`.
    pub fn cors_origins<I>(mut self, origins: I) -> Self
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        self.config.cors_origins.clear();
        for origin in origins {
            let origin = origin.as_ref();
            if origin.trim().is_empty() {
                continue;
            }
            match crate::server::parse_cors_origin(origin) {
                Ok(origin) if self.config.cors_origins.contains(&origin) => {}
                Ok(origin) => self.config.cors_origins.push(origin),
                Err(problem) => self.problems.push(format!("cors_origins: {problem}")),
            }
        }
        self
    }

    /// The language of the admin pages: `fr`, `fr-CA`, a language tag. It is
    /// their `<html lang>`, and the language of the texts openshop writes on
    /// them: the errors of the fields an input schema refuses (`Required`,
    /// `Must be at least 1`), the page of a loader that failed, the error
    /// toast of an action that failed, and the toasts of `openshop.js`.
    /// Built in: English, the default, and French; another language is
    /// English, unless [`admin_texts`](Self::admin_texts) gives its texts.
    /// The pages of the framework (flows, runs, providers…) stay in English.
    ///
    /// An empty value is ignored; a value that is not a language tag, such as
    /// `fr_FR`, is a configuration error.
    pub fn admin_locale(mut self, locale: impl AsRef<str>) -> Self {
        let locale = locale.as_ref().trim();
        if locale.is_empty() {
            self.config.admin_locale = None;
        } else if crate::admin::is_language_tag(locale) {
            self.config.admin_locale = Some(locale.to_string());
        } else {
            self.problems.push(format!(
                "admin_locale: `{locale}` is not a language tag: expected `fr` or `fr-CA`"
            ));
        }
        self
    }

    /// The application's texts for what openshop writes on the admin pages,
    /// for a language openshop has no table of, or other words than its own:
    /// a function from the [`AdminText`] to its message, `None` for one it
    /// leaves to the table of [`admin_locale`](Self::admin_locale), then to
    /// English.
    ///
    /// ```ignore
    /// admin_texts: |text: &AdminText| match text {
    ///     AdminText::Field(FieldError::Required) => Some("Pflichtfeld".to_string()),
    ///     _ => None,
    /// },
    /// ```
    pub fn admin_texts<F>(mut self, texts: F) -> Self
    where
        F: Fn(&AdminText) -> Option<String> + Send + Sync + 'static,
    {
        self.config.admin_texts = Some(std::sync::Arc::new(texts));
        self
    }

    /// The languages of the admin pages, from the files of `locales/`: what
    /// `app!` sets by itself when the package has the folder, and
    /// [`locales!`](crate::locales) gives. The staff member's language is the
    /// one of their admin pages, and of [`t!`](crate::t): see
    /// [`admin_language`](OpenShopConfig::admin_language).
    pub fn locales(mut self, locales: Locales) -> Self {
        self.config.locales = Some(locales);
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

    /// Removes the log lines older than `retention`, in small batches, from
    /// the workers (`dev`, `start` and `worker`). Without it, or with `None`,
    /// the lines are kept forever. Runs and step results are not purged.
    ///
    /// The application gives the duration, from wherever it keeps its
    /// settings: `log_retention: Duration::from_secs(30 * 24 * 3600)`.
    pub fn log_retention(mut self, retention: impl Into<Option<Duration>>) -> Self {
        self.config.log_retention = retention.into();
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
    transport: Arc<dyn Transport>,
    /// Where Shopify is reached: the configuration's, else the environment's.
    endpoints: Endpoints,
    /// The Partner API billing reads the plans from, when it is set.
    partner_api: Option<PartnerApiClient>,
    /// The plans read from the Partner API, per shop.
    plans: PlanCache,
    client_credentials: ClientCredentials,
    custom_pages: Vec<CustomAdminPage>,
    mcp_registry: McpRegistry,
    webhook_deliveries: WebhookDeliveries,
    staff_locales: StaffLocales,
}

/// A running application: configuration plus the database pool and environment.
/// Cheap to clone; every handler context carries one.
#[derive(Clone)]
pub struct OpenShop {
    inner: Arc<Inner>,
}

impl OpenShop {
    pub fn new(config: OpenShopConfig, db: PgPool, env: RuntimeEnv) -> Result<Self> {
        Self::with_transport(config, db, env, Arc::new(ReqwestTransport::new()?))
    }

    /// Like [`OpenShop::new`] with another transport for every exchange with
    /// Shopify, such as [`MockTransport`](crate::testing::MockTransport) in tests.
    pub fn with_transport(
        config: OpenShopConfig,
        db: PgPool,
        env: RuntimeEnv,
        transport: Arc<dyn Transport>,
    ) -> Result<Self> {
        validate_openshop_config(&config)?;
        crate::server::ops::check_password(config.ops_password.as_deref(), env.production)?;
        crate::server::check_cors_origins(&config.cors_origins, env.production)?;
        for (name, value) in env.invalid_app_urls() {
            tracing::warn!("[openshop] {name}={value:?} is not an http(s) URL: it allows no CORS origin");
        }
        let endpoints = Endpoints::resolve(&config.shopify_endpoints, &env.shopify_endpoints)?;
        let apps = ShopifyApps::resolve(config.shopify.as_ref(), &env)?
            .with_transport(transport.clone())
            .with_admin_origin(endpoints.admin_origin.as_ref())?;
        if let Some(line) = endpoints.startup_line() {
            tracing::info!("{line}");
        }
        let partner_api = PartnerApiClient::resolve(
            &config.partner_api,
            &env.partner_api,
            endpoints.partner_origin(),
            transport.clone(),
            config.shopify_retry.clone().unwrap_or_default(),
        )?;
        if let Some(partner_api) = &partner_api {
            tracing::info!("{}", partner_api.startup_line());
        }
        let crypto = Crypto::new(env.encryption_key.clone(), env.allows_plaintext_secrets());
        crypto.check_configured()?;
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
                endpoints,
                partner_api,
                plans: PlanCache::default(),
                client_credentials,
                custom_pages,
                mcp_registry,
                webhook_deliveries: WebhookDeliveries::default(),
                staff_locales: StaffLocales::default(),
            }),
        })
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

    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.inner.transport
    }

    /// Where this application reaches Shopify: each field of the
    /// configuration, else of its environment variable, written in full. A
    /// field that is `None` is Shopify's.
    pub fn shopify_endpoints(&self) -> &ShopifyEndpoints {
        &self.inner.endpoints.shown
    }

    /// The same, checked.
    pub(crate) fn endpoints(&self) -> &Endpoints {
        &self.inner.endpoints
    }

    /// The Partner API of the application, when a Partner ID and a token
    /// are set: what billing reads the plans from, and what sends any other
    /// query of the Partner API.
    pub fn partner_api(&self) -> Option<&PartnerApiClient> {
        self.inner.partner_api.as_ref()
    }

    /// Where billing reads the plans of the shops: the Partner API when it
    /// is set, the Admin API otherwise.
    pub fn billing_source(&self) -> BillingSource {
        match self.inner.partner_api {
            Some(_) => BillingSource::PartnerApi,
            None => BillingSource::AdminApi,
        }
    }

    /// The plans read from the Partner API, per shop.
    pub(crate) fn plans(&self) -> &PlanCache {
        &self.inner.plans
    }

    /// The client credentials grant, and the shop of the
    /// `client_credentials_shop` setting background work may use it for.
    pub(crate) fn client_credentials(&self) -> &ClientCredentials {
        &self.inner.client_credentials
    }

    /// The locale of each staff member, as their last request gave it.
    pub(crate) fn staff_locales(&self) -> &StaffLocales {
        &self.inner.staff_locales
    }

    /// The webhook deliveries this process ran, to run none of them twice.
    pub(crate) fn webhook_deliveries(&self) -> &WebhookDeliveries {
        &self.inner.webhook_deliveries
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
