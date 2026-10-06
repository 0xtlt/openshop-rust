use openshop::{Model, define_model};

pub fn reviews() -> Model {
    define_model("reviews")
        .column("product_id", "text NOT NULL")
        .column("customer_id", "text NOT NULL")
        .column("rating", "integer NOT NULL")
        .column("text", "text NOT NULL")
        .column("approved", "boolean DEFAULT false")
}
