use openshop::Webhook;

pub fn app_uninstalled() -> Webhook {
    Webhook::new(|ctx| async move {
        println!("[webhook] App uninstalled from {}", ctx.shop);
        Ok(())
    })
}
