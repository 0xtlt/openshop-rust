//! The Shopify Admin API of an installed shop, through `shopify_api`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use shopify_api::{ApiVersion, Operation, Shopify};
use tokio::sync::Mutex;

use super::client_credentials::{Grant, GrantClient, stored};
use crate::app::OpenShop;
use crate::runtime::transport::USER_AGENT;
use crate::server::shop_domain::strict_shop_domain;

/// The `shopify_api` client of an app and shop, as a client and its clones
/// resolved it.
#[derive(Clone)]
enum Resolved {
    /// The shop has no token, and none may be asked for.
    None,
    /// The stored token, sent as it is.
    Stored(Shopify),
    /// The shared client that renews the token with the client credentials
    /// grant, for a caller of this kind.
    Granted(GrantClient, Grant),
}

type Clients = Arc<Mutex<HashMap<(String, String), Resolved>>>;

/// The Shopify Admin API of one shop.
///
/// It runs on a [`shopify_api::Shopify`] client: its typed operations
/// ([`execute`](Self::execute) with [`graphql!`](crate::graphql)), its
/// retries of throttled requests, and the rest of its API through
/// [`api`](Self::api): pagination, bulk operations, webhook subscriptions.
///
/// Creating a client reads nothing: the access token of the installation is
/// read from the database at the first request, then kept for that Shopify
/// app and shop. A clone whose `shop` or `shopify_app` was changed reads the
/// token of its own app and shop.
///
/// The shop is checked at each request: `<name>.myshopify.com` or `<name>`,
/// nothing else, and the address the token goes to is built from it.
///
/// For the shop of the `client_credentials_shop` setting, a shop without a
/// token gets one with the client credentials grant. A token obtained that
/// way is renewed five minutes before it expires, and a token the Admin API
/// refuses with a `401` is renewed once and the request sent again once, for
/// any shop whose installation row holds it. The client of an Admin API
/// request also asks for the shop and app of its verified session token when
/// it has no token, and for those only: a clone pointed at another shop or
/// app may do what background work may.
#[derive(Clone)]
pub struct ShopifyClient {
    pub shop: String,
    pub shopify_app: String,
    api_version: Option<ApiVersion>,
    app: OpenShop,
    /// What lets this client ask Shopify for a token: background work, or
    /// the shop and app Shopify vouched for.
    grant: Option<Grant>,
    clients: Clients,
}

impl ShopifyClient {
    /// The client of a shop installed for a Shopify app of the application.
    pub fn new(app: &OpenShop, shop: impl Into<String>, shopify_app: impl Into<String>) -> Self {
        Self {
            shop: shop.into(),
            shopify_app: shopify_app.into(),
            api_version: None,
            app: app.clone(),
            grant: Some(Grant::Background),
            clients: Arc::default(),
        }
    }

    /// Sends every request to `api_version` instead of the version each
    /// operation was checked against.
    pub fn with_api_version(mut self, api_version: ApiVersion) -> Self {
        self.api_version = Some(api_version);
        self
    }

    /// What lets this client ask Shopify for a token of its shop.
    pub(crate) fn granting(mut self, grant: Option<Grant>) -> Self {
        self.grant = grant;
        self
    }

    /// The shop, checked.
    fn domain(&self) -> Result<String> {
        strict_shop_domain(&self.shop).ok_or_else(|| {
            anyhow!(
                "[openshop] Invalid shop domain for app \"{}\": expected <name>.myshopify.com",
                self.shopify_app
            )
        })
    }

    /// The `shopify_api` client of the shop, resolved at the first request
    /// and kept for this app and shop.
    async fn resolved(&self) -> Result<Resolved> {
        let shop = self.domain()?;
        let key = (self.shopify_app.clone(), shop.clone());
        if let Some(resolved) = self.clients.lock().await.get(&key) {
            return Ok(resolved.clone());
        }
        let stored = stored(&self.app, &self.shopify_app, &shop).await?;
        let granted = match &self.grant {
            Some(grant) => self
                .app
                .client_credentials()
                .client(&self.app, &self.shopify_app, &shop, grant, stored.as_ref())
                .await?
                .map(|client| Resolved::Granted(client, grant.clone())),
            None => None,
        };
        let resolved = match (granted, stored.and_then(|stored| stored.token)) {
            (Some(granted), _) => granted,
            (None, Some(token)) => Resolved::Stored(
                Shopify::builder(&shop)
                    .shared_transport(self.app.transport().clone())
                    .user_agent(USER_AGENT)
                    .access_token(token)
                    .build()?,
            ),
            (None, None) => Resolved::None,
        };
        // A shop without a token may have one at the next request: a grant
        // held back no longer is, a merchant installed the app.
        if !matches!(resolved, Resolved::None) {
            self.clients.lock().await.insert(key, resolved.clone());
        }
        Ok(resolved)
    }

    /// The `shopify_api` client of `resolved`, pinned to the version of this
    /// client if it has one.
    fn client_of(&self, resolved: &Resolved) -> Result<Shopify> {
        let client = match resolved {
            Resolved::None => bail!(
                "[openshop] No access token found for app \"{}\" and shop \"{}\". Is the app installed?",
                self.shopify_app,
                self.domain()?,
            ),
            Resolved::Stored(client) => client.clone(),
            Resolved::Granted(granted, _) => granted.client.clone(),
        };
        Ok(match self.api_version {
            Some(version) => client.with_api_version(version),
            None => client,
        })
    }

    /// The `shopify_api` client of the shop, for what this client does not
    /// wrap: [`Shopify::paginate`], the bulk operations, the webhook
    /// subscriptions, [`Shopify::execute_response`]. Fails when the shop has
    /// no access token.
    ///
    /// A failure of the client credentials grant under it is not remembered,
    /// and nothing is sent again without the grant, as the methods of this
    /// client do.
    pub async fn api(&self) -> Result<Shopify> {
        self.client_of(&self.resolved().await?)
    }

    /// Runs `call` with the client of the shop. When the client credentials
    /// grant fails under it (a refusal, a failure, another token stored
    /// meanwhile), the failure is remembered and `call` runs once more as it
    /// would without the grant: with the token of the row, or none.
    async fn run<T, F, Fut>(&self, call: F) -> Result<T>
    where
        F: Fn(Shopify) -> Fut,
        Fut: Future<Output = shopify_api::Result<T>>,
    {
        let resolved = self.resolved().await?;
        let error = match call(self.client_of(&resolved)?).await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let Resolved::Granted(granted, grant) = &resolved else {
            return Err(error.into());
        };
        let shop = self.domain()?;
        if !self
            .app
            .client_credentials()
            .failed(&self.shopify_app, &shop, grant, granted, &error)
        {
            return Err(error.into());
        }
        self.clients
            .lock()
            .await
            .remove(&(self.shopify_app.clone(), shop));
        match self.resolved().await? {
            // A token Shopify gave that could not be stored: that is the answer.
            Resolved::None if matches!(error, shopify_api::Error::TokenStore(_)) => Err(error.into()),
            resolved => Ok(call(self.client_of(&resolved)?).await?),
        }
    }

    /// Runs an operation of [`graphql!`](crate::graphql), checked against
    /// Shopify's schema while the crate compiled, and returns its typed `data`.
    ///
    /// Fails when Shopify answers with `errors`, with a status other than
    /// 2xx, or with data that does not fit the operation.
    pub async fn execute<D: DeserializeOwned>(&self, operation: impl Into<Operation<D>>) -> Result<D> {
        let operation = operation.into();
        self.run(|client| {
            let operation = operation.clone();
            async move { client.execute(operation).await }
        })
        .await
    }

    /// Runs any query or mutation and returns its `data`. Nothing checks the
    /// text: prefer [`execute`](Self::execute) with [`graphql!`](crate::graphql).
    ///
    /// `variables` is anything that serializes to an object, or `()` for none.
    pub async fn graphql(&self, query: &str, variables: impl Serialize) -> Result<Value> {
        let variables = serde_json::to_value(variables)?;
        self.run(|client| {
            let variables = &variables;
            async move { client.graphql(query, variables).await }
        })
        .await
    }

    /// [`graphql`](Self::graphql), with `data` read as `T`.
    pub async fn graphql_as<T: DeserializeOwned>(&self, query: &str, variables: impl Serialize) -> Result<T> {
        let variables = serde_json::to_value(variables)?;
        self.run(|client| {
            let variables = &variables;
            async move { client.graphql_as(query, variables).await }
        })
        .await
    }

    /// Runs any query or mutation and returns Shopify's answer as it is,
    /// `errors` included; `None` when the shop has no access token. A status
    /// other than 2xx is an error, [`shopify_api::Error::Http`].
    pub(crate) async fn graphql_raw(
        &self,
        query: &str,
        variables: impl Serialize,
    ) -> Result<Option<shopify_api::Result<shopify_api::RawResponse>>> {
        if matches!(self.resolved().await?, Resolved::None) {
            return Ok(None);
        }
        let variables = serde_json::to_value(variables)?;
        let outcome = self
            .run(|client| {
                let variables = &variables;
                async move { client.graphql_raw(query, variables).await }
            })
            .await;
        match outcome {
            Ok(answer) => Ok(Some(Ok(answer))),
            // What Shopify answered is the caller's to read; anything else is an error.
            Err(error) => match error.downcast::<shopify_api::Error>() {
                Ok(error) => Ok(Some(Err(error))),
                Err(error) => Err(error),
            },
        }
    }
}
