use openshop::prelude::*;

#[derive(Model)]
#[model("reviews")]
struct Review {
    id: Uuid,
    #[model(column = "writer", column = "author")]
    author: String,
}

fn main() {}
