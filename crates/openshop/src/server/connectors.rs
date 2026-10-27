//! Provider connectors: provider methods bound to a shop's stored configuration.

use std::borrow::Borrow;
use std::future::Future;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::app::OpenShop;
use crate::runtime::logger;
use crate::server::shop_domain::normalize_shop_domain;
use crate::shopify::client::ShopifyClient;
use crate::shopify::client_credentials::Grant;
use crate::types::{ProviderCall, RouteShopContext, RouteShopInput};

pub type ConnectorMethod = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

/// The callable methods of one provider for one shop.
#[derive(Clone, Default)]
pub struct Connector {
    methods: Arc<IndexMap<String, ConnectorMethod>>,
}

impl Connector {
    pub fn new(methods: IndexMap<String, ConnectorMethod>) -> Self {
        Self {
            methods: Arc::new(methods),
        }
    }

    pub fn method_names(&self) -> impl Iterator<Item = &str> {
        self.methods.keys().map(String::as_str)
    }

    pub fn has(&self, method: &str) -> bool {
        self.methods.contains_key(method)
    }

    /// Calls a method with JSON-serializable arguments.
    pub async fn call(&self, method: &str, args: impl Serialize) -> Result<Value> {
        let Some(function) = self.methods.get(method) else {
            bail!("Unknown provider method \"{method}\"");
        };
        function(serde_json::to_value(args)?).await
    }

    /// Calls a method and deserializes its result.
    pub async fn call_as<T: DeserializeOwned>(&self, method: &str, args: impl Serialize) -> Result<T> {
        Ok(serde_json::from_value(self.call(method, args).await?)?)
    }
}

/// Where the connectors of a [`Connectors`] come from.
enum Source {
    /// Given as they are, such as the fakes of a test.
    Ready(IndexMap<String, Connector>),
    /// Built from the configuration a shop saved, at the first call.
    Shop {
        app: OpenShop,
        shop: String,
        shopify_app: String,
        loaded: OnceCell<IndexMap<String, Connector>>,
    },
}

/// The providers of an application for one shop, keyed by provider name.
///
/// The connectors of a context read nothing until a method is called: the
/// configuration the shop saved is then loaded once and kept.
#[derive(Clone)]
pub struct Connectors {
    source: Arc<Source>,
}

impl Default for Connectors {
    fn default() -> Self {
        Self::new(IndexMap::new())
    }
}

impl Connectors {
    pub fn new(connectors: IndexMap<String, Connector>) -> Self {
        Self {
            source: Arc::new(Source::Ready(connectors)),
        }
    }

    /// The providers of the application, with the configuration `shop` saved
    /// for the Shopify app `shopify_app`.
    pub fn for_shop(app: &OpenShop, shop: impl Into<String>, shopify_app: impl Into<String>) -> Self {
        Self {
            source: Arc::new(Source::Shop {
                app: app.clone(),
                shop: shop.into(),
                shopify_app: shopify_app.into(),
                loaded: OnceCell::new(),
            }),
        }
    }

    async fn loaded(&self) -> Result<&IndexMap<String, Connector>> {
        match &*self.source {
            Source::Ready(connectors) => Ok(connectors),
            Source::Shop {
                app,
                shop,
                shopify_app,
                loaded,
            } => loaded.get_or_try_init(|| load_connectors(app, shop, shopify_app)).await,
        }
    }

    /// The connector of a provider.
    pub async fn get(&self, provider: &str) -> Result<&Connector> {
        self.loaded()
            .await?
            .get(provider)
            .ok_or_else(|| anyhow!("Unknown provider \"{provider}\""))
    }

    /// The names of the providers.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        let names: Vec<&str> = match &*self.source {
            Source::Ready(connectors) => connectors.keys().map(String::as_str).collect(),
            Source::Shop { app, .. } => app.config().providers.keys().map(String::as_str).collect(),
        };
        names.into_iter()
    }

    /// Calls a method by the names of its provider and of the method, with
    /// JSON-serializable arguments. Prefer the typed form a `#[provider]`
    /// block generates, `Warehouse::on(&connectors).push(&orders)`, wherever
    /// the provider is known when the code is written.
    pub async fn call(&self, provider: &str, method: &str, args: impl Serialize) -> Result<Value> {
        self.get(provider).await?.call(method, args).await
    }

    /// Calls a method and deserializes its result.
    pub async fn call_as<T: DeserializeOwned>(&self, provider: &str, method: &str, args: impl Serialize) -> Result<T> {
        let result = self.call(provider, method, args).await?;
        serde_json::from_value(result).map_err(|error| anyhow!("Invalid result from \"{provider}.{method}\": {error}"))
    }

    /// Calls the method behind a typed [`ProviderCall`]. What the connector
    /// struct of a `#[provider]` block does for each method.
    #[doc(hidden)]
    pub async fn invoke<A, R, B>(&self, call: ProviderCall<A, R>, args: &B) -> Result<R>
    where
        A: Borrow<B>,
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        self.call_as(call.provider(), call.method(), args).await
    }
}

/// Wraps a closure as a connector method.
pub fn connector_method<F, Fut>(method: F) -> ConnectorMethod
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value>> + Send + 'static,
{
    Arc::new(move |args| Box::pin(method(args)))
}

/// The connectors of every provider, each method bound to the configuration the shop saved.
async fn load_connectors(app: &OpenShop, shop: &str, shopify_app: &str) -> Result<IndexMap<String, Connector>> {
    let providers = &app.config().providers;
    let mut connectors = IndexMap::new();
    if providers.is_empty() {
        return Ok(connectors);
    }

    let stored: Vec<(String, Option<Value>)> =
        sqlx::query_as("SELECT provider_name, config FROM provider_configs WHERE app_handle = $1 AND shop = $2")
            .bind(shopify_app)
            .bind(shop)
            .fetch_all(app.db())
            .await?;

    for (name, provider) in providers {
        let stored_config = stored
            .iter()
            .find(|(provider_name, _)| provider_name == name)
            .and_then(|(_, config)| config.as_ref());
        let provider_config = app.crypto().decrypt_config(stored_config)?;

        let mut methods = IndexMap::new();
        for (method_name, method) in &provider.methods {
            let method = method.clone();
            let provider_config = provider_config.clone();
            methods.insert(
                method_name.clone(),
                Arc::new(move |args| method(provider_config.clone(), args)) as ConnectorMethod,
            );
        }
        connectors.insert(name.clone(), Connector::new(methods));
    }

    Ok(connectors)
}

/// Whether `shop` has the Shopify app `shopify_app` installed, with an access
/// token. A shop without one that `grant` lets the caller ask a token for is
/// given one with the client credentials grant first; when Shopify refuses,
/// or the token cannot be stored, the shop is not installed, and the caller
/// does what it did without the grant.
pub(crate) async fn is_installed_shop(
    app: &OpenShop,
    shopify_app: &str,
    shop: &str,
    grant: Option<&Grant>,
) -> Result<bool> {
    let installation: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM installations \
         WHERE app_handle = $1 AND shop = $2 AND access_token IS NOT NULL AND uninstalled_at IS NULL \
         LIMIT 1",
    )
    .bind(shopify_app)
    .bind(shop)
    .fetch_optional(app.db())
    .await?;
    if installation.is_some() {
        return Ok(true);
    }
    let Some(grant) = grant else {
        return Ok(false);
    };

    match app.client_credentials().grant(app, shopify_app, shop, grant).await {
        Ok(granted) => Ok(granted.is_some()),
        Err(error) => {
            logger::warn(format!("{error:#}"));
            Ok(false)
        }
    }
}

/// Resolves an actively installed shop and its connectors. Nothing vouches
/// for the shop of the input: only the shop of the `client_credentials_shop` setting
/// may get a token with the client credentials grant here.
pub async fn resolve_shop_context(app: &OpenShop, input: RouteShopInput) -> Result<RouteShopContext> {
    let Some(shop) = normalize_shop_domain(&input.shop) else {
        bail!("[openshop] Invalid shop domain");
    };
    let shopify_app = app.apps().by_handle(input.shopify_app.as_deref())?.handle.clone();

    if !is_installed_shop(app, &shopify_app, &shop, Some(&Grant::Background)).await? {
        bail!("[openshop] Shop is not actively installed");
    }

    Ok(RouteShopContext {
        db: app.db().clone(),
        shopify: ShopifyClient::new(app, &shop, &shopify_app),
        connectors: Connectors::for_shop(app, &shop, &shopify_app),
        app: app.clone(),
        shop,
        shopify_app,
    })
}
