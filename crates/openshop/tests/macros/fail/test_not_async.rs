use openshop::prelude::*;

static APP: App = app! {};

#[openshop::test(APP)]
fn runs(_ctx: &openshop::testing::TestContext) {}

fn main() {}
