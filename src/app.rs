//! The egui application for Ferrite Launcher.
//!
//! This module owns the launcher UI state and translates button clicks into
//! calls to the Minecraft, mod-loader, and persistent instance backends.

use crate::auth::Account;
use crate::icons::IconCache;
use crate::instance_mods::InstalledMod;
use crate::instances::InstanceProfile;
use crate::loaders::ModLoader;
use crate::modrinth::{ProjectDetails, SearchFilters, SearchResponse};
use eframe::egui::{self, Color32, RichText};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

const BACKGROUND: Color32 = Color32::from_rgb(18, 20, 24);
const SIDEBAR: Color32 = Color32::from_rgb(25, 28, 34);
const CARD: Color32 = Color32::from_rgb(31, 35, 42);
const ACCENT: Color32 = Color32::from_rgb(220, 55, 65);
const MUTED: Color32 = Color32::from_rgb(150, 155, 165);

/// Starts Ferrite Launcher in eframe's native window.
pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1_000.0, 700.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Ferrite Launcher",
        options,
        Box::new(|_cc| Ok(Box::new(Ferrite::default()))),
    )
}

/// A top-level destination in the launcher's sidebar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Play,
    Instances,
    Mods,
    Settings,
}

/// A result sent back to egui by a blocking Modrinth worker thread.
enum ModTaskResult {
    Search(SearchFilters, Result<SearchResponse, String>),
    Details(bool, Result<ProjectDetails, String>),
    Local {
        target: InstanceProfile,
        message: String,
        result: Result<Vec<InstalledMod>, String>,
    },
}

/// Actual operations performed during creation, without estimated percentages.
#[derive(Clone, Copy)]
enum InstanceCreationStage {
    Preparing,
    DownloadingMinecraft,
    InstallingLoader,
    Finalizing,
}

impl InstanceCreationStage {
    /// Returns the same progress label for the global footer and creation dialog.
    fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Preparing instance...",
            Self::DownloadingMinecraft => "Downloading Minecraft (reusing cached files)...",
            Self::InstallingLoader => "Installing mod loader...",
            Self::Finalizing => "Finalizing instance...",
        }
    }
}

/// Ordered progress and terminal results from the instance-creation worker.
enum InstanceCreationEvent {
    Stage(InstanceCreationStage),
    DownloadProgress(String),
    Finished(Result<InstanceProfile, String>),
}

/// Messages from one sign-in attempt; only public device instructions reach the UI.
enum AuthEvent {
    Progress(String),
    Device {
        user_code: String,
        verification_uri: String,
    },
    Account(Result<Account, String>),
}

/// One cancellable worker. Dropping its receiver discards even queued credentials.
struct AuthTask {
    events: Receiver<AuthEvent>,
    cancel: Arc<AtomicBool>,
}

impl Drop for AuthTask {
    /// Signals cancellation without blocking the UI on an in-flight HTTP request.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Memory-only account and sign-in state; never serialized or logged.
#[derive(Default)]
struct AccountSession {
    account: Option<Account>,
    client_id: String,
    open: bool,
    task: Option<AuthTask>,
    device: Option<(String, String)>,
    status: String,
}

/// All state required to render and interact with the Ferrite Launcher UI.
struct Ferrite {
    /// Session credentials and a worker cancelled automatically when the app drops.
    auth: AccountSession,
    /// A short result/status message displayed at the bottom of the window.
    running_text: String,
    /// The page currently selected in the sidebar.
    current_page: Page,
    /// The label of the active settings subsection.
    current_settings_tab: String,
    /// Temporary state for the placeholder Global settings control.
    is_global_checked: bool,
    /// Temporary state for the placeholder Launcher settings control.
    is_launcher_checked: bool,
    /// Temporary state for the placeholder Appearance settings control.
    is_appearance_checked: bool,
    /// Whether the create-instance dialog should be drawn this frame.
    create_instance_open: bool,
    /// The name being entered in the create-instance dialog.
    instance_name: String,
    /// Receives creation progress/results; its presence prevents another creation.
    instance_creation_task: Option<Receiver<InstanceCreationEvent>>,
    /// Creation progress or failure retained independently of other UI messages.
    instance_creation_status: Option<String>,
    /// The Minecraft version selected for the instance being created.
    selected_version: String,
    /// The mod-loader label selected for the instance being created.
    selected_loader: String,
    /// Release versions fetched from Mojang.
    versions: Vec<String>,
    /// Profiles loaded from and saved to the persistent instance store.
    instances: Vec<InstanceProfile>,
    /// Index into [`Self::instances`] for the active profile.
    selected_instance: Option<usize>,
    /// Explicit mod target, independent of Play selection and creation completion.
    mod_target: Option<InstanceProfile>,
    /// Editable filters and the immutable filters associated with displayed results.
    mod_filters: SearchFilters,
    result_filters: SearchFilters,
    mod_categories: String,
    mod_results: Option<SearchResponse>,
    mod_details: Option<(bool, ProjectDetails)>,
    installed_mods: Option<Vec<InstalledMod>>,
    show_installed: bool,
    /// Confirmation captures both the profile and exact filename, never an index.
    pending_uninstall: Option<(InstanceProfile, String)>,
    /// Receives the active search or installation result without blocking egui.
    mod_task: Option<Receiver<ModTaskResult>>,
    /// Bounded asynchronous icon decoding shared by browser and installed mods.
    icons: IconCache,
}

impl Default for Ferrite {
    fn default() -> Self {
        let versions = match crate::minecraft::get_versions() {
            Ok(versions) => versions,
            Err(error) => {
                eprintln!("Failed to fetch Minecraft versions: {error}");
                Vec::new()
            }
        };

        let (instances, running_text) = match crate::instances::load() {
            Ok(instances) => (instances, String::from("Game not running.")),
            Err(error) => (
                Vec::new(),
                format!("Failed to load saved instances: {error}"),
            ),
        };
        let selected_instance = (!instances.is_empty()).then_some(0);

        Self {
            auth: AccountSession {
                client_id: std::env::var("FERRITE_MICROSOFT_CLIENT_ID").unwrap_or_default(),
                ..Default::default()
            },
            icons: IconCache::default(),
            running_text,
            current_page: Page::Play,
            current_settings_tab: String::from("Global"),
            is_global_checked: false,
            is_launcher_checked: false,
            is_appearance_checked: false,
            create_instance_open: false,
            instance_name: String::new(),
            instance_creation_task: None,
            instance_creation_status: None,
            selected_version: versions
                .first()
                .cloned()
                .unwrap_or_else(|| String::from("26.2")),
            selected_loader: String::from("Vanilla"),
            versions,
            instances,
            selected_instance,
            mod_target: None,
            mod_filters: SearchFilters::default(),
            result_filters: SearchFilters::default(),
            mod_categories: String::new(),
            mod_results: None,
            mod_details: None,
            installed_mods: None,
            show_installed: false,
            pending_uninstall: None,
            mod_task: None,
        }
    }
}

impl Ferrite {
    /// Starts blocking Microsoft authentication off the UI thread with a fresh channel.
    fn start_sign_in(&mut self) {
        if self.auth.task.is_some() {
            return;
        }
        let client_id = self.auth.client_id.trim().to_owned();
        if client_id.is_empty() {
            self.auth.status = "Enter your Microsoft public-client application ID first.".into();
            return;
        }
        self.auth.device = None;
        self.auth.status = "Starting Microsoft sign-in...".into();
        let (sender, events) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.auth.task = Some(AuthTask { events, cancel });
        std::thread::spawn(move || {
            let result = crate::auth::start_login(&client_id).and_then(|code| {
                if worker_cancel.load(Ordering::Relaxed) {
                    return Err("Sign-in cancelled.".into());
                }
                sender
                    .send(AuthEvent::Device {
                        user_code: code.user_code.clone(),
                        verification_uri: code.verification_uri.clone(),
                    })
                    .map_err(|_| "Sign-in cancelled.".to_owned())?;
                crate::auth::complete_login(&client_id, code, &worker_cancel, |message| {
                    let _ = sender.send(AuthEvent::Progress(message.into()));
                })
            });
            if !worker_cancel.load(Ordering::Relaxed) {
                let _ = sender.send(AuthEvent::Account(result));
            }
        });
    }

    /// Invalidates the attempt, including queued results, without clearing an existing session.
    fn cancel_sign_in(&mut self) {
        self.auth.task = None;
        self.auth.device = None;
        self.auth.status = "Sign-in cancelled.".into();
    }

    /// Forgets session credentials and prevents late worker results from signing back in.
    fn sign_out(&mut self) {
        self.cancel_sign_in();
        self.auth.account = None;
        self.auth.status = "Signed out.".into();
        self.running_text = self.auth.status.clone();
    }

    /// Drains auth events on every page, never blocking egui or persisting credentials.
    fn poll_auth(&mut self) {
        loop {
            let Some(task) = &self.auth.task else { break };
            let event = match task.events.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.auth.task = None;
                    self.auth.device = None;
                    self.auth.status = "Sign-in worker stopped unexpectedly. Try again.".into();
                    self.running_text = self.auth.status.clone();
                    break;
                }
            };
            match event {
                AuthEvent::Progress(message) => self.auth.status = message,
                AuthEvent::Device {
                    user_code,
                    verification_uri,
                } => {
                    self.auth.device = Some((user_code, verification_uri));
                    self.auth.status =
                        "Open the Microsoft link and enter the code to sign in.".into();
                }
                AuthEvent::Account(result) => {
                    self.auth.task = None;
                    self.auth.device = None;
                    match result {
                        Ok(account) if !account.is_expired() => {
                            self.auth.status =
                                format!("Signed in as {} (this session only).", account.name);
                            self.auth.account = Some(account);
                        }
                        Ok(_) => self.auth.status = "Session expired. Please sign in again.".into(),
                        Err(error) => self.auth.status = format!("Sign-in failed: {error}"),
                    }
                }
            }
            self.running_text = self.auth.status.clone();
        }
    }

    /// Shows account identity and expiry, with the same dialog available from Play and Settings.
    fn account_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Account");
        if let Some(account) = &self.auth.account {
            ui.label(format!("Signed in as {}", account.name));
            ui.label(if account.is_expired() {
                "Session expired — sign in again before playing."
            } else {
                "Session active. Sign in again when it expires."
            });
        } else {
            ui.label("Sign in with Microsoft to play Minecraft Java.");
        }
        ui.horizontal(|ui| {
            if ui
                .button(if self.auth.account.is_some() {
                    "Sign in again / Account"
                } else {
                    "Sign in / Account"
                })
                .clicked()
            {
                self.auth.open = true;
            }
            if self.auth.account.is_some() && ui.button("Sign out").clicked() {
                self.sign_out();
            }
        });
    }

    /// Displays public device instructions only; closing the dialog cancels pending login.
    fn account_window(&mut self, context: &egui::Context) {
        if !self.auth.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Microsoft account")
            .open(&mut open)
            .collapsible(false)
            .default_width(440.0)
            .show(context, |ui| {
                self.account_section(ui);
                ui.separator();
                ui.label("Credentials stay in memory for this launcher session only.");
                ui.label("Microsoft public-client application ID");
                ui.add_enabled(self.auth.task.is_none(), egui::TextEdit::singleline(&mut self.auth.client_id));
                ui.label("Uses FERRITE_MICROSOFT_CLIENT_ID when set. Supply your own application configured for consumer device-code sign-in and Minecraft API access.");
                if let Some((code, uri)) = &self.auth.device {
                    ui.hyperlink_to("Open Microsoft sign-in in your browser", uri);
                    ui.horizontal(|ui| {
                        ui.monospace(code);
                        if ui.button("Copy code").clicked() {
                            ui.ctx().copy_text(code.clone());
                        }
                    });
                }
                if self.auth.task.is_some() {
                    ui.spinner();
                    if ui.button("Cancel sign-in").clicked() { self.cancel_sign_in(); }
                } else if ui.button("Start Microsoft sign-in").clicked() {
                    self.start_sign_in();
                }
                ui.label(&self.auth.status);
            });
        self.auth.open = open;
        if !open && self.auth.task.is_some() {
            self.cancel_sign_in();
        }
    }

    /// Resolves the loader selected in the create-instance form.
    fn selected_loader(&self) -> Option<ModLoader> {
        ModLoader::from_label(&self.selected_loader)
    }

    /// Returns the active profile while safely handling a stale index.
    fn selected_instance(&self) -> Option<&InstanceProfile> {
        self.selected_instance
            .and_then(|index| self.instances.get(index))
    }

    /// Produces the active profile label used by combo boxes.
    fn selected_instance_label(&self) -> String {
        self.selected_instance()
            .map(|instance| instance.name.clone())
            .unwrap_or_else(|| "No instance selected".to_owned())
    }

    /// Validates and snapshots the form before starting one blocking worker.
    ///
    /// The worker installs files only; profile-list persistence remains on egui's
    /// thread. Inputs stay intact until installation and persistence both succeed.
    fn create_instance(&mut self) {
        if self.instance_creation_task.is_some() {
            return;
        }
        self.instance_creation_status = None;
        let name = self.instance_name.trim();
        if name.is_empty() {
            self.running_text = "An instance name is required.".to_owned();
            return;
        }
        if self.instances.iter().any(|instance| instance.name == name) {
            self.running_text = format!("An instance named '{name}' already exists.");
            return;
        }

        let Some(loader) = self.selected_loader() else {
            self.running_text = format!("Unknown mod loader: {}", self.selected_loader);
            return;
        };

        if self.selected_version.trim().is_empty()
            || (!self.versions.is_empty() && !self.versions.contains(&self.selected_version))
        {
            self.running_text =
                "Select a valid Minecraft version before creating an instance.".to_owned();
            return;
        }

        let profile = InstanceProfile::new(
            name.to_owned(),
            self.selected_version.clone(),
            self.selected_loader.clone(),
            &self.instances,
        );
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("instance-creation".to_owned())
            .spawn(move || {
                let result = (|| -> Result<InstanceProfile, String> {
                    let _ = sender.send(InstanceCreationEvent::Stage(
                        InstanceCreationStage::Preparing,
                    ));
                    crate::instances::create_game_dir(&profile)
                        .map_err(|error| format!("Failed to create instance directory: {error}"))?;
                    let _ = sender.send(InstanceCreationEvent::Stage(
                        InstanceCreationStage::DownloadingMinecraft,
                    ));
                    crate::minecraft::install_version_with_progress(&profile.version, |message| {
                        let _ =
                            sender.send(InstanceCreationEvent::DownloadProgress(message.into()));
                    })
                    .map_err(|error| format!("Failed to download Minecraft: {error}"))?;
                    if loader != ModLoader::Vanilla {
                        let _ = sender.send(InstanceCreationEvent::Stage(
                            InstanceCreationStage::InstallingLoader,
                        ));
                        // Loader backends repeat the vanilla install, reusing cached downloads.
                        crate::loaders::install(&profile.version, loader).map_err(|error| {
                            format!("Failed to install {}: {error}", loader.label())
                        })?;
                    }
                    let _ = sender.send(InstanceCreationEvent::Stage(
                        InstanceCreationStage::Finalizing,
                    ));
                    Ok(profile)
                })();
                let _ = sender.send(InstanceCreationEvent::Finished(result));
            });
        match worker {
            Ok(_) => {
                self.instance_creation_task = Some(receiver);
                self.instance_creation_status =
                    Some(InstanceCreationStage::Preparing.label().to_owned());
            }
            Err(error) => {
                self.instance_creation_failed(format!("Failed to start instance worker: {error}"))
            }
        }
    }

    /// Drains worker events globally, committing the profile to the current list
    /// only on success. A failed save rolls back the in-memory insertion.
    fn poll_instance_creation(&mut self) {
        loop {
            let Some(receiver) = &self.instance_creation_task else {
                return;
            };
            match receiver.try_recv() {
                Ok(InstanceCreationEvent::Stage(stage)) => {
                    self.instance_creation_status = Some(stage.label().to_owned());
                }
                Ok(InstanceCreationEvent::DownloadProgress(message)) => {
                    self.instance_creation_status = Some(message);
                }
                Ok(InstanceCreationEvent::Finished(result)) => {
                    self.instance_creation_task = None;
                    match result {
                        Ok(profile) => {
                            let name = profile.name.clone();
                            self.instances.push(profile);
                            if let Err(error) = crate::instances::save(&self.instances) {
                                self.instances.pop();
                                self.instance_creation_failed(format!(
                                    "Failed to save instance: {error}"
                                ));
                                return;
                            }
                            self.selected_instance = Some(self.instances.len() - 1);
                            self.running_text = format!("Created instance '{name}'.");
                            self.instance_creation_status = None;
                            self.instance_name.clear();
                            self.create_instance_open = false;
                        }
                        Err(error) => self.instance_creation_failed(error),
                    }
                    return;
                }
                Err(TryRecvError::Disconnected) => {
                    self.instance_creation_failed(
                        "The instance creation worker stopped unexpectedly.".to_owned(),
                    );
                    return;
                }
                Err(TryRecvError::Empty) => return,
            }
        }
    }

    /// Reports failure without discarding form inputs, including after dialog closure.
    fn instance_creation_failed(&mut self, message: String) {
        self.instance_creation_task = None;
        self.running_text = message.clone();
        self.instance_creation_status = Some(message);
        self.create_instance_open = true;
    }

    /// Launches the active profile in its isolated game directory.
    fn launch_selected(&mut self) {
        let auth_error = match self.auth.account.as_ref() {
            None => Some("Sign in with Microsoft before launching."),
            Some(account) if account.is_expired() => {
                Some("Session expired. Please sign in again before launching.")
            }
            Some(_) => None,
        };
        if let Some(message) = auth_error {
            self.running_text = message.into();
            self.auth.status = message.into();
            self.auth.open = true;
            return;
        }
        if self.mod_task.is_some() || self.pending_uninstall.is_some() {
            self.running_text = "Wait for mod management to finish before launching.".into();
            return;
        }
        let Some(instance) = self.selected_instance() else {
            self.running_text = "Select an instance before launching.".to_owned();
            return;
        };
        let name = instance.name.clone();
        let version = instance.version.clone();
        let loader_name = instance.loader.clone();
        let game_dir = instance.game_dir();

        let Some(loader) = ModLoader::from_label(&loader_name) else {
            self.running_text = format!("Unknown mod loader: {loader_name}");
            return;
        };

        self.running_text = match crate::loaders::launch_authenticated(
            &version,
            loader,
            &game_dir,
            self.auth.account.as_ref().expect("account checked above"),
        ) {
            Ok(()) => format!("Launched '{name}'."),
            Err(error) => format!("Failed to launch '{name}': {error}"),
        };
    }

    /// Removes the active profile metadata and its isolated game files.
    fn remove_selected(&mut self) {
        if self.mod_task.is_some()
            || self.pending_uninstall.is_some()
            || crate::minecraft::is_running()
        {
            self.running_text =
                "Stop Minecraft and finish mod management before removing instances.".into();
            return;
        }
        let Some(index) = self.selected_instance else {
            return;
        };

        let removed = self.instances.remove(index);
        if let Err(error) = crate::instances::save(&self.instances) {
            self.instances.insert(index, removed);
            self.running_text = format!("Failed to remove instance: {error}");
            return;
        }

        if self
            .mod_target
            .as_ref()
            .is_some_and(|target| target.game_dir() == removed.game_dir())
        {
            self.mod_target = None;
            self.installed_mods = None;
        }
        self.selected_instance = self
            .instances
            .get(index)
            .map(|_| index)
            .or_else(|| index.checked_sub(1));
        let name = removed.name.clone();
        self.running_text = match crate::instances::delete_game_dir(&removed) {
            Ok(()) => format!("Removed instance '{name}'."),
            Err(error) => format!("Removed '{name}', but could not delete its files: {error}"),
        };
    }

    /// Draws the brand and primary navigation in the left sidebar.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(18.0);
        ui.label(RichText::new("FERRITE").size(24.0).strong().color(ACCENT));
        ui.label(RichText::new("LAUNCHER").size(12.0).color(MUTED));
        ui.add_space(38.0);

        nav_button(ui, &mut self.current_page, Page::Play, "▶  PLAY");
        nav_button(ui, &mut self.current_page, Page::Instances, "▦  INSTANCES");
        nav_button(ui, &mut self.current_page, Page::Mods, "⬡  MODS");
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(18.0);
            nav_button(ui, &mut self.current_page, Page::Settings, "⚙  SETTINGS");
            ui.add_space(8.0);
            ui.label(RichText::new("Ferrite 0.1.0").small().color(MUTED));
        });
    }

    /// Draws the streamlined launch page.
    fn play_page(&mut self, ui: &mut egui::Ui) {
        page_heading(ui, "Play", "Choose an instance and start Minecraft.");
        self.account_section(ui);
        ui.add_space(24.0);

        egui::Frame::new()
            .fill(CARD)
            .corner_radius(12.0)
            .inner_margin(24.0)
            .show(ui, |ui| {
                ui.set_max_width(560.0);
                ui.label(RichText::new("INSTANCE").small().color(MUTED));
                egui::ComboBox::from_id_salt("play_instance")
                    .width(ui.available_width())
                    .selected_text(self.selected_instance_label())
                    .show_ui(ui, |ui| {
                        for (index, instance) in self.instances.iter().enumerate() {
                            ui.selectable_value(
                                &mut self.selected_instance,
                                Some(index),
                                format!(
                                    "{}  ·  {}  ·  {}",
                                    instance.name, instance.version, instance.loader
                                ),
                            );
                        }
                    });
                ui.add_space(20.0);

                if let Some(instance) = self.selected_instance() {
                    ui.label(RichText::new(&instance.name).size(28.0).strong());
                    ui.label(
                        RichText::new(format!(
                            "Minecraft {}  •  {}",
                            instance.version, instance.loader
                        ))
                        .color(MUTED),
                    );
                } else {
                    ui.label(RichText::new("No instances yet").size(24.0).strong());
                    ui.label(RichText::new("Create one from the Instances page.").color(MUTED));
                }

                ui.add_space(24.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            self.selected_instance().is_some()
                                && self.mod_task.is_none()
                                && self.pending_uninstall.is_none(),
                            egui::Button::new(RichText::new("▶  PLAY").strong())
                                .fill(ACCENT)
                                .min_size(egui::vec2(160.0, 42.0)),
                        )
                        .clicked()
                    {
                        self.launch_selected();
                    }
                    if ui.button("Stop game").clicked() {
                        self.running_text = match crate::minecraft::kill() {
                            Ok(()) => String::from("Game not running."),
                            Err(error) => format!("Failed to stop game: {error}"),
                        };
                    }
                });
            });
    }

    /// Draws saved profile cards and instance management actions.
    fn instances_page(&mut self, ui: &mut egui::Ui) {
        page_heading(ui, "Instances", "Manage your Minecraft profiles.");
        ui.add_space(8.0);
        if ui
            .add(egui::Button::new("＋ Create instance").fill(ACCENT))
            .clicked()
        {
            self.create_instance_open = true;
        }
        ui.add_space(20.0);

        if self.instances.is_empty() {
            ui.label(
                RichText::new("No instances created yet.")
                    .size(20.0)
                    .color(MUTED),
            );
            return;
        }

        let mut launch = false;
        let mut remove = false;
        let mut mods = None;
        let mod_idle = self.mod_task.is_none() && self.pending_uninstall.is_none();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, instance) in self.instances.iter().enumerate() {
                let selected = self.selected_instance == Some(index);
                egui::Frame::new()
                    .fill(if selected {
                        Color32::from_rgb(43, 39, 44)
                    } else {
                        CARD
                    })
                    .stroke(egui::Stroke::new(
                        if selected { 1.5 } else { 1.0 },
                        if selected {
                            ACCENT
                        } else {
                            Color32::from_rgb(50, 55, 64)
                        },
                    ))
                    .corner_radius(10.0)
                    .inner_margin(18.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        // Metadata and actions use separate rows so long names do
                        // not push buttons beyond a narrow viewport.
                        if ui
                            .selectable_label(
                                selected,
                                RichText::new(&instance.name).size(20.0).strong(),
                            )
                            .clicked()
                        {
                            self.selected_instance = Some(index);
                        }
                        ui.label(
                            RichText::new(format!(
                                "Minecraft {}  •  {}",
                                instance.version, instance.loader
                            ))
                            .color(MUTED),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(mod_idle, egui::Button::new("Play"))
                                .clicked()
                            {
                                self.selected_instance = Some(index);
                                launch = true;
                            }
                            if ui
                                .add_enabled(mod_idle, egui::Button::new("Mods"))
                                .clicked()
                            {
                                mods = Some(instance.clone());
                            }
                            if ui
                                .add_enabled(
                                    mod_idle && !crate::minecraft::is_running(),
                                    egui::Button::new("Remove"),
                                )
                                .clicked()
                            {
                                self.selected_instance = Some(index);
                                remove = true;
                            }
                        });
                    });
                ui.add_space(10.0);
            }
        });
        if let Some(target) = mods {
            self.set_mod_target(target);
            self.show_installed = true;
            self.current_page = Page::Mods;
            self.local_mod_task(None);
        }
        if launch {
            self.launch_selected();
        } else if remove {
            self.remove_selected();
        }
    }

    /// Changes only the explicit mod target; active workers and confirmations lock it.
    fn set_mod_target(&mut self, target: InstanceProfile) {
        if self.mod_task.is_some() || self.pending_uninstall.is_some() {
            return;
        }
        self.mod_filters.game_version = Some(target.version.clone());
        self.mod_filters.loader =
            (target.loader != "Vanilla").then(|| target.loader.to_ascii_lowercase());
        self.mod_target = Some(target);
        self.installed_mods = None;
    }

    /// Serializes all mod work, including searches triggered by Enter.
    fn start_mod_task(&mut self, work: impl FnOnce() -> ModTaskResult + Send + 'static) {
        if self.mod_task.is_some() || self.pending_uninstall.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("mods".into())
            .spawn(move || {
                let _ = sender.send(work());
            }) {
            Ok(_) => {
                self.mod_task = Some(receiver);
                self.running_text = "Working on mods…".into();
            }
            Err(error) => self.running_text = format!("Failed to start mod worker: {error}"),
        }
    }

    /// Searches with a captured filter set, independent of subsequent form edits.
    fn search_modrinth(&mut self, filters: SearchFilters) {
        self.start_mod_task(move || {
            let result = crate::modrinth::search_filtered(&filters);
            ModTaskResult::Search(filters, result)
        });
    }

    /// Downloads project metadata without blocking rendering.
    fn open_mod_details(&mut self, id: String, is_mod: bool) {
        self.start_mod_task(move || ModTaskResult::Details(is_mod, crate::modrinth::details(&id)));
    }

    /// Checks the captured target still belongs to the live profile list.
    fn target_exists(&self, target: &InstanceProfile) -> bool {
        self.instances
            .iter()
            .any(|profile| profile.game_dir() == target.game_dir())
    }

    /// Downloads a mod and dependencies into an immutable, explicitly chosen target.
    fn install_mod(&mut self, project_id: String, title: String, is_mod: bool) {
        if !self.can_install(is_mod) {
            return;
        }
        let Some(target) = self.mod_target.clone() else {
            return;
        };
        if !is_mod
            || !self.target_exists(&target)
            || !matches!(
                target.loader.to_ascii_lowercase().as_str(),
                "fabric" | "forge" | "neoforge" | "quilt"
            )
            || crate::minecraft::is_running()
        {
            return;
        }
        self.installed_mods = None;
        self.start_mod_task(move || {
            let result = if crate::minecraft::is_running() {
                Err("Stop Minecraft before installing mods.".into())
            } else {
                crate::modrinth::install(
                    &project_id,
                    &target.version,
                    &target.loader.to_ascii_lowercase(),
                    target.game_dir(),
                )
                .map(|paths| paths.len())
            };
            let message = match result {
                Ok(count) => format!("Installed {title} ({count} files) into '{}'.", target.name),
                Err(error) => format!("Install into '{}' failed: {error}", target.name),
            };
            let result = crate::instance_mods::list(target.game_dir());
            ModTaskResult::Local {
                target,
                message,
                result,
            }
        });
    }

    /// Lists or mutates local jars on a worker, then refreshes even after a failure.
    /// `Some((filename, None))` uninstalls; a boolean enables/disables the exact file.
    fn local_mod_task(&mut self, action: Option<(String, Option<bool>)>) {
        if self.mod_task.is_some() || self.pending_uninstall.is_some() {
            return;
        }
        let Some(target) = self.mod_target.clone() else {
            return;
        };
        if !self.target_exists(&target) || crate::minecraft::is_running() {
            return;
        }
        self.installed_mods = None;
        self.start_mod_task(move || {
            let operation = if crate::minecraft::is_running() {
                Err("Stop Minecraft before managing mods.".into())
            } else {
                match action {
                    Some((filename, Some(enabled))) => {
                        crate::instance_mods::set_enabled(target.game_dir(), &filename, enabled)
                            .map(|_| ())
                    }
                    Some((filename, None)) => {
                        crate::instance_mods::uninstall(target.game_dir(), &filename)
                    }
                    None => Ok(()),
                }
            };
            let message = match operation {
                Ok(()) => format!("Refreshed mods for '{}'.", target.name),
                Err(error) => format!("Mods for '{}': {error}", target.name),
            };
            let result = crate::instance_mods::list(target.game_dir());
            ModTaskResult::Local {
                target,
                message,
                result,
            }
        });
    }

    /// Polls globally; local data is applied only to its captured target.
    fn poll_mod_task(&mut self) {
        let Some(receiver) = &self.mod_task else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.mod_task = None;
                self.running_text =
                    "The mod worker stopped unexpectedly; refresh before retrying.".into();
                return;
            }
        };
        self.mod_task = None;
        match result {
            ModTaskResult::Search(filters, result) => match result {
                Ok(response) => {
                    self.running_text = format!("{} Modrinth projects found.", response.total_hits);
                    self.result_filters = filters;
                    self.mod_results = Some(response);
                    self.mod_details = None;
                }
                Err(error) => self.running_text = format!("Search failed: {error}"),
            },
            ModTaskResult::Details(is_mod, result) => match result {
                Ok(details) => {
                    self.mod_details = Some((is_mod, details));
                    self.running_text = "Project details loaded.".into();
                }
                Err(error) => self.running_text = format!("Details failed: {error}"),
            },
            ModTaskResult::Local {
                target,
                message,
                result,
            } => {
                self.running_text = message;
                if self.target_exists(&target)
                    && self
                        .mod_target
                        .as_ref()
                        .is_some_and(|current| current.game_dir() == target.game_dir())
                {
                    match result {
                        Ok(mods) => self.installed_mods = Some(mods),
                        Err(error) => {
                            self.installed_mods = None;
                            self.running_text
                                .push_str(&format!(" List failed: {error}"));
                        }
                    }
                }
            }
        }
    }

    /// Draws the Modrinth browser and local management with an explicit target.
    fn mods_page(&mut self, ui: &mut egui::Ui) {
        page_heading(
            ui,
            "Mods",
            "Manage installed jars or browse Modrinth projects.",
        );
        let idle = self.mod_task.is_none() && self.pending_uninstall.is_none();
        let mut target = None;
        ui.add_enabled_ui(idle, |ui| {
            egui::ComboBox::from_id_salt("mod_target")
                .selected_text(
                    self.mod_target
                        .as_ref()
                        .map(|p| format!("Target: {} · {} · {}", p.name, p.version, p.loader))
                        .unwrap_or_else(|| "Choose target instance".into()),
                )
                .show_ui(ui, |ui| {
                    for profile in &self.instances {
                        if ui
                            .selectable_label(
                                false,
                                format!(
                                    "{} · {} · {}",
                                    profile.name, profile.version, profile.loader
                                ),
                            )
                            .clicked()
                        {
                            target = Some(profile.clone());
                        }
                    }
                });
        });
        if let Some(target) = target {
            self.set_mod_target(target);
            if self.show_installed {
                self.local_mod_task(None);
            }
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.show_installed, true, "Installed mods");
            ui.selectable_value(&mut self.show_installed, false, "Browse Modrinth");
        });
        if self.mod_task.is_some() {
            ui.spinner();
        }
        if crate::minecraft::is_running() {
            ui.label("Stop Minecraft before refreshing or changing installed mods.");
        }
        egui::ScrollArea::vertical()
            .id_salt("mods_content")
            .show(ui, |ui| {
                if self.show_installed {
                    self.installed_mods_ui(ui);
                } else if self.mod_details.is_some() {
                    self.mod_details_ui(ui);
                } else {
                    self.mod_browser_ui(ui);
                }
            });
    }

    /// Shows metadata without decoding untrusted archive icons on the UI thread.
    fn installed_mods_ui(&mut self, ui: &mut egui::Ui) {
        let ready = self.mod_task.is_none()
            && self.pending_uninstall.is_none()
            && self.mod_target.is_some()
            && !crate::minecraft::is_running();
        if ui
            .add_enabled(ready, egui::Button::new("Refresh installed mods"))
            .clicked()
        {
            self.local_mod_task(None);
        }
        let mut action = None;
        if let Some(mods) = &self.installed_mods {
            if mods.is_empty() {
                ui.label("No installed mods.");
            }
            for installed in mods {
                ui.push_id(&installed.filename, |ui| {
                    ui.separator();
                    ui.strong(&installed.name);
                    ui.label(format!(
                        "Version: {} · {}",
                        installed.version.as_deref().unwrap_or("Unknown"),
                        if installed.enabled {
                            "Enabled"
                        } else {
                            "Disabled"
                        }
                    ));
                    ui.label(&installed.filename);
                    // Include content in the key so replacing a JAR then refreshing
                    // cannot reuse an old icon from the same filename.
                    let mut hash = std::collections::hash_map::DefaultHasher::new();
                    std::hash::Hash::hash(&installed.icon, &mut hash);
                    let key = format!("{:x}", std::hash::Hasher::finish(&hash));
                    self.icons
                        .show_bytes(ui, &key, installed.icon.as_deref(), 48.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                ready,
                                egui::Button::new(if installed.enabled {
                                    "Disable"
                                } else {
                                    "Enable"
                                }),
                            )
                            .clicked()
                        {
                            action = Some((installed.filename.clone(), Some(!installed.enabled)));
                        }
                        if ui
                            .add_enabled(ready, egui::Button::new("Uninstall…"))
                            .clicked()
                        {
                            self.pending_uninstall = self
                                .mod_target
                                .clone()
                                .map(|target| (target, installed.filename.clone()));
                        }
                    });
                });
            }
        } else {
            ui.label("Refresh to load this instance's mods.");
        }
        if let Some(action) = action {
            self.local_mod_task(Some(action));
        }
    }

    /// Requires explicit confirmation of both instance and exact filename.
    fn uninstall_window(&mut self, context: &egui::Context) {
        let Some((target, filename)) = self.pending_uninstall.clone() else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Uninstall mod?")
            .open(&mut open)
            .collapsible(false)
            .show(context, |ui| {
                ui.label(format!(
                    "Permanently delete {filename} from '{}' ?",
                    target.name
                ));
                ui.label("This cannot be undone. Dependencies are not removed.");
                confirm = ui
                    .add_enabled(
                        self.mod_task.is_none() && !crate::minecraft::is_running(),
                        egui::Button::new("Uninstall permanently"),
                    )
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
        if confirm {
            self.pending_uninstall = None;
            if self.target_exists(&target) {
                self.mod_target = Some(target);
                self.local_mod_task(Some((filename, None)));
            }
        } else if cancel || !open {
            self.pending_uninstall = None;
        }
    }

    /// Exposes all search facets; pagination always reuses the submitted filter set.
    fn mod_browser_ui(&mut self, ui: &mut egui::Ui) {
        let idle = self.mod_task.is_none() && self.pending_uninstall.is_none();
        let mut search = false;
        ui.add_enabled_ui(idle, |ui| {
            let response = ui.text_edit_singleline(&mut self.mod_filters.query);
            search = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.collapsing("Search filters", |ui| {
                optional_text(
                    ui,
                    "Minecraft version (exact)",
                    &mut self.mod_filters.game_version,
                );
                optional_choice(
                    ui,
                    "Loader",
                    &mut self.mod_filters.loader,
                    &["fabric", "forge", "neoforge", "quilt"],
                );
                optional_choice(
                    ui,
                    "Client side",
                    &mut self.mod_filters.client_side,
                    &["required", "optional", "unsupported", "unknown"],
                );
                optional_choice(
                    ui,
                    "Server side",
                    &mut self.mod_filters.server_side,
                    &["required", "optional", "unsupported", "unknown"],
                );
                ui.label("Categories (comma-separated IDs; all must match)");
                ui.text_edit_singleline(&mut self.mod_categories);
                optional_choice(
                    ui,
                    "Project type",
                    &mut self.mod_filters.project_type,
                    &[
                        "mod",
                        "modpack",
                        "resourcepack",
                        "shader",
                        "plugin",
                        "datapack",
                    ],
                );
                egui::ComboBox::from_label("Sort")
                    .selected_text(&self.mod_filters.sort)
                    .show_ui(ui, |ui| {
                        for sort in ["relevance", "downloads", "follows", "newest", "updated"] {
                            ui.selectable_value(&mut self.mod_filters.sort, sort.into(), sort);
                        }
                    });
            });
            ui.horizontal(|ui| {
                search |= ui.button("Search").clicked();
                if ui.button("Popular (downloads)").clicked() {
                    self.mod_filters.query.clear();
                    self.mod_filters.sort = "downloads".into();
                    search = true;
                }
            });
        });
        if idle && search {
            self.mod_filters.categories = self
                .mod_categories
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            self.mod_filters.offset = 0;
            self.search_modrinth(self.mod_filters.clone());
        }
        let idle = self.mod_task.is_none() && self.pending_uninstall.is_none();
        let can_install =
            self.can_install(self.result_filters.project_type.as_deref() == Some("mod"));
        let mut page = None;
        let mut details = None;
        let mut install = None;
        if let Some(response) = &self.mod_results {
            ui.label(format!(
                "{} total · showing {}–{} · {} · {}",
                response.total_hits,
                if response.hits.is_empty() {
                    0
                } else {
                    response.offset + 1
                },
                response.offset + response.hits.len() as u64,
                self.result_filters
                    .project_type
                    .as_deref()
                    .unwrap_or("all project types"),
                self.result_filters.sort
            ));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(idle && response.offset > 0, egui::Button::new("Previous"))
                    .clicked()
                {
                    page = Some(response.offset.saturating_sub(response.limit));
                }
                if ui
                    .add_enabled(
                        idle && response.limit > 0
                            && response.offset.saturating_add(response.limit) < response.total_hits,
                        egui::Button::new("Next"),
                    )
                    .clicked()
                {
                    page = Some(response.offset.saturating_add(response.limit));
                }
            });
            for hit in &response.hits {
                ui.separator();
                if ui
                    .add_enabled(idle, egui::Button::new(RichText::new(&hit.title).strong()))
                    .clicked()
                {
                    details = Some(hit.project_id.clone());
                }
                self.icons.show_url(ui, hit.icon_url.as_deref(), 48.0);
                ui.label(format!(
                    "By {} · {} downloads",
                    hit.author.as_deref().unwrap_or("Unknown"),
                    format_downloads(hit.downloads)
                ));
                ui.label(&hit.description);
                ui.label(format!(
                    "Categories: {}",
                    hit.categories.as_deref().unwrap_or_default().join(", ")
                ));
                ui.collapsing(
                    format!("Supported Minecraft versions · {}", hit.title),
                    |ui| {
                        ui.label(hit.versions.as_deref().unwrap_or_default().join(", "));
                    },
                );
                if let Some(target) = &self.mod_target {
                    let supported = hit
                        .versions
                        .as_ref()
                        .map(|versions| versions.contains(&target.version));
                    ui.label(format!(
                        "Target Minecraft {}: {} (loader checked at install)",
                        target.version,
                        match supported {
                            Some(true) => "supported",
                            Some(false) => "not listed",
                            None => "unknown",
                        }
                    ));
                }
                if ui
                    .add_enabled(can_install, egui::Button::new("Install into target"))
                    .clicked()
                {
                    install = Some((hit.project_id.clone(), hit.title.clone()));
                }
            }
        }
        if let Some(offset) = page {
            let mut filters = self.result_filters.clone();
            filters.offset = offset;
            self.search_modrinth(filters);
        } else if let Some(id) = details {
            self.open_mod_details(
                id,
                self.result_filters.project_type.as_deref() == Some("mod"),
            );
        } else if let Some((id, title)) = install {
            self.install_mod(
                id,
                title,
                self.result_filters.project_type.as_deref() == Some("mod"),
            );
        }
        ui.label("Installation is available only for mod-only results and a mod-loader target.");
    }

    /// Installation eligibility is based on result provenance, not editable filters.
    fn can_install(&self, is_mod: bool) -> bool {
        is_mod
            && self.mod_task.is_none()
            && self.pending_uninstall.is_none()
            && !crate::minecraft::is_running()
            && self.mod_target.as_ref().is_some_and(|target| {
                self.target_exists(target)
                    && matches!(
                        target.loader.to_ascii_lowercase().as_str(),
                        "fabric" | "forge" | "neoforge" | "quilt"
                    )
            })
    }

    /// Displays full metadata, team, releases and dependency links with a back action.
    fn mod_details_ui(&mut self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(
                self.mod_task.is_none(),
                egui::Button::new("← Back to results"),
            )
            .clicked()
        {
            self.mod_details = None;
            return;
        }
        let Some((is_mod, details)) = &self.mod_details else {
            return;
        };
        let project = &details.project;
        ui.heading(&project.title);
        self.icons.show_url(ui, project.icon_url.as_deref(), 96.0);
        ui.label(format!(
            "By {} · {} downloads",
            details.author().unwrap_or("Unknown"),
            format_downloads(project.downloads)
        ));
        ui.label(&project.description);
        ui.hyperlink_to(
            "Open on Modrinth",
            format!("https://modrinth.com/project/{}", project.slug),
        );
        if let Some(url) = &project.source_url {
            ui.hyperlink_to("Source", url);
        }
        if let Some(url) = &project.issues_url {
            ui.hyperlink_to("Issues", url);
        }
        ui.label(format!("Minecraft: {}", project.game_versions.join(", ")));
        ui.label(format!("Loaders: {}", project.loaders.join(", ")));
        ui.label(format!(
            "Client: {} · Server: {}",
            project.client_side, project.server_side
        ));
        ui.collapsing("Team", |ui| {
            for member in details.team_members.iter().filter(|m| m.accepted) {
                ui.hyperlink_to(
                    format!("{} · {}", member.user.username, member.role),
                    format!("https://modrinth.com/user/{}", member.user.username),
                );
            }
        });
        ui.collapsing("Description (Markdown source)", |ui| {
            ui.label(&project.body);
        });
        let install = ui
            .add_enabled(
                self.can_install(*is_mod),
                egui::Button::new("Install compatible release into target"),
            )
            .clicked();
        for version in &details.versions {
            ui.push_id(&version.id, |ui| {
                ui.collapsing(
                    format!(
                        "{} · {} · {}",
                        version.name, version.version_number, version.version_type
                    ),
                    |ui| {
                        ui.label(format!(
                            "{} · {} downloads",
                            version.date_published,
                            format_downloads(version.downloads)
                        ));
                        ui.label(format!(
                            "Minecraft: {} · Loaders: {}",
                            version.game_versions.join(", "),
                            version.loaders.join(", ")
                        ));
                        ui.hyperlink_to(
                            "Version page",
                            format!(
                                "https://modrinth.com/project/{}/version/{}",
                                version.project_id, version.id
                            ),
                        );
                        for dependency in &version.dependencies {
                            ui.label(format!(
                                "Dependency: {} · {}",
                                dependency.dependency_type,
                                dependency.file_name.as_deref().unwrap_or("")
                            ));
                            if let Some(id) = &dependency.project_id {
                                ui.hyperlink_to(id, format!("https://modrinth.com/project/{id}"));
                                if let Some(version) = &dependency.version_id {
                                    ui.hyperlink_to(
                                        version,
                                        format!(
                                            "https://modrinth.com/project/{id}/version/{version}"
                                        ),
                                    );
                                }
                            } else if let Some(id) = &dependency.version_id {
                                ui.label(format!("Version ID: {id}"));
                            }
                        }
                        if let Some(changelog) = &version.changelog {
                            ui.collapsing("Changelog", |ui| {
                                ui.label(changelog);
                            });
                        }
                    },
                );
            });
        }
        if install {
            let (id, title, is_mod) = (project.id.clone(), project.title.clone(), *is_mod);
            self.install_mod(id, title, is_mod);
        }
    }

    /// Draws launcher settings as page content rather than a popup window.
    fn settings_page(&mut self, ui: &mut egui::Ui) {
        page_heading(ui, "Settings", "Configure Ferrite Launcher.");
        self.account_section(ui);
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            for tab in ["Global", "Launcher", "Appearance"] {
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
                ui.checkbox(&mut self.is_global_checked, "Enable global defaults");
            }
            "Launcher" => {
                ui.heading("Launcher settings");
                ui.checkbox(
                    &mut self.is_launcher_checked,
                    "Keep launcher open while playing",
                );
            }
            _ => {
                ui.heading("Appearance");
                ui.checkbox(
                    &mut self.is_appearance_checked,
                    "Use compact instance cards",
                );
            }
        }
    }

    /// Draws the modal form used to create a persisted instance.
    fn create_instance_window(&mut self, context: &egui::Context) {
        if !self.create_instance_open {
            return;
        }

        let mut open = self.create_instance_open;
        let mut create_requested = false;
        egui::Window::new("Create instance")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(context, |ui| {
                ui.label("Each instance keeps its own worlds, mods, and settings.");
                ui.add_space(10.0);
                ui.add_enabled_ui(self.instance_creation_task.is_none(), |ui| {
                    ui.label("Instance name");
                    ui.text_edit_singleline(&mut self.instance_name);
                    egui::ComboBox::from_label("Minecraft version")
                        .selected_text(&self.selected_version)
                        .show_ui(ui, |ui| {
                            for version in &self.versions {
                                ui.selectable_value(
                                    &mut self.selected_version,
                                    version.clone(),
                                    version,
                                );
                            }
                        });
                    egui::ComboBox::from_label("Mod loader")
                        .selected_text(&self.selected_loader)
                        .show_ui(ui, |ui| {
                            for loader in ModLoader::ALL {
                                ui.selectable_value(
                                    &mut self.selected_loader,
                                    loader.label().to_owned(),
                                    loader.label(),
                                );
                            }
                        });
                    ui.add_space(12.0);
                    create_requested = ui
                        .add_enabled(
                            !self.instance_name.trim().is_empty(),
                            egui::Button::new("Create instance").fill(ACCENT),
                        )
                        .clicked();
                });
                if let Some(status) = &self.instance_creation_status {
                    ui.label(status);
                }
            });
        self.create_instance_open = open;
        if create_requested {
            self.create_instance();
        }
    }
}

impl eframe::App for Ferrite {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_instance_creation();
        self.poll_mod_task();
        self.poll_auth();
        ui.style_mut().visuals = egui::Visuals::dark();
        ui.style_mut().visuals.panel_fill = BACKGROUND;
        ui.style_mut().spacing.item_spacing = egui::vec2(10.0, 10.0);
        ui.painter().rect_filled(ui.max_rect(), 0.0, BACKGROUND);

        // This eframe integration gives `App::ui` an existing root `Ui`, so
        // the sidebar and content area are composed horizontally inside it.
        let available_width = ui.available_width();
        let available_height = ui.available_height();
        let sidebar_width = (available_width * 0.2).clamp(145.0, 175.0);
        let gap = ui.spacing().item_spacing.x;
        let content_outer_width = (available_width - sidebar_width - gap).max(0.0);
        let content_width = (content_outer_width - 64.0).max(0.0);
        let content_height = (available_height - 64.0).max(0.0);

        ui.horizontal_top(|ui| {
            egui::Frame::new()
                .fill(SIDEBAR)
                .inner_margin(18.0)
                .show(ui, |ui| {
                    ui.set_width((sidebar_width - 36.0).max(0.0));
                    ui.set_min_height((available_height - 36.0).max(0.0));
                    // Frames inherit their parent's layout. Reset the horizontal
                    // column layout before drawing vertically stacked navigation.
                    ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                        self.sidebar(ui);
                    });
                });

            egui::Frame::new()
                .fill(BACKGROUND)
                .inner_margin(egui::Margin::same(32))
                .show(ui, |ui| {
                    ui.set_width(content_width);
                    ui.set_min_height(content_height);

                    ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                        // Reserve a fixed footer and give the page the remaining
                        // height. Scrollable pages now receive the right dimensions
                        // on the first frame instead of after a resize.
                        let page_height = (content_height - 38.0).max(0.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(content_width, page_height),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| match self.current_page {
                                Page::Play => self.play_page(ui),
                                Page::Instances => self.instances_page(ui),
                                Page::Mods => self.mods_page(ui),
                                Page::Settings => self.settings_page(ui),
                            },
                        );
                        ui.separator();
                        let status = if self.instance_creation_task.is_some() {
                            self.instance_creation_status
                                .as_deref()
                                .unwrap_or(&self.running_text)
                        } else {
                            &self.running_text
                        };
                        ui.label(RichText::new(status).color(MUTED));
                    });
                });
        });

        self.account_window(ui.ctx());
        self.create_instance_window(ui.ctx());
        self.uninstall_window(ui.ctx());
        if self.instance_creation_task.is_some()
            || self.mod_task.is_some()
            || self.auth.task.is_some()
        {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        } else if self.auth.account.is_some() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
    }
}

/// Draws one full-width sidebar destination.
fn nav_button(ui: &mut egui::Ui, current: &mut Page, page: Page, label: &str) {
    let selected = *current == page;
    if ui
        .add_sized(
            [ui.available_width(), 38.0],
            egui::Button::selectable(selected, RichText::new(label).strong()),
        )
        .clicked()
    {
        *current = page;
    }
}

/// Draws a consistent page title and supporting subtitle.
fn page_heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.heading(RichText::new(title).size(32.0).strong());
    ui.label(RichText::new(subtitle).color(MUTED));
}

/// Optional exact facet text; empty input omits the facet.
fn optional_text(ui: &mut egui::Ui, label: &str, value: &mut Option<String>) {
    ui.label(label);
    let mut text = value.clone().unwrap_or_default();
    if ui.text_edit_singleline(&mut text).changed() {
        *value = (!text.trim().is_empty()).then(|| text.trim().to_owned());
    }
}

/// Selects an API facet or leaves it unrestricted.
fn optional_choice(ui: &mut egui::Ui, label: &str, value: &mut Option<String>, choices: &[&str]) {
    egui::ComboBox::from_label(label)
        .selected_text(value.as_deref().unwrap_or("Any"))
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "Any");
            for choice in choices {
                ui.selectable_value(value, Some((*choice).into()), *choice);
            }
        });
}

/// Image-loading hook: keep the URL accessible until a bounded decoder is integrated.

/// Formats large download counts compactly for result cards.
fn format_downloads(downloads: u64) -> String {
    if downloads >= 1_000_000 {
        format!("{:.1}M", downloads as f64 / 1_000_000.0)
    } else if downloads >= 1_000 {
        format!("{:.1}K", downloads as f64 / 1_000.0)
    } else {
        downloads.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Avoids the startup network request while testing UI state transitions.
    fn app() -> Ferrite {
        Ferrite {
            auth: AccountSession::default(),
            icons: IconCache::default(),
            running_text: String::new(),
            current_page: Page::Mods,
            current_settings_tab: "Global".into(),
            is_global_checked: false,
            is_launcher_checked: false,
            is_appearance_checked: false,
            create_instance_open: false,
            instance_name: String::new(),
            instance_creation_task: None,
            instance_creation_status: None,
            selected_version: "1.21.1".into(),
            selected_loader: "Fabric".into(),
            versions: Vec::new(),
            instances: Vec::new(),
            selected_instance: None,
            mod_target: None,
            mod_filters: SearchFilters::default(),
            result_filters: SearchFilters::default(),
            mod_categories: String::new(),
            mod_results: None,
            mod_details: None,
            installed_mods: None,
            show_installed: false,
            pending_uninstall: None,
            mod_task: None,
        }
    }

    /// Installs a fake worker channel without contacting Microsoft.
    fn auth_worker(app: &mut Ferrite) -> (mpsc::Sender<AuthEvent>, Arc<AtomicBool>) {
        let (sender, events) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        app.auth.task = Some(AuthTask {
            events,
            cancel: Arc::clone(&cancel),
        });
        (sender, cancel)
    }

    #[test]
    fn launch_requires_account_and_opens_sign_in() {
        let mut app = app();
        app.instances.push(profile("Authenticated only"));
        app.selected_instance = Some(0);
        app.launch_selected();
        assert!(app.auth.open);
        assert!(app.running_text.contains("Sign in with Microsoft"));
        assert!(app.auth.task.is_none());
    }

    #[test]
    fn empty_client_id_never_starts_worker() {
        let mut app = app();
        app.auth.client_id = "  ".into();
        app.start_sign_in();
        assert!(app.auth.task.is_none());
        assert!(app.auth.status.contains("application ID"));
    }

    #[test]
    fn auth_events_are_polled_on_other_pages_and_errors_clear_device() {
        let mut app = app();
        let (sender, cancel) = auth_worker(&mut app);
        sender
            .send(AuthEvent::Device {
                user_code: "TEST-CODE".into(),
                verification_uri: "https://microsoft.com/link".into(),
            })
            .unwrap();
        sender
            .send(AuthEvent::Progress("Waiting for Microsoft".into()))
            .unwrap();
        app.poll_auth();
        assert!(app.auth.device.is_some());
        assert_eq!(app.auth.status, "Waiting for Microsoft");
        assert_eq!(app.running_text, app.auth.status);
        sender
            .send(AuthEvent::Account(Err("Access denied".into())))
            .unwrap();
        app.poll_auth();
        assert!(app.auth.device.is_none());
        assert!(app.auth.task.is_none());
        assert!(cancel.load(Ordering::Relaxed));
        assert!(app.auth.status.contains("Access denied"));
    }

    #[test]
    fn cancel_and_sign_out_discard_queued_and_late_results() {
        let mut app = app();
        for sign_out in [false, true] {
            let (sender, cancel) = auth_worker(&mut app);
            sender
                .send(AuthEvent::Progress("Stale progress".into()))
                .unwrap();
            sender
                .send(AuthEvent::Account(Err("Stale result".into())))
                .unwrap();
            app.auth.device = Some(("TEST".into(), "https://microsoft.com/link".into()));
            if sign_out {
                app.sign_out();
            } else {
                app.cancel_sign_in();
            }
            assert!(cancel.load(Ordering::Relaxed));
            assert!(app.auth.device.is_none());
            assert!(
                sender
                    .send(AuthEvent::Progress("Late progress".into()))
                    .is_err()
            );
            let status = app.auth.status.clone();
            let (_new_sender, _) = auth_worker(&mut app);
            app.poll_auth();
            assert_eq!(app.auth.status, status);
            assert!(app.auth.account.is_none());
        }
    }

    #[test]
    fn dropped_app_cancels_worker_and_disconnect_is_reported() {
        let mut app = app();
        let (sender, cancel) = auth_worker(&mut app);
        drop(sender);
        app.poll_auth();
        assert!(app.auth.status.contains("stopped unexpectedly"));
        assert!(cancel.load(Ordering::Relaxed));
        let (sender, cancel) = auth_worker(&mut app);
        drop(app);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(sender.send(AuthEvent::Progress("Late".into())).is_err());
    }

    fn profile(name: &str) -> InstanceProfile {
        InstanceProfile::new(name.into(), "1.21.1".into(), "Fabric".into(), &[])
    }

    #[test]
    fn instance_progress_and_failure_preserve_form_inputs() {
        let mut app = app();
        app.instance_name = "My new instance".into();
        let (sender, receiver) = mpsc::channel();
        app.instance_creation_task = Some(receiver);
        sender
            .send(InstanceCreationEvent::DownloadProgress(
                "Downloading assets".into(),
            ))
            .unwrap();
        app.poll_instance_creation();
        assert_eq!(
            app.instance_creation_status.as_deref(),
            Some("Downloading assets")
        );
        assert!(app.instance_creation_task.is_some());
        sender
            .send(InstanceCreationEvent::Finished(Err(
                "Network interrupted".into()
            )))
            .unwrap();
        app.poll_instance_creation();
        assert!(app.instance_creation_task.is_none());
        assert!(app.create_instance_open);
        assert_eq!(app.instance_name, "My new instance");
        assert!(app.running_text.contains("Network interrupted"));
        assert!(app.instances.is_empty());
    }

    #[test]
    fn active_worker_locks_target_and_rejects_second_task() {
        let mut app = app();
        app.set_mod_target(profile("first"));
        let (sender, receiver) = mpsc::channel();
        app.mod_task = Some(receiver);
        app.set_mod_target(profile("second"));
        app.start_mod_task(|| panic!("must not start another worker"));
        assert_eq!(app.mod_target.as_ref().unwrap().name, "first");
        sender
            .send(ModTaskResult::Search(
                SearchFilters::default(),
                Err("original worker".into()),
            ))
            .unwrap();
        app.poll_mod_task();
        assert!(app.running_text.contains("original worker"));
        assert!(app.mod_task.is_none());
    }

    #[test]
    fn local_results_never_follow_play_selection_or_stale_targets() {
        let mut app = app();
        let first = profile("first");
        let second = profile("second");
        app.instances = vec![first.clone(), second.clone()];
        app.set_mod_target(first.clone());
        app.selected_instance = Some(1); // Creation completion or a Play selection.
        let (sender, receiver) = mpsc::channel();
        app.mod_task = Some(receiver);
        sender
            .send(ModTaskResult::Local {
                target: first,
                message: "first finished".into(),
                result: Ok(vec![]),
            })
            .unwrap();
        app.poll_mod_task();
        assert_eq!(app.installed_mods, Some(vec![]));
        app.installed_mods = None;
        let (sender, receiver) = mpsc::channel();
        app.mod_task = Some(receiver);
        sender
            .send(ModTaskResult::Local {
                target: second,
                message: "stale".into(),
                result: Ok(vec![]),
            })
            .unwrap();
        app.poll_mod_task();
        assert!(app.installed_mods.is_none());
    }

    #[test]
    fn result_filters_survive_form_edits_and_non_mod_install_is_rejected() {
        let mut app = app();
        let target = profile("target");
        app.instances.push(target.clone());
        app.set_mod_target(target);
        let mut submitted = SearchFilters::default();
        submitted.project_type = Some("shader".into());
        submitted.offset = 20;
        let (sender, receiver) = mpsc::channel();
        app.mod_task = Some(receiver);
        sender
            .send(ModTaskResult::Search(
                submitted.clone(),
                Ok(SearchResponse {
                    hits: vec![],
                    offset: 20,
                    limit: 20,
                    total_hits: 100,
                }),
            ))
            .unwrap();
        app.mod_filters.project_type = Some("mod".into());
        app.poll_mod_task();
        assert_eq!(app.result_filters, submitted);
        assert!(!app.can_install(false));
        app.install_mod("unused".into(), "shader".into(), false);
        assert!(app.mod_task.is_none());
        app.mod_target.as_mut().unwrap().loader = "Vanilla".into();
        assert!(!app.can_install(true));
    }
}
