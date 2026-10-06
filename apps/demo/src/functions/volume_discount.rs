use openshop::{
    CombinesWith, DiscountMode, FieldDef, FunctionDefinition, FunctionOwner, ShopifyFunctionType, validators,
};

pub fn volume_discount() -> FunctionDefinition {
    FunctionDefinition::new(ShopifyFunctionType::Discount, "volume-discount")
        .modes([DiscountMode::Automatic, DiscountMode::Code])
        .owner(
            FunctionOwner::dynamic_title(|config| {
                format!("{}% off {}+ items", config["percentage"], config["minQuantity"])
            })
            .starts_at(true)
            .ends_at(true)
            .combines_with(CombinesWith {
                product_discounts: Some(true),
                order_discounts: Some(false),
                shipping_discounts: Some(true),
            }),
        )
        .field(
            "minQuantity",
            FieldDef::number("Minimum quantity").validate(validators::integer_min(1)),
        )
        .field(
            "percentage",
            FieldDef::number("Discount percentage").validate(validators::number_range(0.0, 100.0)),
        )
        .field(
            "excludedTags",
            FieldDef::text("Excluded product tags").placeholder("gift, sample"),
        )
}
