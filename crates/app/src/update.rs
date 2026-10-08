//! Update check against the GitHub releases of [`build_info::REPO`]. Stable builds compare
//! with the latest stable release, nightly builds with the newest pre-release, and dev
//! builds never check.

use std::time::Duration;

use anyhow::Context as _;
use lanlink_core::build_info;
use serde::Deserialize;

/// A release newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The release page on GitHub.
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

/// Whether this build checks for updates at all (dev builds don't).
pub fn supported() -> bool {
    matches!(build_info::CHANNEL, "stable" | "nightly")
}

/// Ask GitHub for the newest release on this build's channel. `None` means up to date.
pub async fn check() -> anyhow::Result<Option<Release>> {
    // reqwest is built without a bundled crypto provider; use ring, which iroh already
    // links. Fails harmlessly when a provider is already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .user_agent(format!("lanlink/{}", build_info::VERSION))
        .timeout(Duration::from_secs(20))
        .build()?;
    let repo = build_info::REPO;
    let url = if build_info::CHANNEL == "stable" {
        format!("https://api.github.com/repos/{repo}/releases/latest")
    } else {
        format!("https://api.github.com/repos/{repo}/releases?per_page=30")
    };
    let body = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let releases = if build_info::CHANNEL == "stable" {
        vec![serde_json::from_slice::<GhRelease>(&body).context("parsing latest release")?]
    } else {
        serde_json::from_slice::<Vec<GhRelease>>(&body).context("parsing releases")?
    };
    Ok(newer(build_info::VERSION, build_info::CHANNEL, releases))
}

/// The newest release on `channel` that is newer than `current`, if any.
fn newer(current: &str, channel: &str, releases: Vec<GhRelease>) -> Option<Release> {
    let current = semver::Version::parse(current).ok()?;
    let want_prerelease = channel == "nightly";
    releases
        .into_iter()
        .filter(|r| !r.draft && r.prerelease == want_prerelease)
        .filter_map(|r| {
            let v = semver::Version::parse(r.tag_name.trim_start_matches('v')).ok()?;
            Some((v, r.html_url))
        })
        .filter(|(v, _)| *v > current)
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(v, url)| Release {
            version: v.to_string(),
            url,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, prerelease: bool) -> GhRelease {
        GhRelease {
            tag_name: tag.into(),
            html_url: format!("https://example.com/{tag}"),
            prerelease,
            draft: false,
        }
    }

    #[test]
    fn stable_sees_newer_stable_only() {
        let list = || {
            vec![
                rel("v0.3.0-nightly.20261010.20", true),
                rel("v0.2.0", false),
                rel("v0.1.0", false),
            ]
        };
        let r = newer("0.1.0", "stable", list()).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.url, "https://example.com/v0.2.0");
        assert_eq!(newer("0.2.0", "stable", list()), None);
    }

    #[test]
    fn nightly_picks_newest_prerelease() {
        let list = vec![
            rel("v0.1.0-nightly.20261008.12", true),
            rel("v0.2.0-nightly.20261010.14", true),
            rel("v0.1.0-nightly.20261009.13", true),
            rel("v0.5.0", false),
        ];
        let r = newer("0.1.0-nightly.20261008.12", "nightly", list).unwrap();
        assert_eq!(r.version, "0.2.0-nightly.20261010.14");
    }

    #[test]
    fn same_or_older_nightly_is_up_to_date() {
        let list = vec![rel("v0.1.0-nightly.20261009.13", true)];
        assert_eq!(newer("0.1.0-nightly.20261009.13", "nightly", list), None);
        let list = vec![rel("v0.1.0-nightly.20261009.13", true)];
        assert_eq!(newer("0.1.0-nightly.20261010.14", "nightly", list), None);
    }

    #[test]
    fn ignores_drafts_and_bad_tags() {
        let mut draft = rel("v9.0.0", false);
        draft.draft = true;
        let list = vec![draft, rel("latest", false), rel("v0.1.1", false)];
        assert_eq!(newer("0.1.0", "stable", list).unwrap().version, "0.1.1");
        assert_eq!(
            newer("not-a-version", "stable", vec![rel("v1.0.0", false)]),
            None
        );
    }
}
