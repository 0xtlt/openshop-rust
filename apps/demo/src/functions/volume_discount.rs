use openshop::prelude::*;

/// A percentage off once the cart holds enough items. The fields are what a
/// merchant fills for each discount created from the admin.
#[derive(Debug, FunctionConfig)]
#[function(
    "volumeDiscount",
    discount = "volume-discount",
    modes(automatic, code),
    title = Self::title,
    starts_at,
    ends_at,
    combines_with(product_discounts, order_discounts = false, shipping_discounts)
)]
pub struct VolumeDiscount {
    #[field(label = "Minimum quantity", validate = validators::integer_min(1))]
    pub min_quantity: u32,
    #[field(label = "Discount percentage", range(min = 0, max = 100))]
    pub percentage: f64,
    #[field(label = "Excluded product tags", placeholder = "gift, sample")]
    pub excluded_tags: Option<String>,
}

impl VolumeDiscount {
    /// The title Shopify shows for a discount.
    fn title(&self) -> String {
        format!("{}% off {}+ items", self.percentage, self.min_quantity)
    }
}
