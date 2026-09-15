//! Native entry point for Ferrite Launcher.
//!
//! Application state and UI orchestration live in [`app`]; this module stays thin so
//! eframe startup errors can propagate through `main` without duplicating setup.

mod app;
mod auth;
mod config;
mod discord;
mod icons;
mod instance_mods;
mod instances;
mod loaders;
mod minecraft;
mod modrinth;
mod packs;
mod updates;

/// Starts the native UI and returns any window/event-loop initialization error.
fn main() -> eframe::Result {
    println!("Starting GUI!");
    app::run()
}
