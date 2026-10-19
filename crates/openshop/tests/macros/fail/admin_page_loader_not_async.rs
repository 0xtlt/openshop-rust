use openshop::prelude::*;

struct Reviews;

#[admin_page("/reviews")]
impl Reviews {
    #[loader]
    fn list(_ctx: AdminContext) -> Result<Value> {
        Ok(json!([]))
    }
}

fn main() {}
