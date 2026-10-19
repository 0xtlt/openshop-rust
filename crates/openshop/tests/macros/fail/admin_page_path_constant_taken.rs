use openshop::prelude::*;

struct Reviews;

#[admin_page("/reviews")]
impl Reviews {
    const PATH: &'static str = "/other";
}

fn main() {}
