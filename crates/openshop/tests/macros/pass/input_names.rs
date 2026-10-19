use openshop::prelude::*;

/// Fields are camel case outside Rust.
#[input]
struct Query {
    product_id: String,
}

/// A rule of the type is kept, as `rename_all_fields` is not one for the type.
#[input]
#[serde(rename_all = "snake_case")]
struct Kept {
    product_id: String,
}

#[input]
#[serde(rename = "rename_all")]
struct Renamed {
    product_id: String,
}

fn main() {
    let query = Query { product_id: "1".into() };
    assert_eq!(json!(query), json!({ "productId": "1" }));
    let kept = Kept { product_id: "1".into() };
    assert_eq!(json!(kept), json!({ "product_id": "1" }));
    // `rename_all` as the text of another option does not name the fields.
    let renamed = Renamed { product_id: "1".into() };
    assert_eq!(json!(renamed), json!({ "productId": "1" }));
}
