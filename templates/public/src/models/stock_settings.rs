use openshop::chrono::{DateTime, Utc};
use openshop::prelude::*;

/// What a merchant chose for their shop: one row per shop, once they saved.
#[derive(Debug, Clone, PartialEq, Model)]
#[model("stock_settings", unique([shop]))]
pub struct StockSettings {
    pub id: Uuid,
    pub shop: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// A variant with fewer units than this runs low.
    pub threshold: i32,
}

impl StockSettings {
    /// The threshold of a shop that saved none.
    pub const DEFAULT_THRESHOLD: i32 = 5;

    /// The threshold of the shop of a context.
    pub async fn threshold_of(scope: &impl ShopScope) -> Result<i32> {
        let saved = Self::of(scope).first().await?;
        Ok(saved.map_or(Self::DEFAULT_THRESHOLD, |settings| settings.threshold))
    }
}
