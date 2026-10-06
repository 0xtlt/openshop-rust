use openshop::chrono::{DateTime, Utc};
use openshop::{Model, Uuid};

/// A product review left by a customer.
///
/// The struct is the table: `dev` creates it, `migrate generate` writes the
/// migrations when it changes, and `Review::for_shop(&db, &shop)` reads and
/// writes its rows.
#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "reviews")]
pub struct Review {
    pub id: Uuid,
    pub shop: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub product_id: String,
    pub customer_id: String,
    pub rating: i32,
    pub text: String,
    #[model(default = "false")]
    pub approved: Option<bool>,
}
