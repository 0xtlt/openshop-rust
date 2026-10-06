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

use crate::app::OpenShop;
use crate::server::shop_domain::normalize_shop_domain;
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

/// Connectors of every configured provider, keyed by provider name.
#[derive(Clone, Default)]
pub struct Connectors {
    connectors: Arc<IndexMap<String, Connector>>,
}

impl Connectors {
    pub fn new(connectors: IndexMap<String, Connector>) -> Self {
        Self {
            connectors: Arc::new(connectors),
        }
    }

    pub fn get(&self, provider: &str) -> Result<&Connector> {
        self.connectors
            .get(provider)
            .ok_or_else(|| anyhow!("Unknown provider \"{provider}\""))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.connectors.keys().map(String::as_str)
    }

    pub async fn call(&self, provider: &str, method: &str, args: impl Serialize) -> Result<Value> {
        self.get(provider)?.call(method, args).await
    }

    pub async fn call_as<T: DeserializeOwned>(&self, provider: &str, method: &str, args: impl Serialize) -> Result<T> {
        self.get(provider)?.call_as(method, args).await
    }

    /// Calls the method behind a typed [`ProviderCall`].
    ///
    /// `args` is a reference to the argument type of the call, or to what it
    /// borrows as: `&[T]` for a `Vec<T>`, `&str` for a `String`.
    pub async fn invoke<A, R, B>(&self, call: ProviderCall<A, R>, args: &B) -> Result<R>
    where
        A: Borrow<B>,
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let (provider, method) = (call.provider(), call.method());
        let result = self.call(provider, method, args).await?;
        serde_json::from_value(result).map_err(|error| anyhow!("Invalid result from \"{provider}.{method}\": {error}"))
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

pub async fn build_connectors(app: &OpenShop, shop: &str, shopify_app: &str) -> Result<Connectors> {
    let providers = &app.config().providers;
    if providers.is_empty() {
        return Ok(Connectors::default());
    }

    let stored: Vec<(String, Option<Value>)> =
        sqlx::query_as("SELECT provider_name, config FROM provider_configs WHERE app_handle = $1 AND shop = $2")
            .bind(shopify_app)
            .bind(shop)
            .fetch_all(app.db())
            .await?;

    let mut connectors = IndexMap::new();
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

    Ok(Connectors::new(connectors))
}

pub(crate) async fn is_installed_shop(app: &OpenShop, shopify_app: &str, shop: &str) -> Result<bool> {
    let installation: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM installations \
         WHERE app_handle = $1 AND shop = $2 AND access_token IS NOT NULL AND uninstalled_at IS NULL \
         LIMIT 1",
    )
    .bind(shopify_app)
    .bind(shop)
    .fetch_optional(app.db())
    .await?;
    Ok(installation.is_some())
}

/// Resolves an actively installed shop and its connectors.
pub async fn resolve_shop_context(app: &OpenShop, input: RouteShopInput) -> Result<RouteShopContext> {
    let Some(shop) = normalize_shop_domain(&input.shop) else {
        bail!("[openshop] Invalid shop domain");
    };
    let shopify_app = app.apps().by_handle(input.shopify_app.as_deref())?.handle.clone();

    if !is_installed_shop(app, &shopify_app, &shop).await? {
        bail!("[openshop] Shop is not actively installed");
    }

    Ok(RouteShopContext {
        connectors: build_connectors(app, &shop, &shopify_app).await?,
        shop,
        shopify_app,
    })
}
