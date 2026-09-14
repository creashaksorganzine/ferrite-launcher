//! # MAIN.RS
//! litterally just serves as a "start the program" script

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

/// Runs [`app::run`] and reports native window startup failures to the caller.
fn main() -> eframe::Result {
    println!("Starting GUI!");
    app::run()
}
