use openshop::prelude::*;

#[derive(Model)]
#[model("settings")]
struct Setting {
    id: Uuid,
    table: String,
}

fn main() {}
