use super::types::{FunctionDef, FunctionInstance};

pub fn can_create_instance(
    definition: Option<&FunctionDef>,
    instances: &[FunctionInstance],
    instances_loaded: bool,
) -> bool {
    match definition {
        None => false,
        Some(definition) if definition.singleton != Some(true) => true,
        Some(_) => instances_loaded && instances.is_empty(),
    }
}

pub fn instance_label(instance: &FunctionInstance) -> String {
    instance
        .label
        .clone()
        .or_else(|| instance.title.clone())
        .unwrap_or_else(|| "(untitled)".to_string())
}

pub fn instance_state(instance: &FunctionInstance) -> String {
    if let Some(state) = instance.state.as_deref().filter(|state| !state.is_empty()) {
        let mut characters = state.chars();
        let first = characters.next().map(|first| first.to_uppercase().to_string());
        return first.unwrap_or_default() + characters.as_str();
    }
    instance.status.clone().unwrap_or_else(|| {
        if instance.enabled == Some(true) {
            "Active"
        } else {
            "Inactive"
        }
        .to_string()
    })
}

pub fn instance_is_active(instance: &FunctionInstance) -> bool {
    instance.state.as_deref() == Some("active")
        || instance.status.as_deref() == Some("ACTIVE")
        || instance.enabled == Some(true)
}

pub fn has_config_fields(definition: &FunctionDef) -> bool {
    !definition.fields.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn cart_transform() -> FunctionDef {
        FunctionDef {
            label: "Bundle cart transform".to_string(),
            kind: "cart-transform".to_string(),
            handle: "cart-transform".to_string(),
            modes: None,
            supports_update: false,
            singleton: Some(true),
            fields: IndexMap::new(),
        }
    }

    fn instance() -> FunctionInstance {
        FunctionInstance {
            id: "gid://shopify/CartTransform/1".to_string(),
            label: Some("Bundle cart transform".to_string()),
            state: Some("active".to_string()),
            title: None,
            status: None,
            enabled: None,
            block_on_failure: Some(true),
            config: IndexMap::new(),
        }
    }

    #[test]
    fn renders_cart_transform_semantics_without_shopify_title_or_enabled_fields() {
        assert_eq!(instance_label(&instance()), "Bundle cart transform");
        assert_eq!(instance_state(&instance()), "Active");
        assert!(instance_is_active(&instance()));
    }

    #[test]
    fn hides_singleton_creation_and_empty_configuration() {
        assert!(can_create_instance(Some(&cart_transform()), &[], true));
        assert!(!can_create_instance(Some(&cart_transform()), &[instance()], true));
        assert!(!has_config_fields(&cart_transform()));
    }

    #[test]
    fn hides_singleton_creation_while_instance_discovery_is_unknown() {
        assert!(!can_create_instance(Some(&cart_transform()), &[], false));
    }

    #[test]
    fn falls_back_to_discount_title_status_and_enabled() {
        let discount = FunctionInstance {
            label: None,
            state: None,
            title: Some("Summer sale".to_string()),
            status: Some("EXPIRED".to_string()),
            ..instance()
        };
        assert_eq!(instance_label(&discount), "Summer sale");
        assert_eq!(instance_state(&discount), "EXPIRED");
        assert!(!instance_is_active(&discount));

        let bare = FunctionInstance {
            title: None,
            status: None,
            enabled: Some(true),
            ..discount
        };
        assert_eq!(instance_label(&bare), "(untitled)");
        assert_eq!(instance_state(&bare), "Active");
        assert!(instance_is_active(&bare));
        assert!(!can_create_instance(None, &[], true));
    }
}
