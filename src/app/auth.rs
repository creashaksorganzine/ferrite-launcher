//! Microsoft account UI and the cancellable device-code worker lifecycle.
//!
//! Authentication performs blocking HTTP work on one worker. The UI owns credentials and
//! receives display-safe progress, public device instructions, and the terminal account.
//! Cancellation drops the receiver and sets a shared atomic flag rather than waiting for
//! the worker.

use super::{AuthEvent, AuthTask, Ferrite};
use eframe::egui::{self, RichText};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};

impl Ferrite {
    /// Starts blocking Microsoft authentication off the UI thread with a fresh channel.
    ///
    /// The worker owns the client ID and cancellation handle; its channel is the only
    /// route by which device instructions or credentials can re-enter UI-owned state.
    pub(super) fn start_sign_in(&mut self) {
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
        // UI and worker need independent ownership because the worker is detached.
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
    pub(super) fn cancel_sign_in(&mut self) {
        self.auth.task = None;
        self.auth.device = None;
        self.auth.status = "Sign-in cancelled.".into();
    }

    /// Forgets session credentials and prevents late worker results from signing back in.
    pub(super) fn sign_out(&mut self) {
        self.cancel_sign_in();
        self.auth.account = None;
        self.auth.status = "Signed out.".into();
        self.running_text = self.auth.status.clone();
    }

    /// Drains all currently queued auth events without blocking the frame.
    ///
    /// This runs regardless of the visible page. A terminal event drops [`AuthTask`],
    /// which also marks cancellation and ensures no later event can revive the attempt.
    pub(super) fn poll_auth(&mut self) {
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

    /// Shows the launch authentication mode and account state on Play and Settings.
    pub(super) fn account_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Account");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.auth.offline_mode, false, "Microsoft account");
            ui.selectable_value(&mut self.auth.offline_mode, true, "Offline mode");
        });

        if self.auth.offline_mode {
            ui.label("Offline mode uses the name 'Player' and does not require Microsoft sign-in.");
            ui.label(
                RichText::new("Online-mode servers and paid-account services will not work.")
                    .color(self.muted_color()),
            );
            return;
        }

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
    pub(super) fn account_window(&mut self, context: &egui::Context) {
        if !self.auth.open {
            return;
        }
        // Keep egui's `open` borrow local; reconcile it with `self` after the closure.
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
}
