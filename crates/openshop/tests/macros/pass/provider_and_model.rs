use openshop::prelude::*;

#[derive(Debug, ProviderConfig)]
struct Warehouse {
    #[field(label = "API URL", url)]
    api_url: String,
    #[field(password)]
    api_key: String,
    #[field(select("Europe" = "eu", "United States" = "us"))]
    region: Option<String>,
}

#[provider("warehouse")]
impl Warehouse {
    pub async fn ping(&self) -> Result<String> {
        Ok(self.api_url.clone())
    }
}

#[derive(Debug, Clone, Model)]
#[model("reviews", index([shop, product_id]))]
struct Review {
    id: Uuid,
    shop: String,
    product_id: String,
    #[model(default = "false")]
    approved: bool,
    // Named after a constant of the framework in the other case: `TABLE` is not generated.
    table_name: String,
}

static APP: App = app! {
    providers: [Warehouse],
    models: [Review],
};

fn main() {
    assert_eq!(Warehouse::NAME, "warehouse");
    let _ = Warehouse::PING;
    assert_eq!(<Review as openshop::Record>::TABLE, "reviews");
    let _ = Review::PRODUCT_ID;
    let _ = Review::TABLE_NAME;
    let _ = &APP;
}
