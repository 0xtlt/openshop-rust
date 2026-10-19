use openshop::prelude::*;

fn main() {
    let _ = graphql!("query($id: ID!) { product(id: $id) { title } }");
}
