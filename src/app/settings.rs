//! Settings UI, live appearance helpers, update checks, and Discord synchronization.
//!
//! Normal controls mutate typed [`Config`] and save immediately; the advanced editor
//! deliberately keeps raw TOML separate until explicit apply. Network update checks use
//! a worker channel, while visual settings are read from central state every frame.

use super::{ACCENT, BACKGROUND, CARD, Ferrite, MUTED, SIDEBAR, page_heading};
use crate::config::Config;
use crate::discord::DiscordPresence;
use crate::updates::UpdateCheck;
use eframe::egui::{self, Color32, RichText};
use std::sync::mpsc::{self, TryRecvError};

impl Ferrite {
    /// Starts a single blocking GitHub release check and records whether it was manual.
    pub(super) fn start_update_check(&mut self, manual: bool) {
        if self.update_task.is_some() {
            if manual {
                self.update_status = Some("An update check is already running.".into());
            }
            return;
        }
        self.update_status = Some("Checking for updates...".into());
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("github-update-check".to_owned())
            .spawn(move || {
                let result = crate::updates::check_for_update().map_err(|error| error.to_string());
                let _ = sender.send((manual, result));
            }) {
            Ok(_) => self.update_task = Some(receiver),
            Err(error) => {
                self.update_status = Some(format!("Could not start update check: {error}"));
            }
        }
    }

    /// Non-blockingly applies an update worker's terminal result to banner/status state.
    pub(super) fn poll_update_check(&mut self) {
        let Some(receiver) = &self.update_task else {
            return;
        };
        match receiver.try_recv() {
            Ok((_manual, Ok(UpdateCheck::Available(info)))) => {
                self.update_status = Some(format!(
                    "Update available: {} → {}",
                    info.current_version, info.latest_version
                ));
                self.update_info = Some(info);
                self.update_dismissed = false;
                self.update_task = None;
            }
            Ok((
                _manual,
                Ok(UpdateCheck::UpToDate {
                    current_version,
                    latest_version,
                }),
            )) => {
                self.update_status = Some(format!(
                    "Ferrite is up to date ({current_version}; latest release {latest_version})."
                ));
                self.update_info = None;
                self.update_task = None;
            }
            Ok((_manual, Err(error))) => {
                self.update_status = Some(format!("Could not check for updates: {error}"));
                self.update_task = None;
            }
            Err(TryRecvError::Disconnected) => {
                self.update_status = Some("Could not check for updates: worker stopped.".into());
                self.update_task = None;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Draws a session-dismissible release banner without changing update preferences.
    pub(super) fn update_banner(&mut self, ui: &mut egui::Ui) {
        if self.update_dismissed {
            return;
        }
        let Some(info) = self.update_info.as_ref() else {
            return;
        };
        let current = info.current_version.to_string();
        let latest = info.latest_version.to_string();
        let release_url = info.release_url.clone();
        let release_name = info.release_name.clone();
        egui::Frame::new()
            .fill(self.card_color())
            .stroke(egui::Stroke::new(1.0, self.accent_color()))
            .corner_radius(self.config.appearance.corner_radius)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("Update Available")
                            .strong()
                            .color(self.accent_color()),
                    );
                    ui.label(format!("Current {current} · Latest {latest}"));
                    if let Some(name) = release_name {
                        ui.label(format!("· {name}"));
                    }
                    if ui.button("Open Release").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(release_url));
                    }
                    if ui.button("Dismiss").clicked() {
                        self.update_dismissed = true;
                    }
                });
            });
        ui.add_space(8.0);
    }

    /// Reconciles the optional IPC owner with the persisted Rich Presence setting.
    pub(super) fn sync_discord_presence(&mut self) {
        if self.config.discord.rich_presence {
            if self.discord.is_none() {
                self.discord = DiscordPresence::new();
            }
        } else if let Some(mut presence) = self.discord.take() {
            presence.clear();
        }
    }

    /// Connects or disconnects Rich Presence immediately when its setting changes.
    pub(super) fn set_discord_enabled(&mut self, enabled: bool) {
        self.config.discord.rich_presence = enabled;
        self.sync_discord_presence();
        let message = if enabled && self.discord.is_some() {
            "Discord Rich Presence enabled."
        } else if enabled {
            "Discord Rich Presence is enabled, but Discord is unavailable."
        } else {
            "Discord Rich Presence disabled."
        };
        self.save_config_change(message);
    }

    /// Persists typed settings and refreshes the advanced editor only after success.
    pub(super) fn save_config_change(&mut self, success_message: &str) {
        match crate::config::save(&self.config) {
            Ok(()) => {
                self.running_text = success_message.to_owned();
                self.config_status = Some(success_message.to_owned());
                self.raw_config_toml = crate::config::to_toml(&self.config).unwrap_or_default();
            }
            Err(error) => {
                let message = format!("Failed to save configuration: {error}");
                self.running_text = message.clone();
                self.config_status = Some(message);
            }
        }
    }

    /// Replaces all live settings after a validated load/apply and reconciles dependents.
    pub(super) fn replace_config(&mut self, config: Config, raw_toml: String, message: &str) {
        self.config = config;
        self.accent_edit = self.config.appearance.accent.clone();
        self.raw_config_toml = raw_toml;
        self.sync_discord_presence();
        self.running_text = message.to_owned();
        self.config_status = Some(message.to_owned());
    }

    /// Reloads typed and raw representations together, leaving current state on failure.
    pub(super) fn reload_config(&mut self) {
        match crate::config::load() {
            Ok(config) => {
                let raw = crate::config::read_toml()
                    .or_else(|_| crate::config::to_toml(&config))
                    .unwrap_or_default();
                self.replace_config(config, raw, "Reloaded configuration from disk.");
            }
            Err(error) => {
                self.config_status = Some(format!("Reload failed: {error}"));
            }
        }
    }

    /// Validates and saves the editor buffer before replacing live configuration.
    pub(super) fn apply_raw_config(&mut self) {
        match crate::config::save_toml(&self.raw_config_toml) {
            Ok(config) => {
                let raw = crate::config::to_toml(&config).unwrap_or_default();
                self.replace_config(config, raw, "Applied and saved configuration TOML.");
            }
            Err(error) => {
                self.config_status = Some(format!("TOML was not saved: {error}"));
            }
        }
    }

    pub(super) fn accent_color(&self) -> Color32 {
        parse_hex_color(&self.config.appearance.accent).unwrap_or(ACCENT)
    }

    pub(super) fn background_color(&self) -> Color32 {
        if self.config.appearance.theme == "light" {
            Color32::from_rgb(242, 244, 248)
        } else {
            BACKGROUND
        }
    }

    pub(super) fn sidebar_color(&self) -> Color32 {
        if self.config.appearance.theme == "light" {
            Color32::from_rgb(226, 230, 237)
        } else {
            SIDEBAR
        }
    }

    pub(super) fn card_color(&self) -> Color32 {
        if self.config.appearance.theme == "light" {
            Color32::WHITE
        } else {
            CARD
        }
    }

    /// Draws launcher settings as page content rather than a popup window.
    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui) {
        page_heading(ui, "Settings", "Configure Ferrite Launcher.");
        self.account_section(ui);
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            for tab in ["Global", "Launcher", "Appearance", "Advanced"] {
                if ui
                    .selectable_label(self.current_settings_tab == tab, tab)
                    .clicked()
                {
                    self.current_settings_tab = tab.to_owned();
                }
            }
        });
        ui.separator();
        ui.add_space(12.0);

        match self.current_settings_tab.as_str() {
            "Global" => {
                ui.heading("Global settings");
                ui.label("Minecraft memory");
                let changed = ui
                    .add(
                        egui::Slider::new(
                            &mut self.config.minecraft.default_memory_mb,
                            512..=32_768,
                        )
                        .step_by(256.0)
                        .suffix(" MB"),
                    )
                    .changed();
                ui.label("Applied to the next Minecraft launch.");
                ui.separator();
                ui.checkbox(&mut self.is_global_checked, "Enable global defaults");
                if changed {
                    self.save_config_change("Saved default Minecraft memory.");
                }
            }
            "Launcher" => {
                ui.heading("Launcher settings");
                let mut changed = false;
                let mut keep_open = !self.config.launcher.close_on_launch;
                if ui
                    .checkbox(&mut keep_open, "Keep launcher open while playing")
                    .changed()
                {
                    self.config.launcher.close_on_launch = !keep_open;
                    changed = true;
                }
                changed |= ui
                    .checkbox(
                        &mut self.config.launcher.show_snapshots,
                        "Show Minecraft snapshots",
                    )
                    .changed();
                ui.label("Snapshot visibility is refreshed when Ferrite restarts.");
                changed |= ui
                    .checkbox(
                        &mut self.config.launcher.check_for_updates,
                        "Check for launcher updates automatically",
                    )
                    .changed();
                if changed {
                    self.save_config_change("Saved launcher settings.");
                }
                if ui
                    .add_enabled(
                        self.update_task.is_none(),
                        egui::Button::new(if self.update_task.is_some() {
                            "Checking..."
                        } else {
                            "Check for Updates"
                        }),
                    )
                    .clicked()
                {
                    self.start_update_check(true);
                }
                if let Some(status) = &self.update_status {
                    ui.label(status);
                }

                ui.separator();
                let mut discord_enabled = self.config.discord.rich_presence;
                if ui
                    .checkbox(&mut discord_enabled, "Enable Discord Rich Presence")
                    .changed()
                {
                    self.set_discord_enabled(discord_enabled);
                }
                if self.config.discord.rich_presence && self.discord.is_none() {
                    ui.label(
                        RichText::new(
                            "Enabled, but Discord is not currently available. Toggle off and on to retry.",
                        )
                        .color(MUTED),
                    );
                }
            }
            "Appearance" => {
                ui.heading("Appearance");
                let mut changed = false;
                let previous_theme = self.config.appearance.theme.clone();
                egui::ComboBox::from_label("Theme")
                    .selected_text(capitalize(&self.config.appearance.theme))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.config.appearance.theme,
                            "dark".to_owned(),
                            "Dark",
                        );
                        ui.selectable_value(
                            &mut self.config.appearance.theme,
                            "light".to_owned(),
                            "Light",
                        );
                    });
                changed |= previous_theme != self.config.appearance.theme;
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.config.appearance.font_scale, 0.5..=2.0)
                            .step_by(0.05)
                            .text("Font scale"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.config.appearance.corner_radius, 0..=32)
                            .text("Corner radius"),
                    )
                    .changed();

                ui.label("Accent color");
                ui.horizontal(|ui| {
                    let mut color = self.accent_color();
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        self.config.appearance.accent = color_to_hex(color);
                        self.accent_edit = self.config.appearance.accent.clone();
                        changed = true;
                    }
                    ui.text_edit_singleline(&mut self.accent_edit);
                    if ui.button("Apply color").clicked() {
                        if let Some(color) = parse_hex_color(self.accent_edit.trim()) {
                            self.config.appearance.accent = color_to_hex(color);
                            self.accent_edit = self.config.appearance.accent.clone();
                            changed = true;
                        } else {
                            self.config_status = Some("Accent must use #RRGGBB format.".to_owned());
                        }
                    }
                });
                ui.separator();
                ui.checkbox(
                    &mut self.is_appearance_checked,
                    "Use compact instance cards",
                );
                if changed {
                    self.save_config_change("Saved appearance settings.");
                }
            }
            "Advanced" => {
                ui.heading("Advanced configuration");
                ui.horizontal(|ui| {
                    if ui.button("Open Config Folder").clicked() {
                        self.config_status = Some(match crate::config::open_config_folder() {
                            Ok(()) => "Opened the config folder.".to_owned(),
                            Err(error) => format!("Could not open config folder: {error}"),
                        });
                    }
                    if ui.button("Reload Config").clicked() {
                        self.reload_config();
                    }
                });
                if let Ok(path) = crate::config::config_path() {
                    ui.label(
                        RichText::new(path.display().to_string())
                            .small()
                            .color(MUTED),
                    );
                }
                ui.label("Raw TOML is only parsed and saved when Apply / Save is pressed.");
                ui.add(
                    egui::TextEdit::multiline(&mut self.raw_config_toml)
                        .code_editor()
                        .desired_rows(18)
                        .desired_width(f32::INFINITY),
                );
                if ui.button("Apply / Save").clicked() {
                    self.apply_raw_config();
                }
            }
            _ => {}
        }

        if let Some(status) = &self.config_status {
            ui.add_space(8.0);
            ui.label(status);
        }
    }
}

/// Parses strict `#RRGGBB` input; invalid text leaves the active accent unchanged.
pub(super) fn parse_hex_color(value: &str) -> Option<Color32> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color32::from_rgb(red, green, blue))
}

/// Serializes an egui color to the configuration's lowercase `#rrggbb` form.
pub(super) fn color_to_hex(color: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

pub(super) fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}
