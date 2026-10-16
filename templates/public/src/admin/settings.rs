//! The `/settings` page: under how many units a variant of the shop runs low.

use leptos::prelude::*;
use leptos::task::spawn_local;
use openshop::prelude::*;
use openshop::ui::{AdminActionOptions, shopify, use_action, use_loader};
use serde::{Deserialize, Serialize};

#[input]
#[derive(Debug)]
pub struct SaveSettings {
    #[schemars(range(min = 1, max = 1000))]
    pub threshold: i32,
}

/// What the merchant chose, or what the application chooses for them.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub threshold: i32,
}

pub struct SettingsPage;

#[admin_page("/settings", nav = "Settings")]
impl SettingsPage {
    #[loader]
    async fn settings(ctx: AdminContext) -> Result<Settings> {
        use crate::models::stock_settings::StockSettings;

        Ok(Settings {
            threshold: StockSettings::threshold_of(&ctx).await?,
        })
    }

    #[action]
    async fn save_settings(ctx: AdminContext, input: SaveSettings) -> Result<Settings> {
        use crate::models::stock_settings::{NewStockSettings, StockSettings};

        // The rows of the shop of the session: one, once its merchant saved.
        let settings = StockSettings::of(&ctx);
        match settings.first().await? {
            Some(mut saved) => {
                saved.threshold = input.threshold;
                settings.save(&saved).await?;
            }
            None => {
                settings
                    .insert(NewStockSettings {
                        threshold: input.threshold,
                    })
                    .await?;
            }
        }
        Ok(Settings {
            threshold: input.threshold,
        })
    }

    #[view]
    fn view() -> impl IntoView {
        let settings = use_loader(Self::SETTINGS, || ());
        let save = use_action(Self::SAVE_SETTINGS);
        // What the field holds: what was saved, then what the merchant types.
        let threshold = RwSignal::new(String::new());
        Effect::new(move |_| {
            if let Some(saved) = settings.data.get() {
                threshold.set(saved.threshold.to_string());
            }
        });
        let save_threshold = move |_| {
            let Ok(units) = threshold.get_untracked().trim().parse::<i32>() else {
                shopify::toast_error("Enter a number of units");
                return;
            };
            spawn_local(async move {
                let options = AdminActionOptions::new().revalidate(Self::SETTINGS);
                if let Ok(Some(saved)) = save.invoke(SaveSettings { threshold: units }, options).await {
                    shopify::toast(&format!("A variant runs low under {} units", saved.threshold));
                }
            });
        };

        view! {
            <s-page heading="Settings">
                {move || settings.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
                {move || save.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
                <s-section heading="Low stock">
                    <s-stack gap="base">
                        <s-number-field
                            label="A variant runs low under"
                            suffix="units"
                            min="1"
                            max="1000"
                            prop:value=move || threshold.get()
                            on:input=move |event| threshold.set(event_target_value(&event))
                        />
                        <s-button variant="primary" loading=move || save.pending.get() on:click=save_threshold>
                            "Save"
                        </s-button>
                    </s-stack>
                </s-section>
            </s-page>
        }
    }
}
