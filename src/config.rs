//! Persistent, non-secret launcher preferences.
//!
//! [`config_path`] uses [`directories::ProjectDirs`] to select the platform-specific
//! per-user configuration directory and appends `config.toml`. The entire [`Config`]
//! is serialized as human-readable TOML; missing tables and fields inherit defaults
//! through Serde's `default` handling, while known values are semantically validated.
//!
//! [`load`] is strict. Startup code can instead use [`load_or_create`], which creates
//! a default file when none exists and recovers from syntactically malformed or
//! type-invalid TOML by returning defaults plus a warning. Files that parse but fail
//! semantic validation, filesystem failures, and unavailable platform directories are
//! still returned as errors so callers can decide whether an in-memory fallback is safe.
//!
//! This module stores preferences only. Credentials and access tokens do not belong in
//! [`Config`] or in raw TOML supplied to [`save_toml`].

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

const QUALIFIER: &str = "io";
const ORGANIZATION: &str = "Ferrite";
const APPLICATION: &str = "Ferrite Launcher";
const FILE_NAME: &str = "config.toml";

/// Complete on-disk configuration.
///
/// Deserializing a partial file fills absent sections and fields from [`Default`],
/// which allows newer versions to add settings without requiring a migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub appearance: AppearanceConfig,
    pub launcher: LauncherConfig,
    pub discord: DiscordConfig,
    pub minecraft: MinecraftConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            appearance: AppearanceConfig::default(),
            launcher: LauncherConfig::default(),
            discord: DiscordConfig::default(),
            minecraft: MinecraftConfig::default(),
        }
    }
}

impl Config {
    fn validate(&self) -> Result<(), ConfigError> {
        if !matches!(self.appearance.theme.as_str(), "dark" | "light") {
            return Err(ConfigError::Validation(
                "appearance.theme must be 'dark' or 'light'".to_owned(),
            ));
        }
        let accent = self.appearance.accent.as_bytes();
        if accent.len() != 7 || accent[0] != b'#' || !accent[1..].iter().all(u8::is_ascii_hexdigit)
        {
            return Err(ConfigError::Validation(
                "appearance.accent must be a color in #RRGGBB format".to_owned(),
            ));
        }
        if !(0.5..=2.0).contains(&self.appearance.font_scale) {
            return Err(ConfigError::Validation(
                "appearance.font_scale must be between 0.5 and 2.0".to_owned(),
            ));
        }
        if self.appearance.corner_radius > 32 {
            return Err(ConfigError::Validation(
                "appearance.corner_radius must be between 0 and 32".to_owned(),
            ));
        }
        if !(512..=32_768).contains(&self.minecraft.default_memory_mb) {
            return Err(ConfigError::Validation(
                "minecraft.default_memory_mb must be between 512 and 32768".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Visual preferences applied by the launcher UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppearanceConfig {
    pub theme: String,
    pub accent: String,
    pub font_scale: f32,
    pub corner_radius: u8,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: "dark".to_owned(),
            accent: "#ff6600".to_owned(),
            font_scale: 1.0,
            corner_radius: 8,
        }
    }
}

/// General launcher behavior and update-check preferences.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LauncherConfig {
    pub close_on_launch: bool,
    pub show_snapshots: bool,
    pub check_for_updates: bool,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            close_on_launch: false,
            show_snapshots: false,
            check_for_updates: true,
        }
    }
}

/// Discord integration preferences; no Discord credentials are stored here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DiscordConfig {
    pub rich_presence: bool,
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            rich_presence: true,
        }
    }
}

/// Defaults used when launching Minecraft instances.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MinecraftConfig {
    pub default_memory_mb: u32,
}

impl Default for MinecraftConfig {
    fn default() -> Self {
        Self {
            default_memory_mb: 4096,
        }
    }
}

/// Failure while locating, decoding, validating, writing, or revealing configuration.
///
/// Parse and serialization variants retain their typed errors for formatting or direct
/// pattern matching. Because this type does not override [`std::error::Error::source`],
/// callers cannot traverse those values through Rust's standard error-source chain.
/// Displayed messages may include filesystem paths or TOML locations, but this module
/// never intentionally places configuration contents in an error.
#[derive(Debug)]
pub enum ConfigError {
    ConfigDirectoryUnavailable,
    Io(io::Error),
    Deserialize(toml::de::Error),
    Serialize(toml::ser::Error),
    Validation(String),
    OpenFolder(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigDirectoryUnavailable => {
                write!(
                    formatter,
                    "the operating system configuration directory is unavailable"
                )
            }
            Self::Io(error) => write!(formatter, "configuration filesystem error: {error}"),
            Self::Deserialize(error) => write!(formatter, "invalid configuration TOML: {error}"),
            Self::Serialize(error) => {
                write!(formatter, "could not serialize configuration: {error}")
            }
            Self::Validation(error) => write!(formatter, "invalid configuration value: {error}"),
            Self::OpenFolder(error) => write!(formatter, "could not open config folder: {error}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(error: toml::de::Error) -> Self {
        Self::Deserialize(error)
    }
}

impl From<toml::ser::Error> for ConfigError {
    fn from(error: toml::ser::Error) -> Self {
        Self::Serialize(error)
    }
}

/// Result of startup-oriented loading.
///
/// `warning` is populated when an existing file could not be deserialized and the
/// returned configuration is therefore an in-memory default. The invalid file is
/// left untouched until the caller explicitly saves a setting.
pub struct ConfigLoad {
    /// Configuration to apply for this process.
    pub config: Config,
    /// User-facing explanation of a recoverable fallback, if one occurred.
    pub warning: Option<String>,
}

/// Strictly reads, parses, and validates Ferrite's configuration from disk.
///
/// Unlike [`load_or_create`], a missing or malformed file is returned as an error and
/// no filesystem state is changed.
pub fn load() -> Result<Config, ConfigError> {
    parse_toml(&read_toml()?)
}

/// Reads the config file as UTF-8 text without parsing or validating it.
///
/// This performs filesystem I/O only and is useful for displaying the exact source
/// in an advanced editor.
pub fn read_toml() -> Result<String, ConfigError> {
    Ok(fs::read_to_string(config_path()?)?)
}

/// Parses and validates TOML, applying defaults for missing sections and fields.
///
/// Unknown fields follow Serde's normal behavior and are ignored. No disk I/O occurs.
pub fn parse_toml(input: &str) -> Result<Config, ConfigError> {
    let config: Config = toml::from_str(input)?;
    config.validate()?;
    Ok(config)
}

/// Serializes a complete configuration as human-readable TOML without writing it.
///
/// This does not revalidate a programmatically constructed [`Config`].
pub fn to_toml(config: &Config) -> Result<String, ConfigError> {
    Ok(toml::to_string_pretty(config)?)
}

/// Parses, validates, and saves TOML, returning the normalized configuration.
///
/// Parsing and validation happen before any write, so those failures leave the
/// existing file untouched. Successful output is formatted by [`to_toml`] rather
/// than preserving the input's comments or whitespace.
pub fn save_toml(input: &str) -> Result<Config, ConfigError> {
    let config = parse_toml(input)?;
    save(&config)?;
    Ok(config)
}

/// Loads configuration for startup, creating a default file when none exists.
///
/// Deserialization failures (including malformed TOML and field type mismatches)
/// return defaults with a warning and preserve the bad file for inspection. Semantic
/// validation and I/O failures remain errors. Creating a missing file also creates
/// its parent directory and can therefore fail.
pub fn load_or_create() -> Result<ConfigLoad, ConfigError> {
    let path = config_path()?;
    match read_toml() {
        Ok(input) => match parse_toml(&input) {
            Ok(config) => Ok(ConfigLoad {
                config,
                warning: None,
            }),
            Err(ConfigError::Deserialize(error)) => Ok(ConfigLoad {
                config: Config::default(),
                warning: Some(format!(
                    "Could not read {}: {error}. Using defaults; changing a setting will replace the invalid file.",
                    path.display()
                )),
            }),
            Err(error) => Err(error),
        },
        Err(ConfigError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            let config = Config::default();
            save(&config)?;
            Ok(ConfigLoad {
                config,
                warning: None,
            })
        }
        Err(error) => Err(error),
    }
}

/// Saves the complete configuration as human-readable TOML.
///
/// The parent directory is created as needed. Data is first written to a sibling
/// temporary file, then renamed into place to avoid exposing a partially written
/// TOML file. If the first rename fails while a destination exists, the implementation
/// removes that destination and retries on every platform. That fallback is not atomic:
/// a second rename failure can leave no active configuration file.
///
/// This function serializes the supplied value but does not call semantic validation.
pub fn save(config: &Config) -> Result<(), ConfigError> {
    let path = config_path()?;
    let parent = path
        .parent()
        .expect("the configuration file always has a parent");
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, to_toml(config)?)?;

    // Prefer replacement by rename. The remove-and-retry fallback handles platforms
    // that reject replacing an existing file, but sacrifices atomic replacement.
    if let Err(error) = fs::rename(&temporary, &path) {
        if path.exists() {
            fs::remove_file(&path)?;
            fs::rename(&temporary, &path)?;
        } else {
            return Err(error.into());
        }
    }
    Ok(())
}

/// Opens the directory containing Ferrite's configuration file.
///
/// Creates the directory first, then spawns the platform file browser (`explorer`,
/// `open`, or `xdg-open`). Success means the process was launched; it does not wait
/// for the browser or prove that a window became visible.
pub fn open_config_folder() -> Result<(), ConfigError> {
    let path = config_path()?;
    let folder = path
        .parent()
        .expect("the configuration file always has a parent");
    fs::create_dir_all(folder)?;

    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    #[cfg(not(any(target_os = "windows", unix)))]
    return Err(ConfigError::OpenFolder(
        "opening folders is unsupported on this platform".to_owned(),
    ));

    Command::new(program)
        .arg(folder)
        .spawn()
        .map_err(|error| ConfigError::OpenFolder(format!("failed to launch {program}: {error}")))?;
    Ok(())
}

/// Returns the platform-specific path to Ferrite's `config.toml`.
///
/// This is a pure path lookup: it neither creates the directory nor checks that the
/// file exists. It fails on platforms where a user configuration directory cannot
/// be determined.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    let directories = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .ok_or(ConfigError::ConfigDirectoryUnavailable)?;
    Ok(directories.config_dir().join(FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_public_configuration_contract() {
        let config = Config::default();
        assert_eq!(config.appearance.theme, "dark");
        assert_eq!(config.appearance.accent, "#ff6600");
        assert_eq!(config.appearance.font_scale, 1.0);
        assert_eq!(config.appearance.corner_radius, 8);
        assert!(!config.launcher.close_on_launch);
        assert!(!config.launcher.show_snapshots);
        assert!(config.launcher.check_for_updates);
        assert!(config.discord.rich_presence);
        assert_eq!(config.minecraft.default_memory_mb, 4096);
    }

    #[test]
    fn missing_fields_receive_defaults() {
        let config = parse_toml("[discord]\nrich_presence = false\n").unwrap();
        assert!(!config.discord.rich_presence);
        assert_eq!(config.appearance, AppearanceConfig::default());
        assert_eq!(config.launcher, LauncherConfig::default());
        assert_eq!(config.minecraft, MinecraftConfig::default());
    }

    #[test]
    fn invalid_toml_does_not_produce_a_config() {
        assert!(matches!(
            parse_toml("[appearance\ntheme = 42"),
            Err(ConfigError::Deserialize(_))
        ));
    }

    #[test]
    fn raw_toml_round_trips() {
        let input = "[appearance]\ntheme = \"light\"\n\n[launcher]\nclose_on_launch = true\n";
        let config = parse_toml(input).unwrap();
        let encoded = to_toml(&config).unwrap();
        let decoded = parse_toml(&encoded).unwrap();

        assert_eq!(decoded, config);
        assert_eq!(decoded.appearance.theme, "light");
        assert!(decoded.launcher.close_on_launch);
    }

    #[test]
    fn default_config_round_trips_through_toml() {
        let encoded = to_toml(&Config::default()).unwrap();
        let decoded = parse_toml(&encoded).unwrap();
        assert_eq!(decoded, Config::default());
    }

    #[test]
    fn semantic_validation_rejects_unsupported_values() {
        for invalid in [
            "[appearance]\ntheme = 'neon'\n",
            "[appearance]\naccent = 'orange'\n",
            "[appearance]\nfont_scale = 9.0\n",
            "[minecraft]\ndefault_memory_mb = 64\n",
        ] {
            assert!(matches!(
                parse_toml(invalid),
                Err(ConfigError::Validation(_))
            ));
        }
    }
}
