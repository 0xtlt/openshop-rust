use openshop::{Value, Webhook};

pub fn orders_create() -> Webhook {
    Webhook::new(|ctx| async move {
        let field = |name: &str| ctx.payload.get(name).and_then(Value::as_str).unwrap_or_default();
        println!(
            "[webhook] New order on {}: {} — {}",
            ctx.shop,
            field("name"),
            field("total_price")
        );
        Ok(())
    })
}
