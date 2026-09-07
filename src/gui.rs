use eframe::egui::{self, Color32, RichText};

pub fn gui() -> eframe::Result {
    eframe::run_native(
        "My App",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(MyApp::default()))),
    )
}

struct MyApp {
    running_text: String,
    settings_open: bool,
    current_settings_tab: String,
    is_global_checked: bool,
    is_launcher_checked: bool,
    is_appearance_checked: bool,
}

impl Default for MyApp {
    fn default() -> Self {
        Self {
            running_text: String::from("Game not running."),
            settings_open: false,
            current_settings_tab: String::from("Global"),
            is_global_checked: false,
            is_launcher_checked: false,
            is_appearance_checked: false,
        }
    }
}

impl eframe::App for MyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().global_style_mut(|style| {
            style.visuals.override_text_color = Some(egui::Color32::RED);
        });

        ui.painter().rect_filled(ui.max_rect(), 0.0, Color32::BLACK);

        ui.vertical_centered(|ui| {
            ui.heading(
                RichText::new("Ferrite Launcher")
                    .size(100.0)
                    .color(Color32::RED)
                    .strong(),
            );
            ui.label("Minecraft Launcher written in Rust");

            ui.add_space(8.0);

            centered_row(ui, "frame_row", |ui| {
                egui::Frame::dark_canvas(ui.style())
                    .fill(egui::Color32::RED)
                    .inner_margin(20.0)
                    .stroke(egui::Stroke::new(2.0, egui::Color32::GRAY))
                    .corner_radius(20.0)
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                if ui.button("Play").clicked() {
                                    self.running_text = String::from("Game is running!");
                                }
                                if ui.button("Settings").clicked() {
                                    self.settings_open = true;
                                }
                            });

                            if ui.button("Kill game!").clicked() {
                                self.running_text = String::from("Game not running.");
                            }
                        });
                    });
            });

            ui.add_space(8.0);

            ui.label(&self.running_text);
            ui.label("Version: 0.1.0");
            if self.settings_open {
                egui::Window::new("Settings").show(ui.ctx(), |ui| {
                    ui.horizontal(|ui| {
                        // LEFT: navigation
                        ui.vertical(|ui| {
                            if ui
                                .selectable_label(
                                    self.current_settings_tab == "Global",
                                    RichText::new("> Global").size(30.0).color(Color32::RED),
                                )
                                .clicked()
                            {
                                self.current_settings_tab = String::from("Global");
                            }

                            ui.add_space(20.0);

                            if ui
                                .selectable_label(
                                    self.current_settings_tab == "Launcher",
                                    RichText::new("> Launcher").size(30.0).color(Color32::WHITE),
                                )
                                .clicked()
                            {
                                self.current_settings_tab = String::from("Launcher");
                            }

                            ui.add_space(20.0);

                            if ui
                                .selectable_label(
                                    self.current_settings_tab == "Appearance",
                                    RichText::new("> Appearance")
                                        .size(30.0)
                                        .color(Color32::YELLOW),
                                )
                                .clicked()
                            {
                                self.current_settings_tab = String::from("Appearance");
                            }

                            ui.add_space(200.0);

                            if ui.button("Close").clicked() {
                                self.settings_open = false;
                            }
                        });

                        // RIGHT: settings
                        ui.separator();

                        ui.vertical(|ui| {
                            if self.current_settings_tab == "Global" {
                                ui.heading("Global Settings");

                                ui.checkbox(&mut self.is_global_checked, "Global setting");

                                if self.is_global_checked {
                                    ui.label("fuck");
                                }
                            } else if self.current_settings_tab == "Launcher" {
                                ui.heading("Launcher Settings");

                                ui.checkbox(&mut self.is_launcher_checked, "Launcher setting");

                                if self.is_launcher_checked {
                                    ui.label("fuck");
                                }
                            } else if self.current_settings_tab == "Appearance" {
                                ui.heading("Appearance");

                                ui.checkbox(&mut self.is_appearance_checked, "Appearance setting");

                                if self.is_appearance_checked {
                                    ui.label("fuck");
                                }
                            }
                        });
                    });
                });
            }
        });
    }
}

fn centered_row(ui: &mut egui::Ui, salt: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    let id = ui.id().with(salt);
    let last_width: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.0);
    // LEFT: navigation

    ui.horizontal(|ui| {
        let pad = (ui.available_width() - last_width) * 0.5;
        if pad > 0.0 {
            ui.add_space(pad);
        }

        let width = ui.scope(add_contents).response.rect.width();
        ui.ctx().data_mut(|d| d.insert_temp(id, width));
    });
}
