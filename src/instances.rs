//! Persistent launcher instance profiles.
//!
//! Installed Minecraft versions, libraries, and assets remain in Ferrite's
//! shared `minecraft` directory. Each instance receives a separate game
//! directory for its worlds, mods, configuration, logs, and resource packs.
//! The small `instances.json` file records the profiles between launcher runs.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Errors produced while reading or writing the instance store.
#[derive(Debug)]
pub enum InstanceError {
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for InstanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "instance filesystem error: {error}"),
            Self::Json(error) => write!(f, "invalid instance metadata: {error}"),
        }
    }
}

impl std::error::Error for InstanceError {}

impl From<io::Error> for InstanceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for InstanceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// A saved Minecraft profile and the directory containing its game data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceProfile {
    /// The unique, user-facing profile name.
    pub name: String,
    /// The base Minecraft version, such as `1.21.1`.
    pub version: String,
    /// The serialized display label of the selected mod loader.
    pub loader: String,
    /// A safe directory name generated when the profile is created.
    directory: String,
}

impl InstanceProfile {
    /// Creates metadata for a profile without writing it to disk.
    ///
    /// The generated directory is based on the profile name and receives a
    /// numeric suffix if that directory identifier is already in use.
    pub fn new(name: String, version: String, loader: String, existing: &[Self]) -> Self {
        let base = directory_slug(&name);
        let mut directory = base.clone();
        let mut suffix = 2;
        while existing
            .iter()
            .any(|profile| profile.directory == directory)
        {
            directory = format!("{base}-{suffix}");
            suffix += 1;
        }

        Self {
            name,
            version,
            loader,
            directory,
        }
    }

    /// Returns this profile's isolated Minecraft game directory.
    pub fn game_dir(&self) -> PathBuf {
        instances_dir().join(&self.directory)
    }
}

/// Loads every saved profile.
///
/// A missing store is treated as a new launcher installation. Invalid JSON is
/// returned as an error rather than silently discarding user profiles.
pub fn load() -> Result<Vec<InstanceProfile>, InstanceError> {
    let path = store_path();
    match fs::read_to_string(path) {
        Ok(json) => Ok(serde_json::from_str(&json)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

/// Atomically saves all profile metadata to `minecraft/instances.json`.
///
/// Data is first written to a temporary sibling file and then renamed, which
/// avoids leaving a partially written JSON document if writing fails.
pub fn save(profiles: &[InstanceProfile]) -> Result<(), InstanceError> {
    let path = store_path();
    let parent = path
        .parent()
        .expect("the instance store always has a parent");
    fs::create_dir_all(parent)?;

    let temporary = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(profiles)?;
    fs::write(&temporary, json)?;
    fs::rename(temporary, path)?;
    Ok(())
}

/// Creates the game-data directory belonging to `profile`.
pub fn create_game_dir(profile: &InstanceProfile) -> Result<(), InstanceError> {
    fs::create_dir_all(profile.game_dir())?;
    Ok(())
}

/// Deletes an instance's game data if it exists.
///
/// Call this only after its metadata has successfully been removed from the
/// saved profile list, so a failed metadata update never leaves a listed
/// profile with its files unexpectedly deleted.
pub fn delete_game_dir(profile: &InstanceProfile) -> Result<(), InstanceError> {
    let path = profile.game_dir();
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn store_path() -> PathBuf {
    Path::new("minecraft").join("instances.json")
}

fn instances_dir() -> PathBuf {
    Path::new("minecraft").join("instances")
}

/// Converts a display name into a safe, portable directory component.
fn directory_slug(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();

    if slug.is_empty() {
        "instance".to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::directory_slug;

    #[test]
    fn directory_slugs_do_not_contain_path_separators() {
        assert_eq!(
            directory_slug("My ../ Fabric Profile"),
            "my-----fabric-profile"
        );
        assert_eq!(directory_slug("测试"), "instance");
    }
}
