use openshop::prelude::*;

struct Reviews;

// A view of the WebAssembly admin UI, which no longer exists: a view returns
// `Markup`, which the server renders.
#[admin_page("/reviews")]
impl Reviews {
    #[view]
    fn view() -> impl std::fmt::Display {
        "Reviews"
    }
}

fn main() {}
