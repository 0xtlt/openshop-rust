use openshop::prelude::*;

graphql! {
    pub mod shop_name = "{ shop { name } }";
}

fn main() {
    let _ = shop_name::operation();
    let _ = graphql!(
        "query($id: ID!, $first: Int = 10) { product(id: $id) { title variants(first: $first) { nodes { id } } } }",
        id = "gid://shopify/Product/1"
    );
}
