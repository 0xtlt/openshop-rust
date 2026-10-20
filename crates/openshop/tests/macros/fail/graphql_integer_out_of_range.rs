use openshop::prelude::*;

fn main() {
    let _ = graphql!("{ products(first: 3000000000) { nodes { title } } }");
}
