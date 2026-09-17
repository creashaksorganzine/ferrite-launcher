//! Theme resolution and the saved-theme manager.
//!
//! Every color in the launcher resolves through [`Ferrite::theme`]. Widgets read tokens
//! from the returned [`ResolvedTheme`] rather than reaching into configuration, so
//! adding a palette source means editing [`ResolvedTheme::resolve`] and nothing else.
//!
//! Resolution is total. A malformed hex string or a deleted saved theme falls back to
//! the built-in constants rather than failing, because a theme problem must never be
//! able to make the window unreadable.

use super::settings::{color_to_hex, parse_hex_color};
use super::{ACCENT, BACKGROUND, CARD, Ferrite, MUTED, SIDEBAR};
use crate::config::{NamedTheme, ThemePalette, ThemePreset};
use eframe::egui::{self, Color32, RichText};

/// Every color token the UI is allowed to draw with, already parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResolvedTheme {
    pub background: Color32,
    pub sidebar: Color32,
    pub card: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub corner_radius: u8,
    pub light: bool,
}

impl ResolvedTheme {
    /// Ferrite's original dark colors, used whenever a configured value is unusable.
    fn builtin_dark() -> Self {
        Self {
            background: BACKGROUND,
            sidebar: SIDEBAR,
            card: CARD,
            accent: ACCENT,
            text: Color32::WHITE,
            muted: MUTED,
            corner_radius: 8,
            light: false,
        }
    }

    /// The original light colors.
    fn builtin_light() -> Self {
        Self {
            background: Color32::from_rgb(242, 244, 248),
            sidebar: Color32::from_rgb(226, 230, 237),
            card: Color32::WHITE,
            accent: ACCENT,
            text: Color32::BLACK,
            muted: MUTED,
            corner_radius: 8,
            light: true,
        }
    }

    /// Builds a theme from a palette, substituting built-in tokens for bad colors.
    fn from_palette(palette: &ThemePalette, light: bool, corner_radius: u8) -> Self {
        let base = if light {
            Self::builtin_light()
        } else {
            Self::builtin_dark()
        };
        Self {
            background: parse_hex_color(&palette.background).unwrap_or(base.background),
            sidebar: parse_hex_color(&palette.sidebar).unwrap_or(base.sidebar),
            card: parse_hex_color(&palette.card).unwrap_or(base.card),
            accent: parse_hex_color(&palette.accent).unwrap_or(base.accent),
            text: parse_hex_color(&palette.text).unwrap_or(base.text),
            muted: parse_hex_color(&palette.muted).unwrap_or(base.muted),
            corner_radius,
            light,
        }
    }

    /// Whether a background color is bright enough to need dark text.
    fn is_bright(color: Color32) -> bool {
        u32::from(color.r()) * 299 + u32::from(color.g()) * 587 + u32::from(color.b()) * 114
            > 128_000
    }
}

impl Ferrite {
    /// Resolves the active palette for this frame.
    ///
    /// Called several times per frame by the color accessors below; it allocates
    /// nothing and only parses six short strings, so it is not worth caching.
    pub(super) fn theme(&self) -> ResolvedTheme {
        let appearance = &self.config.appearance;
        let corner_radius = appearance.corner_radius;
        let legacy_accent = parse_hex_color(&appearance.accent);

        match appearance.theme_config.preset {
            ThemePreset::Legacy => {
                let mut theme = if appearance.theme == "light" {
                    ResolvedTheme::builtin_light()
                } else {
                    ResolvedTheme::builtin_dark()
                };
                theme.accent = legacy_accent.unwrap_or(theme.accent);
                theme.corner_radius = corner_radius;
                theme
            }
            ThemePreset::Dark => ResolvedTheme {
                accent: legacy_accent.unwrap_or(ACCENT),
                corner_radius,
                ..ResolvedTheme::builtin_dark()
            },
            ThemePreset::Light => ResolvedTheme {
                accent: legacy_accent.unwrap_or(ACCENT),
                corner_radius,
                ..ResolvedTheme::builtin_light()
            },
            ThemePreset::Custom => {
                let palette = &appearance.theme_config.custom_palette;
                // Brightness is derived rather than configured so a user cannot pick a
                // white background and end up with white text.
                let light = parse_hex_color(&palette.background)
                    .map(ResolvedTheme::is_bright)
                    .unwrap_or(false);
                ResolvedTheme::from_palette(palette, light, corner_radius)
            }
            ThemePreset::Saved => match appearance.theme_config.active() {
                Some(theme) => {
                    ResolvedTheme::from_palette(&theme.palette, theme.light, theme.corner_radius)
                }
                None => ResolvedTheme::builtin_dark(),
            },
        }
    }

    pub(super) fn is_light_theme(&self) -> bool {
        self.theme().light
    }

    pub(super) fn accent_color(&self) -> Color32 {
        self.theme().accent
    }

    pub(super) fn background_color(&self) -> Color32 {
        self.theme().background
    }

    pub(super) fn sidebar_color(&self) -> Color32 {
        self.theme().sidebar
    }

    pub(super) fn card_color(&self) -> Color32 {
        self.theme().card
    }

    pub(super) fn text_color(&self) -> Color32 {
        self.theme().text
    }

    pub(super) fn muted_color(&self) -> Color32 {
        self.theme().muted
    }

    /// Corner radius for frames drawn by layout widgets.
    pub(super) fn corner_radius(&self) -> u8 {
        self.theme().corner_radius
    }

    /// Draws the saved-theme manager shown under the Appearance settings tab.
    ///
    /// Edits are applied to a detached clone and committed after egui releases its
    /// borrows, matching the pattern the rest of the settings UI uses.
    pub(super) fn theme_manager_ui(&mut self, ui: &mut egui::Ui) {
        let mut config = self.config.appearance.theme_config.clone();
        let mut changed = false;
        let accent = self.accent_color();
        let muted = self.muted_color();

        ui.horizontal(|ui| {
            ui.label("Theme source");
            egui::ComboBox::from_id_salt("theme_preset")
                .selected_text(config.preset.label())
                .show_ui(ui, |ui| {
                    for preset in ThemePreset::ALL {
                        changed |= ui
                            .selectable_value(&mut config.preset, *preset, preset.label())
                            .changed();
                    }
                });
        });

        if config.preset == ThemePreset::Saved && config.themes.is_empty() {
            ui.label(
                RichText::new("No saved themes yet — save the custom palette below first.")
                    .color(accent),
            );
        }

        ui.add_space(8.0);
        ui.label(RichText::new("SAVED THEMES").small().strong().color(muted));

        let mut apply = None;
        let mut duplicate = None;
        let mut overwrite = None;
        let mut remove = None;
        for (index, theme) in config.themes.iter().enumerate() {
            let is_active =
                config.preset == ThemePreset::Saved && config.active_theme == theme.name;
            ui.push_id(index, |ui| {
                ui.horizontal(|ui| {
                    swatch_row(ui, &theme.palette);
                    ui.label(if is_active {
                        RichText::new(&theme.name).strong().color(accent)
                    } else {
                        RichText::new(&theme.name)
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // The first entry is the shipped default and stays removable
                        // only by editing the TOML, so "reset to default" always works.
                        if index > 0 && ui.small_button("Delete").clicked() {
                            remove = Some(index);
                        }
                        if ui.small_button("Duplicate").clicked() {
                            duplicate = Some(index);
                        }
                        if index > 0
                            && ui
                                .small_button("Overwrite")
                                .on_hover_text("Replace this theme with the current custom palette")
                                .clicked()
                        {
                            overwrite = Some(index);
                        }
                        if !is_active && ui.small_button("Use").clicked() {
                            apply = Some(index);
                        }
                    });
                });
            });
        }

        if let Some(index) = apply {
            config.active_theme = config.themes[index].name.clone();
            config.preset = ThemePreset::Saved;
            // Load the palette into the scratch buffer so editing continues from it.
            config.custom_palette = config.themes[index].palette.clone();
            changed = true;
        }
        if let Some(index) = duplicate {
            let source = config.themes[index].clone();
            let name = config.unique_theme_name(&source.name);
            config.themes.push(NamedTheme { name, ..source });
            changed = true;
        }
        if let Some(index) = overwrite {
            config.themes[index].palette = config.custom_palette.clone();
            changed = true;
        }
        if let Some(index) = remove {
            let removed = config.themes.remove(index);
            if config.active_theme == removed.name {
                config.active_theme = config
                    .themes
                    .first()
                    .map(|theme| theme.name.clone())
                    .unwrap_or_default();
            }
            changed = true;
        }

        ui.add_space(6.0);
        let name_id = ui.make_persistent_id("theme_save_name");
        let mut name = ui
            .data_mut(|data| data.get_temp::<String>(name_id))
            .unwrap_or_else(|| "My theme".to_owned());
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut name)
                    .desired_width(180.0)
                    .hint_text("Theme name"),
            );
            if ui
                .button("Save custom palette as theme")
                .on_hover_text("Stores the palette edited above under this name")
                .clicked()
            {
                let unique = config.unique_theme_name(&name);
                let light = parse_hex_color(&config.custom_palette.background)
                    .map(ResolvedTheme::is_bright)
                    .unwrap_or(false);
                config.themes.push(NamedTheme {
                    name: unique.clone(),
                    palette: config.custom_palette.clone(),
                    light,
                    corner_radius: self.config.appearance.corner_radius,
                });
                config.active_theme = unique;
                config.preset = ThemePreset::Saved;
                changed = true;
            }
        });
        ui.data_mut(|data| data.insert_temp(name_id, name));

        if changed {
            self.config.appearance.theme_config = config;
            self.save_config_change("Saved theme settings.");
        }
    }
}

/// Draws six small color chips previewing a palette.
fn swatch_row(ui: &mut egui::Ui, palette: &ThemePalette) {
    let size = egui::vec2(12.0, 12.0);
    for value in [
        &palette.background,
        &palette.sidebar,
        &palette.card,
        &palette.accent,
        &palette.text,
        &palette.muted,
    ] {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let color = parse_hex_color(value).unwrap_or(Color32::TRANSPARENT);
        ui.painter().rect_filled(rect, 2.0, color);
    }
    ui.add_space(6.0);
}

/// Re-exported so the appearance tab can keep rendering hex fields unchanged.
pub(super) fn palette_hex(color: Color32) -> String {
    color_to_hex(color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn unusable_colors_fall_back_to_builtin_tokens() {
        let palette = ThemePalette {
            background: "not a color".to_owned(),
            ..ThemePalette::default()
        };
        let theme = ResolvedTheme::from_palette(&palette, false, 8);
        assert_eq!(theme.background, BACKGROUND);
        assert_eq!(theme.card, parse_hex_color("#1F232A").unwrap());
    }

    #[test]
    fn a_deleted_saved_theme_does_not_break_rendering() {
        let mut config = Config::default();
        config.appearance.theme_config.preset = ThemePreset::Saved;
        config.appearance.theme_config.active_theme = "gone".to_owned();
        config.appearance.theme_config.themes.clear();
        assert!(config.appearance.theme_config.active().is_none());
    }

    #[test]
    fn bright_custom_backgrounds_switch_to_dark_text() {
        assert!(ResolvedTheme::is_bright(Color32::WHITE));
        assert!(!ResolvedTheme::is_bright(BACKGROUND));
    }
}
