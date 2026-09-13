//! # Mod-loader installation/launch dispatch for Ferrite Launcher.
//!
//! `app.rs` should call into this module for install/launch instead of
//! `crate::minecraft` directly, so loader selection lives in one place.
//! Each loader gets its own submodule; this file just matches on
//! `ModLoader` and delegates.
//!
//! # How a loader installs (see `fabric.rs` for the concrete example)
//!
//! `crate::minecraft` has no idea mod loaders exist, and it doesn't need
//! to: a loader installs by building an ordinary *vanilla-shaped*
//! synthetic version (its own id, its own metadata JSON, its own copy
//! of `client.jar`) out of the already-installed vanilla version plus
//! whatever the loader adds, and then just calls
//! `crate::minecraft::launch_authenticated` / `is_version_installed` on that
//! synthetic id like it was any other release. That's what keeps this
//! module — and `minecraft.rs` — from needing to know anything
//! loader-specific about classpath building, argument resolution, or
//! process spawning.
//!
//! # Adding a new loader (Forge / NeoForge / Quilt)
//!
//! 1. Add a submodule (`forge.rs`, `neoforge.rs`, `quilt.rs`) exposing
//!    at minimum:
//!    - `install(mc_version: &str) -> minecraft::Result<()>`
//!    - `installed_composite_id(mc_version: &str) -> minecraft::Result<String>`
//!    following the pattern in `fabric.rs`. Forge/NeoForge installers
//!    are `.jar`-based rather than a clean metadata API, so that
//!    submodule will look a fair bit different internally — but the
//!    *shape* it hands back to `minecraft.rs` (a synthetic version
//!    directory) should be the same.
//! 2. Add the variant to `ModLoader` and a match arm in each of
//!    `install`, `launch`, and `is_installed` below.

mod fabric;
mod forge;
mod neoforge;
mod quilt;

use crate::minecraft::{self, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModLoader {
    Vanilla,
    Fabric,
    Forge,
    NeoForge,
    Quilt,
}

impl ModLoader {
    /// Every variant, in the order the GUI's combo box already lists
    /// them.
    pub const ALL: [ModLoader; 5] = [
        ModLoader::Vanilla,
        ModLoader::Forge,
        ModLoader::Fabric,
        ModLoader::Quilt,
        ModLoader::NeoForge,
    ];

    /// The exact string the GUI's "Mod Loader" combo box uses for this
    /// loader.
    pub fn label(self) -> &'static str {
        match self {
            ModLoader::Vanilla => "Vanilla",
            ModLoader::Fabric => "Fabric",
            ModLoader::Forge => "Forge",
            ModLoader::NeoForge => "NeoForge",
            ModLoader::Quilt => "Quilt",
        }
    }

    pub fn from_label(label: &str) -> Option<ModLoader> {
        ModLoader::ALL.into_iter().find(|l| l.label() == label)
    }
}

/// Installs `mc_version` under the given loader. For `Vanilla` this is
/// exactly `minecraft::install_version`; other loaders install the
/// vanilla version first (if it isn't already) and then layer their own
/// libraries/main-class on top of it.
pub fn install(mc_version: &str, loader: ModLoader) -> Result<()> {
    match loader {
        ModLoader::Vanilla => minecraft::install_version(mc_version),
        ModLoader::Fabric => fabric::install(mc_version),
        ModLoader::Forge => forge::install(mc_version),
        ModLoader::NeoForge => neoforge::install(mc_version),
        ModLoader::Quilt => quilt::install(mc_version),
    }
}

/// Installs an exact loader version when a pack manifest pins one.
pub fn install_version(
    mc_version: &str,
    loader: ModLoader,
    loader_version: Option<&str>,
) -> Result<()> {
    let Some(loader_version) = loader_version else {
        return install(mc_version, loader);
    };
    match loader {
        ModLoader::Vanilla => minecraft::install_version(mc_version),
        ModLoader::Fabric => fabric::install_version(mc_version, Some(loader_version)),
        ModLoader::Forge => forge::install_version(mc_version, Some(loader_version)),
        ModLoader::NeoForge => neoforge::install_version(mc_version, Some(loader_version)),
        ModLoader::Quilt => quilt::install_version(mc_version, Some(loader_version)),
    }
}

/// Explicit offline compatibility wrapper using placeholder credentials.
/// Account-aware callers should use `launch_authenticated`.
/// Launches `mc_version` under the given loader using the shared Minecraft
/// directory as the game's directory, preserving the original behavior.
pub fn launch(mc_version: &str, loader: ModLoader) -> Result<()> {
    launch_in_directory(mc_version, loader, &minecraft::base_dir())
}

/// Explicit offline compatibility wrapper; use `launch_authenticated` for accounts.
/// Launches `mc_version` under the given loader with per-instance saves,
/// configuration, mods, and logs rooted at `game_dir`. Installed versions,
/// libraries, assets, and natives continue to come from shared storage.
pub fn launch_in_directory(mc_version: &str, loader: ModLoader, game_dir: &Path) -> Result<()> {
    let version = prepare_launch_version(mc_version, loader)?;
    minecraft::launch_version_in_directory(&version, game_dir)
}

/// Launches with an authenticated Microsoft account and per-instance game data.
/// Expired sessions fail before loader preparation; there is no offline fallback.
/// Installed versions, libraries, assets, and natives remain in shared storage.
pub fn launch_authenticated(
    mc_version: &str,
    loader: ModLoader,
    game_dir: &Path,
    account: &crate::auth::Account,
) -> Result<()> {
    if account.is_expired() {
        return Err(minecraft::FerriteError::AuthenticationExpired);
    }
    let version = prepare_launch_version(mc_version, loader)?;
    minecraft::launch_authenticated(&version, game_dir, account)
}

fn prepare_launch_version(mc_version: &str, loader: ModLoader) -> Result<String> {
    let composite_id = match loader {
        ModLoader::Vanilla => return Ok(mc_version.to_string()),
        ModLoader::Fabric => fabric::installed_composite_id(mc_version)?,
        ModLoader::Forge => forge::installed_composite_id(mc_version)?,
        ModLoader::NeoForge => neoforge::installed_composite_id(mc_version)?,
        ModLoader::Quilt => quilt::installed_composite_id(mc_version)?,
    };
    minecraft::copy_natives(mc_version, &composite_id)?;
    Ok(composite_id)
}

/// Returns `true` if `mc_version` is installed under the given loader.
pub fn is_installed(mc_version: &str, loader: ModLoader) -> bool {
    match loader {
        ModLoader::Vanilla => minecraft::is_version_installed(mc_version),
        ModLoader::Fabric => fabric::installed_composite_id(mc_version)
            .map(|id| minecraft::is_version_installed(&id))
            .unwrap_or(false),
        ModLoader::Forge => forge::installed_composite_id(mc_version)
            .map(|id| minecraft::is_version_installed(&id))
            .unwrap_or(false),
        ModLoader::NeoForge => neoforge::installed_composite_id(mc_version)
            .map(|id| minecraft::is_version_installed(&id))
            .unwrap_or(false),
        ModLoader::Quilt => quilt::installed_composite_id(mc_version)
            .map(|id| minecraft::is_version_installed(&id))
            .unwrap_or(false),
    }
}

/// Reads the exact installed loader version from the synthetic version metadata.
/// This is used for portable pack manifests and never performs a network request.
pub fn installed_loader_version(mc_version: &str, loader: ModLoader) -> Option<String> {
    let composite_id = match loader {
        ModLoader::Vanilla => return None,
        ModLoader::Fabric => fabric::installed_composite_id(mc_version).ok()?,
        ModLoader::Forge => forge::installed_composite_id(mc_version).ok()?,
        ModLoader::NeoForge => neoforge::installed_composite_id(mc_version).ok()?,
        ModLoader::Quilt => quilt::installed_composite_id(mc_version).ok()?,
    };
    let metadata = std::fs::read_to_string(
        minecraft::version_dir(&composite_id).join(format!("{composite_id}.json")),
    )
    .ok()?;
    let metadata: serde_json::Value = serde_json::from_str(&metadata).ok()?;
    loader_version_from_metadata(mc_version, loader, &metadata)
}

fn loader_version_from_metadata(
    mc_version: &str,
    loader: ModLoader,
    metadata: &serde_json::Value,
) -> Option<String> {
    let coordinates: &[&str] = match loader {
        ModLoader::Vanilla => return None,
        ModLoader::Fabric => &["net.fabricmc:fabric-loader:"],
        ModLoader::Forge => &["net.minecraftforge:forge:"],
        ModLoader::NeoForge => &["net.neoforged:neoforge:", "net.neoforged:forge:"],
        ModLoader::Quilt => &["org.quiltmc:quilt-loader:"],
    };
    metadata
        .get("libraries")?
        .as_array()?
        .iter()
        .filter_map(|library| library.get("name").and_then(|name| name.as_str()))
        .find_map(|name| {
            coordinates.iter().find_map(|prefix| {
                name.strip_prefix(prefix).map(|version| {
                    let version = version.split(':').next().unwrap_or(version);
                    version
                        .strip_prefix(&format!("{mc_version}-"))
                        .unwrap_or(version)
                        .to_owned()
                })
            })
        })
        .filter(|version| !version.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_versions_are_read_from_library_coordinates() {
        let forge = serde_json::json!({
            "libraries": [{ "name": "net.minecraftforge:forge:1.21.1-52.0.4:universal" }]
        });
        assert_eq!(
            loader_version_from_metadata("1.21.1", ModLoader::Forge, &forge).as_deref(),
            Some("52.0.4")
        );
        let fabric = serde_json::json!({
            "libraries": [{ "name": "net.fabricmc:fabric-loader:0.16.10" }]
        });
        assert_eq!(
            loader_version_from_metadata("1.21.1", ModLoader::Fabric, &fabric).as_deref(),
            Some("0.16.10")
        );
    }
}
