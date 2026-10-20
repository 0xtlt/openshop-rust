use openshop::prelude::*;

fn main() {
    let _ = graphql!("query($first: Int = \"ten\") { products(first: $first) { nodes { title } } }");
}
