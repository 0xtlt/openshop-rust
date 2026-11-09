//! The `/settings` page: under how many units a variant of the shop runs low.

use maud::html;
use openshop::prelude::*;
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

    /// Saves the threshold of the shop. A number out of range is refused by
    /// the input schema, under its field.
    #[action]
    async fn save_settings(ctx: AdminContext, input: SaveSettings) -> Result<Outcome> {
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
        Ok(Outcome::toast(format!(
            "A variant runs low under {} units",
            input.threshold
        )))
    }

    /// Rendered by the server with what `settings` returns, and the form the
    /// page refused, if it refused one.
    #[view]
    fn view(settings: Settings, form: FormState) -> Markup {
        let saved = settings.threshold.to_string();
        html! {
            s-page heading="Settings" {
                s-section heading="Low stock" {
                    form method="post" {
                        input type="hidden" name="_action" value=(Self::SAVE_SETTINGS);
                        s-stack gap="base" {
                            s-number-field
                                name="threshold"
                                label="A variant runs low under"
                                suffix="units"
                                min="1"
                                max="1000"
                                value=(form.value_or("threshold", &saved))
                                error=[form.error("threshold")] {}
                            s-button type="submit" variant="primary" { "Save" }
                        }
                    }
                }
            }
        }
    }
}
