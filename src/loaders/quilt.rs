//! Quilt loader support.
//!
//! Quilt is a fork of Fabric and keeps essentially the same shape: a
//! meta API that hands back a ready-to-use "profile" JSON (main class +
//! libraries in Maven-coordinate form) for a given Minecraft version +
//! loader build. This module is just `fabric.rs`'s approach pointed at
//! Quilt's endpoints — see that module's docs for the full rationale
//! (merging into a synthetic vanilla-shaped version rather than
//! teaching `crate::minecraft` about loaders at all).
//!
//! After copying `client.jar`, we also `copy_natives` from the vanilla
//! version into the synthetic id — same reason as `fabric.rs`.

use crate::minecraft::{self, FerriteError, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

const META_BASE: &str = "https://meta.quiltmc.org/v3/versions/loader";

/// Installs the latest stable Quilt loader for `mc_version`, on top of
/// the vanilla install (installing that first if it isn't already
/// present).
pub fn install(mc_version: &str) -> Result<()> {
    install_version(mc_version, None)
}

/// Installs a requested Quilt version, or the latest stable build when omitted.
pub fn install_version(mc_version: &str, requested: Option<&str>) -> Result<()> {
    minecraft::install_version(mc_version)?;

    let client = Client::new();
    let loader_version = match requested.map(str::trim).filter(|value| !value.is_empty()) {
        Some(version) => version.to_owned(),
        None => latest_stable_loader_version(&client, mc_version)?,
    };
    let composite_id = composite_id(mc_version, &loader_version);

    println!("Fetching Quilt profile for loader {loader_version}...");
    let profile = fetch_profile(&client, mc_version, &loader_version)?;

    let vanilla_dir = minecraft::version_dir(mc_version);
    let vanilla_json: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        vanilla_dir.join(format!("{mc_version}.json")),
    )?)?;

    let merged = merge_metadata(&composite_id, &vanilla_json, &profile);

    let composite_dir = minecraft::version_dir(&composite_id);
    fs::create_dir_all(&composite_dir)?;
    fs::write(
        composite_dir.join(format!("{composite_id}.json")),
        serde_json::to_string(&merged)?,
    )?;
    fs::copy(
        vanilla_dir.join("client.jar"),
        composite_dir.join("client.jar"),
    )?;
    minecraft::copy_natives(mc_version, &composite_id)?;

    println!("Downloading Quilt loader libraries...");
    download_quilt_libraries(&client, &profile)?;

    fs::write(marker_path(mc_version), &composite_id)?;
    println!("Quilt {loader_version} installed for Minecraft {mc_version}.");
    Ok(())
}

/// The synthetic version id of the merged vanilla+Quilt install
/// currently recorded for `mc_version`. Errors with
/// `FerriteError::LoaderNotInstalled` if Quilt hasn't been installed
/// for it yet.
pub fn installed_composite_id(mc_version: &str) -> Result<String> {
    fs::read_to_string(marker_path(mc_version))
        .map_err(|_| FerriteError::LoaderNotInstalled(mc_version.to_string()))
}

fn marker_path(mc_version: &str) -> PathBuf {
    minecraft::version_dir(mc_version).join("quilt-loader.txt")
}

fn composite_id(mc_version: &str, loader_version: &str) -> String {
    format!("quilt-loader-{loader_version}-{mc_version}")
}

// ---------------------------------------------------------------------
// Quilt meta API
// ---------------------------------------------------------------------

#[derive(Deserialize)]
struct LoaderListEntry {
    loader: LoaderInfo,
}

#[derive(Deserialize)]
struct LoaderInfo {
    version: String,
    /// Present on Quilt's meta responses same as Fabric's, but kept
    /// `#[serde(default)]` in case a given build omits it.
    #[serde(default)]
    stable: bool,
}

fn latest_stable_loader_version(client: &Client, mc_version: &str) -> Result<String> {
    let url = format!("{META_BASE}/{mc_version}");
    let text = client.get(&url).send()?.error_for_status()?.text()?;

    let entries: Vec<LoaderListEntry> = serde_json::from_str(&text)?;
    // Quilt's meta API lists builds oldest-first. Prefer the last
    // `stable` entry; if none are marked stable, take the last entry
    // overall (the newest published build). Using `.first()` here is
    // what previously installed 0.20.0-beta.9 for 1.21.11, which
    // crashes on modern Java with LaunchClassLoader.
    entries
        .iter()
        .rev()
        .find(|e| e.loader.stable)
        .or_else(|| entries.last())
        .map(|e| e.loader.version.clone())
        .ok_or_else(|| FerriteError::LoaderVersionUnavailable(mc_version.to_string()))
}

fn fetch_profile(
    client: &Client,
    mc_version: &str,
    loader_version: &str,
) -> Result<serde_json::Value> {
    let url = format!("{META_BASE}/{mc_version}/{loader_version}/profile/json");
    let text = client.get(&url).send()?.error_for_status()?.text()?;
    Ok(serde_json::from_str(&text)?)
}

// ---------------------------------------------------------------------
// Metadata merging
// ---------------------------------------------------------------------

/// Builds a synthetic vanilla-shaped version metadata JSON: identical
/// to the vanilla version's own metadata, except for `id`, `mainClass`
/// (taken from Quilt's profile), and `libraries` / `arguments.game`
/// (vanilla's, with Quilt's appended).
fn merge_metadata(
    composite_id: &str,
    vanilla: &serde_json::Value,
    quilt_profile: &serde_json::Value,
) -> serde_json::Value {
    let mut merged = vanilla.clone();

    merged["id"] = serde_json::Value::String(composite_id.to_string());

    if let Some(main_class) = quilt_profile.get("mainClass") {
        merged["mainClass"] = main_class.clone();
    }

    let mut libraries = vanilla["libraries"].as_array().cloned().unwrap_or_default();
    if let Some(quilt_libs) = quilt_profile["libraries"].as_array() {
        for lib in quilt_libs {
            if let Some(converted) = convert_quilt_library(lib) {
                libraries.push(converted);
            }
        }
    }
    merged["libraries"] = serde_json::Value::Array(libraries);

    let mut game_args = vanilla["arguments"]["game"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(quilt_game) = quilt_profile["arguments"]["game"].as_array() {
        game_args.extend(quilt_game.iter().cloned());
    }
    merged["arguments"]["game"] = serde_json::Value::Array(game_args);

    // Knot also ships extra JVM args (e.g. --add-opens). Dropping them
    // is a common cause of Mixin/ServiceLoader crashes on modern Java.
    let mut jvm_args = vanilla["arguments"]["jvm"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(quilt_jvm) = quilt_profile["arguments"]["jvm"].as_array() {
        jvm_args.extend(quilt_jvm.iter().cloned());
    }
    if !jvm_args.is_empty() {
        merged["arguments"]["jvm"] = serde_json::Value::Array(jvm_args);
    }

    merged
}

/// Converts a Quilt-meta library entry
/// (`{"name": "group:artifact:version", "url": "https://repo/"}`) into
/// the `downloads.artifact.{path,url,size}` shape
/// `crate::minecraft`'s deserializer expects for every library.
fn convert_quilt_library(lib: &serde_json::Value) -> Option<serde_json::Value> {
    let name = lib.get("name")?.as_str()?;
    let repo = lib.get("url")?.as_str()?;
    let path = maven_coordinate_to_path(name)?;
    let url = format!("{}/{path}", repo.trim_end_matches('/'));

    Some(serde_json::json!({
        "name": name,
        "downloads": {
            "artifact": {
                "path": path,
                "url": url,
                // Quilt's meta API doesn't publish a size/sha1 for
                // these; `download_file` only warns (never fails) on a
                // size mismatch, so 0 here just disables that check.
                "size": 0
            }
        },
        "rules": []
    }))
}

fn maven_coordinate_to_path(coordinate: &str) -> Option<String> {
    // "group.id:artifact:version[:classifier]" ->
    // "group/id/artifact/version/artifact-version[-classifier].jar"
    let mut parts = coordinate.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next()?;
    let classifier = parts.next();

    let group_path = group.replace('.', "/");
    let file_name = match classifier {
        Some(c) => format!("{artifact}-{version}-{c}.jar"),
        None => format!("{artifact}-{version}.jar"),
    };
    Some(format!("{group_path}/{artifact}/{version}/{file_name}"))
}

// ---------------------------------------------------------------------
// Library download
// ---------------------------------------------------------------------

fn download_quilt_libraries(client: &Client, profile: &serde_json::Value) -> Result<()> {
    let libs_dir = minecraft::libraries_dir();
    fs::create_dir_all(&libs_dir)?;

    let Some(libraries) = profile["libraries"].as_array() else {
        return Ok(());
    };

    for lib in libraries {
        let (Some(name), Some(repo)) = (
            lib.get("name").and_then(|v| v.as_str()),
            lib.get("url").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let Some(path) = maven_coordinate_to_path(name) else {
            continue;
        };

        let dest = libs_dir.join(&path);
        if dest.exists() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        let url = format!("{}/{path}", repo.trim_end_matches('/'));
        minecraft::download_file(client, &url, &dest, None)?;
        println!("  quilt library: {name}");
    }

    Ok(())
}
