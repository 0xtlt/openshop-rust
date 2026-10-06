use openshop::{ProxyDefinition, Value};

pub fn reviews() -> ProxyDefinition {
    ProxyDefinition::liquid().get(|ctx| async move {
        Ok(Value::from(format!(
            r#"
      <div class="openshop-reviews">
        <h2>Customer Reviews for {{{{ shop.name }}}}</h2>
        <p>Reviews loaded via OpenShop App Proxy from {}</p>
        {{% if customer %}}
          <p>Hello {{{{ customer.first_name }}}}! Leave a review below.</p>
        {{% endif %}}
      </div>
    "#,
            ctx.shop
        )))
    })
}
