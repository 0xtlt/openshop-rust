use openshop::prelude::*;

#[derive(Model)]
#[model("reviews", index([shop, product]))]
struct Review {
    id: Uuid,
    shop: String,
}

fn main() {}
