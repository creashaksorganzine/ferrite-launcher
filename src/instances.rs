//! Persistence and filesystem layout for launcher instances.
//!
//! Ferrite keeps downloaded Minecraft versions, libraries, and assets in its
//! shared `minecraft` tree, while each [`InstanceProfile`] points at an isolated
//! `minecraft/instances/<directory>` game directory. Worlds, mods, configuration,
//! logs, and resource packs therefore belong to one profile without duplicating
//! shared runtime files. All paths in this module are relative to the process's
//! current working directory.
//!
//! Profile metadata is serialized as one JSON array in
//! `minecraft/instances.json`. A missing file means no profiles; malformed JSON
//! and filesystem failures are surfaced to the caller instead of being replaced
//! with defaults. Saving uses a sibling temporary file followed by a rename, so
//! readers do not observe a partially serialized document. This is replacement
//! atomicity, not durable transactional storage: the code does not `fsync`, uses
//! a fixed temporary name, and inherits the host platform's rename semantics.
//!
//! Directory identifiers created here are portable single components, but loaded
//! metadata is deserialized as stored rather than revalidated. Callers should
//! treat the launcher-owned metadata file as trusted and should persist a newly
//! created profile before performing destructive operations on its game directory.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Errors produced while reading or writing the instance store.
#[derive(Debug)]
pub enum InstanceError {
    /// Creating, reading, renaming, or deleting launcher files failed.
    Io(io::Error),
    /// The profile array could not be serialized or deserialized.
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
    ///
    /// This module stores the value opaquely; loader selection and validation
    /// belong to the installation/launching layer.
    pub loader: String,
    /// The relative component under `minecraft/instances` used for game data.
    ///
    /// New profiles receive a sanitized value. The field stays private so normal
    /// callers cannot later redirect an instance, although Serde still restores
    /// the value verbatim from the trusted profile store.
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
        // Resolve collisions against stable directory identifiers rather than
        // display names: names may differ while producing the same slug.
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
    ///
    /// This only constructs a relative path; it neither creates nor canonicalizes
    /// the directory. Use [`create_game_dir`] when the directory must exist.
    pub fn game_dir(&self) -> PathBuf {
        instances_dir().join(&self.directory)
    }
}

/// Loads every saved profile.
///
/// A missing store is treated as a new launcher installation. Invalid JSON is
/// returned as an error rather than silently discarding user profiles. Serde
/// restores all fields verbatim; semantic checks such as version availability,
/// unique names, or directory-component validation are not performed here.
pub fn load() -> Result<Vec<InstanceProfile>, InstanceError> {
    let path = store_path();
    match fs::read_to_string(path) {
        Ok(json) => Ok(serde_json::from_str(&json)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

/// Saves all profile metadata to `minecraft/instances.json` via replacement.
///
/// The complete, pretty-printed JSON array is first written to the fixed sibling
/// `instances.json.tmp`, then renamed over the store. Keeping both files in one
/// directory gives filesystems that support replacement rename an atomic
/// old-or-new view and prevents a failed write from truncating the current store.
/// The operation is not safe for concurrent writers, does not synchronize data
/// to stable storage, and may fail when the platform cannot rename over an
/// existing destination; all such failures are returned as [`InstanceError`].
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

/// Creates the game-data directory belonging to `profile` and any missing parents.
///
/// Existing directories are accepted. Other filesystem conflicts and permission
/// failures are returned without changing profile metadata.
pub fn create_game_dir(profile: &InstanceProfile) -> Result<(), InstanceError> {
    fs::create_dir_all(profile.game_dir())?;
    Ok(())
}

/// Recursively deletes an instance's game-data path if it exists.
///
/// Call this only after its metadata has successfully been removed from the
/// saved profile list, so a failed metadata update never leaves a listed
/// profile with its files unexpectedly deleted. The existence check is only for
/// convenient missing-path handling, not a synchronization or security boundary;
/// deletion errors are returned to the caller.
pub fn delete_game_dir(profile: &InstanceProfile) -> Result<(), InstanceError> {
    let path = profile.game_dir();
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

/// Location of the JSON profile array, relative to the current working directory.
fn store_path() -> PathBuf {
    Path::new("minecraft").join("instances.json")
}

/// Parent for per-profile game directories, relative to the current working directory.
fn instances_dir() -> PathBuf {
    Path::new("minecraft").join("instances")
}

/// Converts a display name into a portable ASCII directory component.
///
/// ASCII letters are lowercased, digits, `-`, and `_` are preserved, and every
/// other Unicode scalar becomes `-`. Leading/trailing dashes are removed; an
/// empty result falls back to `instance`. Uniqueness is added separately by
/// [`InstanceProfile::new`].
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
