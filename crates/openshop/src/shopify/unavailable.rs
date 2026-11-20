//! A shop Shopify no longer serves to the app: the Admin API refuses every
//! request of the installation with `401`, `402`, `403`, `404` or `423`.
//! Nothing is wrong with the application: a run that meets it is not a
//! failure (see [`RunOutcome::ShopUnavailable`](crate::RunOutcome::ShopUnavailable)).

use crate::server::shop_domain::strict_shop_domain;

/// The statuses of the Admin API that say the shop is not served: the token
/// refused (`401`), the shop frozen (`402`), blocked (`403`), closed (`404`)
/// or locked (`423`).
const UNAVAILABLE_STATUSES: [u16; 5] = [401, 402, 403, 404, 423];

/// The error a [`ShopifyClient`](crate::ShopifyClient) call fails with when
/// the Admin API answers that it no longer serves the shop. It is the context
/// of the [`shopify_api::Error::Http`] Shopify answered, which
/// `downcast_ref::<shopify_api::Error>()` still finds.
///
/// [`shop_unavailable`] reads its status from any error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopUnavailable {
    /// The shop, `<name>.myshopify.com`.
    pub shop: String,
    /// The handle of the Shopify app.
    pub shopify_app: String,
    /// The status of the Admin API: `401`, `402`, `403`, `404` or `423`.
    pub status: u16,
}

impl ShopUnavailable {
    /// What Shopify means by the status.
    fn meaning(&self) -> &'static str {
        match self.status {
            401 => "the access token is refused, the app was uninstalled",
            402 => "the shop is frozen",
            403 => "the shop is blocked",
            404 => "the shop is closed",
            423 => "the shop is locked",
            _ => "the shop is unavailable",
        }
    }
}

impl std::fmt::Display for ShopUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Shop \"{}\" is unavailable to app \"{}\": the Admin API answered {} ({})",
            self.shop,
            self.shopify_app,
            self.status,
            self.meaning()
        )
    }
}

impl std::error::Error for ShopUnavailable {}

/// The status of the Admin API when `error` says that Shopify no longer
/// serves the shop: `401` (the access token is refused: the app was
/// uninstalled), `402` (the shop is frozen), `403` (blocked), `404` (closed)
/// or `423` (locked). `None` for any other error.
///
/// It reads the error of a [`ShopifyClient`](crate::ShopifyClient) call, and
/// a [`shopify_api::Error::Http`] of the Admin API anywhere in the chain, such
/// as the error of a [`ShopifyClient::api`](crate::ShopifyClient::api) call:
/// no need to downcast and keep a list of statuses.
///
/// A run whose flow fails with such an error for its own shop is not a
/// failure: it ends as
/// [`RunOutcome::ShopUnavailable`](crate::RunOutcome::ShopUnavailable). A
/// flow that goes on for the other shops, or a route that answers something
/// else, tells it with this function:
///
/// ```
/// use openshop::prelude::*;
///
/// async fn total(ctx: &FlowContext, shops: &[openshop::ShopifyClient]) -> Result<usize> {
///     let mut served = 0;
///     for shopify in shops {
///         match shopify.execute(graphql!("query { shop { id } }")).await {
///             Ok(_) => served += 1,
///             Err(error) => match openshop::shop_unavailable(&error) {
///                 Some(status) => ctx.logger.warn(format!("{} skipped ({status})", shopify.shop)),
///                 None => return Err(error),
///             },
///         }
///     }
///     Ok(served)
/// }
/// # let _ = total;
/// ```
pub fn shop_unavailable(error: &anyhow::Error) -> Option<u16> {
    if let Some(unavailable) = error.downcast_ref::<ShopUnavailable>() {
        return Some(unavailable.status);
    }
    admin_api_refusal(error).map(|(status, _)| status)
}

/// The status the run of `shop` and `shopify_app` stops with when its flow
/// failed with `error`: the shop of the run is unavailable. A
/// [`ShopUnavailable`] of another shop or app is a failure of the run, and so
/// is a bare refusal of the Admin API sent to another host than the shop.
pub(crate) fn run_shop_unavailable(error: &anyhow::Error, shop: &str, shopify_app: &str) -> Option<u16> {
    let shop = strict_shop_domain(shop)?;
    if let Some(unavailable) = error.downcast_ref::<ShopUnavailable>() {
        return (unavailable.shop == shop && unavailable.shopify_app == shopify_app).then_some(unavailable.status);
    }
    let (status, host) = admin_api_refusal(error)?;
    host.eq_ignore_ascii_case(&shop).then_some(status)
}

/// A refusal of the Admin API in the chain of `error`: its status, one of
/// [`UNAVAILABLE_STATUSES`], and the host it was sent to.
fn admin_api_refusal(error: &anyhow::Error) -> Option<(u16, &str)> {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<shopify_api::Error>())
        .find_map(refusal)
}

/// The status and the host of `error` when it is a refusal of the Admin API:
/// a status of [`UNAVAILABLE_STATUSES`] at `/admin/api/…`. Not one of the
/// token endpoint, the Partner API or a bulk download.
pub(crate) fn refusal(error: &shopify_api::Error) -> Option<(u16, &str)> {
    let shopify_api::Error::Http { status, url, .. } = error else {
        return None;
    };
    if !UNAVAILABLE_STATUSES.contains(status) {
        return None;
    }
    let address = url.split_once("://").map_or(url.as_str(), |(_, address)| address);
    let (host, path) = address.split_once('/')?;
    path.starts_with("admin/api/").then_some((*status, host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unavailable(status: u16) -> ShopUnavailable {
        ShopUnavailable {
            shop: "a.myshopify.com".into(),
            shopify_app: "default".into(),
            status,
        }
    }

    #[test]
    fn the_message_names_the_shop_the_app_and_what_shopify_means() {
        assert_eq!(
            unavailable(402).to_string(),
            "Shop \"a.myshopify.com\" is unavailable to app \"default\": the Admin API answered 402 (the shop is frozen)"
        );
        assert!(unavailable(401).to_string().contains("the app was uninstalled"));
    }

    #[test]
    fn the_status_is_read_through_context() {
        use anyhow::Context;
        let result: anyhow::Result<()> = Err(unavailable(404).into());
        let error = result.context("while syncing").unwrap_err();
        assert_eq!(shop_unavailable(&error), Some(404));
        assert_eq!(run_shop_unavailable(&error, "a.myshopify.com", "default"), Some(404));
        assert_eq!(run_shop_unavailable(&error, "a", "default"), Some(404));
        assert_eq!(run_shop_unavailable(&error, "b.myshopify.com", "default"), None);
        assert_eq!(run_shop_unavailable(&error, "a.myshopify.com", "other"), None);
    }

    #[test]
    fn other_errors_are_not_an_unavailable_shop() {
        assert_eq!(shop_unavailable(&anyhow::anyhow!("Not Found")), None);
        let error: anyhow::Error = shopify_api::Error::InvalidShop("x".into()).into();
        assert_eq!(shop_unavailable(&error), None);
    }
}
