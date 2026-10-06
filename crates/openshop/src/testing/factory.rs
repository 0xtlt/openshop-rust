//! Factories create Shopify resources for a test and clean them up afterwards.

use std::sync::Arc;

use anyhow::Result;
use futures::future::BoxFuture;
use serde_json::Value;

use crate::runtime::logger;
use crate::shopify::client::ShopifyClient;

type CreateFn<T> = Arc<dyn Fn(ShopifyClient, Value) -> BoxFuture<'static, Result<T>> + Send + Sync>;
type DestroyFn<T> = Arc<dyn Fn(ShopifyClient, T) -> BoxFuture<'static, Result<()>> + Send + Sync>;

pub struct Factory<T> {
    create: CreateFn<T>,
    destroy: DestroyFn<T>,
}

impl<T> Clone for Factory<T> {
    fn clone(&self) -> Self {
        Self {
            create: self.create.clone(),
            destroy: self.destroy.clone(),
        }
    }
}

/// Define a test factory from a `create` (client, overrides) and a `destroy` (client, resource) function.
pub fn define_factory<T, C, CFut, D, DFut>(create: C, destroy: D) -> Factory<T>
where
    C: Fn(ShopifyClient, Value) -> CFut + Send + Sync + 'static,
    CFut: Future<Output = Result<T>> + Send + 'static,
    D: Fn(ShopifyClient, T) -> DFut + Send + Sync + 'static,
    DFut: Future<Output = Result<()>> + Send + 'static,
{
    Factory {
        create: Arc::new(move |shopify, overrides| Box::pin(create(shopify, overrides))),
        destroy: Arc::new(move |shopify, resource| Box::pin(destroy(shopify, resource))),
    }
}

type Cleanup = Box<dyn FnOnce(ShopifyClient) -> BoxFuture<'static, Result<()>> + Send>;

/// Tracks created resources for cleanup.
pub struct FactoryScope {
    shopify: ShopifyClient,
    entries: Vec<Cleanup>,
}

impl FactoryScope {
    pub fn new(shopify: ShopifyClient) -> Self {
        Self {
            shopify,
            entries: Vec::new(),
        }
    }

    /// Create a resource using a factory. Tracked for cleanup.
    pub async fn create<T: Clone + Send + 'static>(&mut self, factory: &Factory<T>, overrides: Value) -> Result<T> {
        let resource = (factory.create)(self.shopify.clone(), overrides).await?;
        let destroy = factory.destroy.clone();
        let tracked = resource.clone();
        self.entries.push(Box::new(move |shopify| destroy(shopify, tracked)));
        Ok(resource)
    }

    /// Cleanup all created resources in reverse order (LIFO).
    /// Errors are logged but don't block cleanup of remaining resources.
    pub async fn cleanup(&mut self) {
        while let Some(destroy) = self.entries.pop() {
            if let Err(error) = destroy(self.shopify.clone()).await {
                logger::warn(format!("[openshop:test] cleanup failed: {error:#}"));
            }
        }
    }

    /// Number of tracked resources
    pub fn size(&self) -> usize {
        self.entries.len()
    }
}
