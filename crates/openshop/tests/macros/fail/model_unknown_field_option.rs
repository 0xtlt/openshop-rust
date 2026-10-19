use openshop::prelude::*;

#[derive(Model)]
#[model("reviews")]
struct Review {
    id: Uuid,
    #[model(indexed)]
    author: String,
}

fn main() {}
