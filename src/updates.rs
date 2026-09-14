use std::error::Error;
use std::fmt;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::{StatusCode, Url};
use semver::Version;
use serde::Deserialize;

const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/Ontogameing/ferrite-launcher/releases/latest";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub current_version: Version,
    pub latest_version: Version,
    pub release_url: String,
    pub release_name: Option<String>,
}

#[derive(Clone, Debug)]
pub enum UpdateCheck {
    UpToDate {
        current_version: Version,
        latest_version: Version,
    },
    Available(UpdateInfo),
}

#[derive(Debug)]
pub enum UpdateError {
    Network(reqwest::Error),
    Http(StatusCode),
    Json(serde_json::Error),
    InvalidCurrentVersion {
        version: String,
        source: semver::Error,
    },
    InvalidReleaseTag {
        tag: String,
        source: semver::Error,
    },
    InvalidReleaseUrl {
        url: String,
        reason: &'static str,
    },
}

impl fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(error) => write!(formatter, "update request failed: {error}"),
            Self::Http(status) => write!(
                formatter,
                "GitHub returned HTTP status {status} while checking for updates"
            ),
            Self::Json(error) => write!(formatter, "invalid GitHub release response: {error}"),
            Self::InvalidCurrentVersion { version, .. } => {
                write!(formatter, "invalid current application version {version:?}")
            }
            Self::InvalidReleaseTag { tag, .. } => {
                write!(formatter, "invalid GitHub release tag {tag:?}")
            }
            Self::InvalidReleaseUrl { url, reason } => {
                write!(formatter, "invalid GitHub release URL {url:?}: {reason}")
            }
        }
    }
}

impl Error for UpdateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Network(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::InvalidCurrentVersion { source, .. } | Self::InvalidReleaseTag { source, .. } => {
                Some(source)
            }
            Self::Http(_) | Self::InvalidReleaseUrl { .. } => None,
        }
    }
}

#[derive(Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    html_url: String,
    name: Option<String>,
}

pub fn check_for_update() -> Result<UpdateCheck, UpdateError> {
    let current_version_text = env!("CARGO_PKG_VERSION");
    let current_version = Version::parse(current_version_text).map_err(|source| {
        UpdateError::InvalidCurrentVersion {
            version: current_version_text.to_owned(),
            source,
        }
    })?;

    let user_agent = format!("ferrite-launcher/{current_version}");
    let client = Client::builder()
        .user_agent(user_agent)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(UpdateError::Network)?;

    let response = client
        .get(LATEST_RELEASE_URL)
        .send()
        .map_err(UpdateError::Network)?;

    let status = response.status();
    if !status.is_success() {
        return Err(UpdateError::Http(status));
    }

    let response_text = response.text().map_err(UpdateError::Network)?;
    evaluate_response(current_version, &response_text)
}

fn evaluate_response(
    current_version: Version,
    response_text: &str,
) -> Result<UpdateCheck, UpdateError> {
    let release: ReleaseResponse =
        serde_json::from_str(response_text).map_err(UpdateError::Json)?;
    evaluate_release(current_version, release)
}

fn evaluate_release(
    current_version: Version,
    release: ReleaseResponse,
) -> Result<UpdateCheck, UpdateError> {
    let version_text = release
        .tag_name
        .strip_prefix(['v', 'V'])
        .unwrap_or(&release.tag_name);
    let latest_version =
        Version::parse(version_text).map_err(|source| UpdateError::InvalidReleaseTag {
            tag: release.tag_name.clone(),
            source,
        })?;

    validate_release_url(&release.html_url)?;

    if latest_version > current_version {
        Ok(UpdateCheck::Available(UpdateInfo {
            current_version,
            latest_version,
            release_url: release.html_url,
            release_name: release.name,
        }))
    } else {
        Ok(UpdateCheck::UpToDate {
            current_version,
            latest_version,
        })
    }
}

fn validate_release_url(release_url: &str) -> Result<(), UpdateError> {
    let parsed = Url::parse(release_url).map_err(|_| UpdateError::InvalidReleaseUrl {
        url: release_url.to_owned(),
        reason: "URL could not be parsed",
    })?;

    if parsed.scheme() != "https" {
        return Err(UpdateError::InvalidReleaseUrl {
            url: release_url.to_owned(),
            reason: "URL must use HTTPS",
        });
    }

    if parsed.host_str() != Some("github.com") {
        return Err(UpdateError::InvalidReleaseUrl {
            url: release_url.to_owned(),
            reason: "URL host must be github.com",
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(tag: &str) -> String {
        serde_json::json!({
            "tag_name": tag,
            "html_url": "https://github.com/Ontogameing/ferrite-launcher/releases/tag/test",
            "name": "Test release",
            "ignored_field": "ignored"
        })
        .to_string()
    }

    fn evaluate(current: &str, tag: &str) -> Result<UpdateCheck, UpdateError> {
        evaluate_response(Version::parse(current).unwrap(), &response(tag))
    }

    #[test]
    fn accepts_lowercase_v_prefix() {
        let result = evaluate("0.1.0", "v0.2.0").unwrap();
        assert!(matches!(
            result,
            UpdateCheck::Available(UpdateInfo { latest_version, .. })
                if latest_version == Version::new(0, 2, 0)
        ));
    }

    #[test]
    fn accepts_uppercase_v_prefix() {
        let result = evaluate("0.1.0", "V0.2.0").unwrap();
        assert!(matches!(result, UpdateCheck::Available(_)));
    }

    #[test]
    fn stable_release_is_newer_than_prerelease() {
        let result = evaluate("0.1.0-alpha", "0.1.0").unwrap();
        assert!(matches!(result, UpdateCheck::Available(_)));
    }

    #[test]
    fn newer_patch_is_available() {
        let result = evaluate("0.1.0", "0.1.1").unwrap();
        assert!(matches!(result, UpdateCheck::Available(_)));
    }

    #[test]
    fn equal_version_is_up_to_date() {
        let result = evaluate("0.1.0", "0.1.0").unwrap();
        assert!(matches!(result, UpdateCheck::UpToDate { .. }));
    }

    #[test]
    fn older_version_is_up_to_date() {
        let result = evaluate("0.2.0", "0.1.9").unwrap();
        assert!(matches!(result, UpdateCheck::UpToDate { .. }));
    }

    #[test]
    fn malformed_release_tag_is_rejected() {
        let error = evaluate("0.1.0", "vnot-a-version").unwrap_err();
        assert!(matches!(error, UpdateError::InvalidReleaseTag { .. }));
    }
}
