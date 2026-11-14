// A key the default file of `locales/` does not have, or a plural without its
// count, does not compile.

fn main() {
    let _ = openshop::t!("home.titel");
    let _ = openshop::t!("home.low");
}
