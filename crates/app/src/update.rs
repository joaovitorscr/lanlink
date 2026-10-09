//! Updates from the GitHub releases of [`build_info::REPO`]. Stable builds compare with the
//! latest stable release, nightly builds with the newest pre-release, and dev builds never
//! check (unless `LANLINK_UPDATE_TEST_CHANNEL` is set, see [`source`]). The user can pick
//! the other [`Channel`] in Settings; moving from nightly to stable offers the latest stable
//! even when it is older than the running nightly.
//!
//! Installing: [`plan`] picks the release asset for this platform, [`prepare`] downloads it
//! next to the config dir and checks it against the release's `SHA256SUMS`, and on macOS
//! stages the new `.app` beside the running one. [`commit`] then swaps the bundle (macOS)
//! and, once the app has shut down, [`relaunch`] runs the installer (Windows) or opens the
//! new bundle (macOS).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Context as _};
use lanlink_core::{build_info, Config};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// Checksums published with every release, one `<sha256>  <file name>` line per asset.
const SUMS_ASSET: &str = "SHA256SUMS";
/// Upper bounds on what we are willing to download.
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SUMS_BYTES: u64 = 64 * 1024;

/// A release newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The release page on GitHub.
    pub url: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
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
    #[serde(default)]
    assets: Vec<Asset>,
}

/// Where updates come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Nightly,
}

impl Channel {
    pub const ALL: [Channel; 2] = [Channel::Stable, Channel::Nightly];

    pub fn label(self) -> &'static str {
        match self {
            Channel::Stable => "Stable",
            Channel::Nightly => "Nightly",
        }
    }

    /// Lowercase, for running text: "new nightly releases".
    pub fn name(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Nightly => "nightly",
        }
    }
}

/// Channel and version to compare releases against, or `None` when this build doesn't
/// check. Dev builds can pretend to be an old build of a channel to exercise the update
/// flow: `LANLINK_UPDATE_TEST_CHANNEL=nightly|stable`, optionally with
/// `LANLINK_UPDATE_TEST_VERSION` (default 0.0.0). Release builds ignore both.
fn source() -> Option<(Channel, String)> {
    match build_info::CHANNEL {
        "stable" => Some((Channel::Stable, build_info::VERSION.to_string())),
        "nightly" => Some((Channel::Nightly, build_info::VERSION.to_string())),
        _ => {
            let channel = match std::env::var("LANLINK_UPDATE_TEST_CHANNEL").ok()?.as_str() {
                "stable" => Channel::Stable,
                "nightly" => Channel::Nightly,
                _ => return None,
            };
            let version =
                std::env::var("LANLINK_UPDATE_TEST_VERSION").unwrap_or_else(|_| "0.0.0".into());
            Some((channel, version))
        }
    }
}

/// Whether this build checks for updates at all (dev builds don't).
pub fn supported() -> bool {
    source().is_some()
}

/// The channel this build came from, used when the user hasn't picked one.
pub fn default_channel() -> Channel {
    source().map_or(Channel::Stable, |(c, _)| c)
}

fn client(timeout: Option<Duration>) -> anyhow::Result<reqwest::Client> {
    // reqwest is built without a bundled crypto provider; use ring, which iroh already
    // links. Fails harmlessly when a provider is already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = reqwest::Client::builder()
        .user_agent(format!("lanlink/{}", build_info::VERSION))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60));
    if let Some(t) = timeout {
        builder = builder.timeout(t);
    }
    Ok(builder.build()?)
}

/// Ask GitHub for the newest release on `channel`. `None` means up to date.
pub async fn check(channel: Channel) -> anyhow::Result<Option<Release>> {
    let Some((_, version)) = source() else {
        return Ok(None);
    };
    let client = client(Some(Duration::from_secs(20)))?;
    let repo = build_info::REPO;
    let url = if channel == Channel::Stable {
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
    let releases = if channel == Channel::Stable {
        vec![serde_json::from_slice::<GhRelease>(&body).context("parsing latest release")?]
    } else {
        serde_json::from_slice::<Vec<GhRelease>>(&body).context("parsing releases")?
    };
    Ok(newer(&version, channel, releases))
}

/// The newest release on `channel` that is newer than `current`, if any. On the stable
/// channel a pre-release `current` (a nightly) takes any other stable version, so
/// switching back to stable can step down to the latest stable release.
fn newer(current: &str, channel: Channel, releases: Vec<GhRelease>) -> Option<Release> {
    let current = semver::Version::parse(current).ok()?;
    let want_prerelease = channel == Channel::Nightly;
    let leaving_nightly = channel == Channel::Stable && !current.pre.is_empty();
    releases
        .into_iter()
        .filter(|r| !r.draft && r.prerelease == want_prerelease)
        .filter_map(|r| {
            let v = semver::Version::parse(r.tag_name.trim_start_matches('v')).ok()?;
            Some((v, r))
        })
        .filter(|(v, _)| *v > current || (leaving_nightly && *v != current))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(v, r)| Release {
            version: v.to_string(),
            url: r.html_url,
            assets: r.assets,
        })
}

// ---- what can be installed here ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Windows,
    Mac,
    Other,
}

fn os() -> Os {
    if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else {
        Os::Other
    }
}

/// How this copy of lanlink gets replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Installed by the Inno Setup installer (Windows): run the new installer.
    Installer,
    /// Running from this `.app` bundle (macOS): replace it.
    Bundle(PathBuf),
}

/// Where the running executable can be updated in place, if at all. Portable Windows
/// copies, bare binaries and apps run from a disk image are updated by hand.
pub fn install_target() -> Option<Target> {
    static TARGET: OnceLock<Option<Target>> = OnceLock::new();
    TARGET
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            match os() {
                Os::Windows => is_inno_install(&exe, |p| p.is_file()).then_some(Target::Installer),
                Os::Mac => {
                    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
                    app_bundle(&exe).map(Target::Bundle)
                }
                Os::Other => None,
            }
        })
        .clone()
}

/// Why [`install_target`] is `None`, for the settings pane.
pub fn manual_reason() -> &'static str {
    match os() {
        Os::Windows => "Only the installed version updates itself, not the portable zip",
        Os::Mac => "Only lanlink.app updates itself, after it is copied out of the disk image",
        Os::Other => "Not available on this platform",
    }
}

/// Inno Setup writes its uninstaller (`unins000.exe` + `.dat`) into the install folder,
/// so their presence next to the exe means the installer put it there. More reliable than
/// the uninstall registry key, which says where lanlink was installed, not where this
/// copy runs from.
fn is_inno_install(exe: &Path, exists: impl Fn(&Path) -> bool) -> bool {
    exe.parent()
        .is_some_and(|dir| exists(&dir.join("unins000.exe")) && exists(&dir.join("unins000.dat")))
}

/// The `.app` bundle containing `exe` (`<bundle>.app/Contents/MacOS/<exe>`), unless it
/// is read-only: mounted from a disk image or translocated by Gatekeeper.
fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    if macos.file_name()? != "MacOS"
        || contents.file_name()? != "Contents"
        || bundle.extension()? != "app"
    {
        return None;
    }
    let read_only = bundle.starts_with("/Volumes")
        || bundle
            .components()
            .any(|c| c.as_os_str() == "AppTranslocation");
    (!read_only).then(|| bundle.to_path_buf())
}

/// File name of the release asset for `os` / `arch` (`std::env::consts::ARCH`).
fn asset_name(version: &str, os: Os, arch: &str) -> Option<String> {
    match (os, arch) {
        (Os::Windows, "x86_64") => Some(format!("lanlink-{version}-windows-x64-setup.exe")),
        (Os::Mac, "aarch64") => Some(format!("lanlink-{version}-macos-arm64.dmg")),
        (Os::Mac, "x86_64") => Some(format!("lanlink-{version}-macos-x64.dmg")),
        _ => None,
    }
}

/// The asset this platform installs from. Nightlies have no Intel Mac disk image.
fn select_asset<'a>(release: &'a Release, os: Os, arch: &str) -> Option<&'a Asset> {
    let name = asset_name(&release.version, os, arch)?;
    release.assets.iter().find(|a| a.name == name)
}

/// Everything needed to download and install one release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub version: String,
    asset: Asset,
    /// `None` only with the dev override `LANLINK_UPDATE_TEST_SUMS`.
    sums: Option<Asset>,
    target: Target,
}

/// How to install `release` here, or `None` when the user has to download it by hand.
pub fn plan(release: &Release) -> Option<Plan> {
    plan_for(
        release,
        os(),
        std::env::consts::ARCH,
        install_target()?,
        test_sums_file().is_some(),
    )
}

fn plan_for(
    release: &Release,
    os: Os,
    arch: &str,
    target: Target,
    local_sums: bool,
) -> Option<Plan> {
    let asset = select_asset(release, os, arch)?.clone();
    let sums = release
        .assets
        .iter()
        .find(|a| a.name == SUMS_ASSET)
        .cloned();
    if sums.is_none() && !local_sums {
        return None;
    }
    Some(Plan {
        version: release.version.clone(),
        asset,
        sums,
        target,
    })
}

/// Dev builds only: read checksums from this file instead of the release's SHA256SUMS,
/// to try the install flow against releases published before SHA256SUMS existed.
fn test_sums_file() -> Option<PathBuf> {
    if matches!(build_info::CHANNEL, "stable" | "nightly") {
        return None;
    }
    std::env::var_os("LANLINK_UPDATE_TEST_SUMS").map(PathBuf::from)
}

/// The expected lowercase hex SHA-256 of `name` in a `sha256sum`-style listing.
fn expected_sha256(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        // `sha256sum -b` marks binary mode with a leading '*'.
        let file = file.trim_start();
        let file = file.strip_prefix('*').unwrap_or(file);
        let valid = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        (valid && file == name).then(|| hash.to_ascii_lowercase())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- download and stage ----

/// A downloaded, verified update ready for [`commit`] and [`relaunch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staged {
    /// Windows: the verified installer.
    Installer(PathBuf),
    /// macOS: the new bundle, copied next to the running `bundle`.
    Bundle { staged: PathBuf, bundle: PathBuf },
}

/// Downloads land here and are deleted on the next start.
fn work_dir() -> PathBuf {
    Config::dir().join("updates")
}

/// Download the plan's asset, verify it against SHA256SUMS and stage it. `progress` gets
/// (bytes so far, total if known).
pub async fn prepare(
    plan: Plan,
    progress: impl FnMut(u64, Option<u64>) + Send,
) -> anyhow::Result<Staged> {
    let dir = work_dir();
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    let client = client(None)?;

    let sums = match (test_sums_file(), &plan.sums) {
        (Some(path), _) => tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?,
        (None, Some(asset)) => {
            let body = fetch_small(&client, &asset.url, MAX_SUMS_BYTES).await?;
            String::from_utf8(body).context("SHA256SUMS is not text")?
        }
        (None, None) => bail!("the release has no {SUMS_ASSET}"),
    };
    let expected = expected_sha256(&sums, &plan.asset.name)
        .with_context(|| format!("{} is not listed in {SUMS_ASSET}", plan.asset.name))?;

    let file = dir.join(&plan.asset.name);
    let actual = download(&client, &plan.asset.url, &file, progress).await?;
    if actual != expected {
        let _ = tokio::fs::remove_file(&file).await;
        bail!("checksum mismatch for {}", plan.asset.name);
    }
    tracing::info!("update {} downloaded and verified", plan.version);

    match plan.target {
        Target::Installer => Ok(Staged::Installer(file)),
        Target::Bundle(bundle) => {
            let staged = tokio::task::spawn_blocking(move || stage_bundle(&file, &bundle, &dir))
                .await
                .context("staging task")??;
            Ok(staged)
        }
    }
}

async fn fetch_small(client: &reqwest::Client, url: &str, max: u64) -> anyhow::Result<Vec<u8>> {
    let mut resp = client.get(url).send().await?.error_for_status()?;
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        body.extend_from_slice(&chunk);
        if body.len() as u64 > max {
            bail!("{url} is larger than expected");
        }
    }
    Ok(body)
}

/// Stream `url` into `dest` (via a `.part` file), returning its hex SHA-256.
async fn download(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> anyhow::Result<String> {
    let mut resp = client.get(url).send().await?.error_for_status()?;
    let total = resp.content_length();
    if total.is_some_and(|t| t > MAX_ASSET_BYTES) {
        bail!("the update is larger than expected");
    }
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let mut file = tokio::fs::File::create(&part)
        .await
        .with_context(|| format!("creating {}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    progress(0, total);
    while let Some(chunk) = resp.chunk().await? {
        done += chunk.len() as u64;
        if done > MAX_ASSET_BYTES {
            bail!("the update is larger than expected");
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        progress(done, total);
    }
    file.sync_all().await?;
    drop(file);
    if total.is_some_and(|t| t != done) {
        bail!(
            "download ended early ({done} of {} bytes)",
            total.unwrap_or(0)
        );
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(hex(&hasher.finalize()))
}

/// Hidden siblings of the running bundle: the staged new version and, after [`commit`],
/// the old one (deleted by [`cleanup`] on the next start).
fn sibling(bundle: &Path, what: &str) -> PathBuf {
    let stem = bundle
        .file_stem()
        .map_or("lanlink".into(), |s| s.to_string_lossy());
    bundle.with_file_name(format!(".{stem}-{what}.app"))
}

/// Mount the disk image, copy its `.app` next to `bundle` (same volume, so [`commit`] can
/// rename it into place), and unmount.
fn stage_bundle(dmg: &Path, bundle: &Path, work: &Path) -> anyhow::Result<Staged> {
    let mnt = work.join("mnt");
    std::fs::create_dir_all(&mnt)?;
    run(Command::new("hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-noautoopen",
            "-readonly",
            "-mountpoint",
        ])
        .arg(&mnt)
        .arg(dmg))?;
    let staged = sibling(bundle, "update");
    let res = copy_app(&mnt, &staged);
    if let Err(e) = run(Command::new("hdiutil").arg("detach").arg(&mnt)) {
        tracing::warn!("hdiutil detach: {e:#}, retrying with -force");
        let _ = run(Command::new("hdiutil").args(["detach", "-force"]).arg(&mnt));
    }
    if let Err(e) = res {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(e);
    }
    Ok(Staged::Bundle {
        staged,
        bundle: bundle.to_path_buf(),
    })
}

fn copy_app(mnt: &Path, staged: &Path) -> anyhow::Result<()> {
    let app = std::fs::read_dir(mnt)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "app") && !p.is_symlink() && p.is_dir())
        .context("no .app in the disk image")?;
    if staged.exists() {
        std::fs::remove_dir_all(staged)?;
    }
    // ditto keeps extended attributes and the code signature intact.
    run(Command::new("ditto").arg(&app).arg(staged))?;
    // Downloads by reqwest aren't quarantined, but a copy from a quarantined image would be.
    let _ = run(Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(staged));
    run(Command::new("codesign")
        .args(["--verify", "--deep"])
        .arg(staged))
    .context("the new lanlink.app's signature does not verify")?;
    Ok(())
}

fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd.output().with_context(|| format!("running {name}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{name} failed ({}): {}", out.status, err.trim());
    }
    Ok(())
}

// ---- install ----

/// Put a staged update in place before quitting. macOS swaps the bundles (the running
/// process keeps its already loaded binary); Windows has nothing to do until [`relaunch`].
pub fn commit(staged: &Staged) -> anyhow::Result<()> {
    match staged {
        Staged::Installer(path) => {
            anyhow::ensure!(path.is_file(), "the downloaded installer is gone");
            Ok(())
        }
        Staged::Bundle { staged, bundle } => swap_bundle(staged, bundle),
    }
}

fn swap_bundle(staged: &Path, bundle: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(staged.is_dir(), "the downloaded lanlink.app is gone");
    let old = sibling(bundle, "old");
    if old.exists() {
        std::fs::remove_dir_all(&old).with_context(|| format!("removing {}", old.display()))?;
    }
    std::fs::rename(bundle, &old).with_context(|| format!("moving {}", bundle.display()))?;
    if let Err(e) = std::fs::rename(staged, bundle) {
        // Put the running version back so lanlink still starts.
        let _ = std::fs::rename(&old, bundle);
        return Err(e).with_context(|| format!("replacing {}", bundle.display()));
    }
    Ok(())
}

/// Called on quit, after the node shut down and the tray icon is gone: start the
/// installer (Windows) or the new bundle (macOS). Both wait for this process to be gone.
pub fn relaunch(staged: &Staged) -> anyhow::Result<()> {
    match staged {
        Staged::Installer(setup) => run_installer(setup),
        Staged::Bundle { bundle, .. } => {
            // Wait (up to 10 s) for this process to exit so the new one gets the
            // single-instance lock, then open a fresh instance of the bundle.
            Command::new("/bin/sh")
                .arg("-c")
                .arg(
                    "i=0; while kill -0 \"$1\" 2>/dev/null && [ $i -lt 50 ]; do \
                     sleep 0.2; i=$((i+1)); done; exec open -n \"$0\"",
                )
                .arg(bundle)
                .arg(std::process::id().to_string())
                .spawn()
                .context("starting the new lanlink")?;
            Ok(())
        }
    }
}

/// Passed to the old exe when the installer fails, so it can say so.
pub const FAILED_ARG: &str = "--update-failed";

/// `cmd /S /C` line that runs the installer silently and waits for it, then starts the
/// app again: the new one, or the untouched old one with [`FAILED_ARG`] if setup failed.
#[cfg_attr(not(windows), allow(dead_code))]
fn installer_command_line(setup: &Path, exe: &Path) -> String {
    let setup = setup.display();
    let exe = exe.display();
    format!(
        "/D /S /C \"start \"\" /wait \"{setup}\" /SILENT /SP- /NORESTART /CLOSEAPPLICATIONS \
         /RESTARTAPPLICATIONS & if errorlevel 1 (start \"\" \"{exe}\" {FAILED_ARG}) \
         else (start \"\" \"{exe}\")\""
    )
}

#[cfg(windows)]
fn run_installer(setup: &Path) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe()?;
    Command::new("cmd.exe")
        .raw_arg(installer_command_line(setup, &exe))
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()
        .context("starting the installer")?;
    Ok(())
}

#[cfg(not(windows))]
fn run_installer(_setup: &Path) -> anyhow::Result<()> {
    bail!("installers only run on Windows")
}

/// Delete leftovers of a previous update: downloads and, on macOS, the replaced bundle.
pub fn cleanup() {
    let _ = std::fs::remove_dir_all(work_dir());
    if let Some(Target::Bundle(bundle)) = install_target() {
        for what in ["old", "update"] {
            let path = sibling(&bundle, what);
            if path.exists() {
                if let Err(e) = std::fs::remove_dir_all(&path) {
                    tracing::warn!("removing {}: {e}", path.display());
                }
            }
        }
    }
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
            assets: Vec::new(),
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
        let r = newer("0.1.0", Channel::Stable, list()).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.url, "https://example.com/v0.2.0");
        assert_eq!(newer("0.2.0", Channel::Stable, list()), None);
    }

    #[test]
    fn nightly_picks_newest_prerelease() {
        let list = vec![
            rel("v0.1.0-nightly.20261008.12", true),
            rel("v0.2.0-nightly.20261010.14", true),
            rel("v0.1.0-nightly.20261009.13", true),
            rel("v0.5.0", false),
        ];
        let r = newer("0.1.0-nightly.20261008.12", Channel::Nightly, list).unwrap();
        assert_eq!(r.version, "0.2.0-nightly.20261010.14");
    }

    #[test]
    fn same_or_older_nightly_is_up_to_date() {
        let list = vec![rel("v0.1.0-nightly.20261009.13", true)];
        assert_eq!(
            newer("0.1.0-nightly.20261009.13", Channel::Nightly, list),
            None
        );
        let list = vec![rel("v0.1.0-nightly.20261009.13", true)];
        assert_eq!(
            newer("0.1.0-nightly.20261010.14", Channel::Nightly, list),
            None
        );
    }

    #[test]
    fn nightly_switching_to_stable_takes_latest_stable() {
        let r = newer(
            "0.3.0-nightly.20261010.20",
            Channel::Stable,
            vec![rel("v0.2.0", false)],
        );
        assert_eq!(r.unwrap().version, "0.2.0");
    }

    #[test]
    fn stable_switching_to_nightly_only_moves_forward() {
        let list = || vec![rel("v0.2.0-nightly.20261010.14", true)];
        let r = newer("0.1.0", Channel::Nightly, list()).unwrap();
        assert_eq!(r.version, "0.2.0-nightly.20261010.14");
        assert_eq!(newer("0.2.0", Channel::Nightly, list()), None);
    }

    #[test]
    fn ignores_drafts_and_bad_tags() {
        let mut draft = rel("v9.0.0", false);
        draft.draft = true;
        let list = vec![draft, rel("latest", false), rel("v0.1.1", false)];
        assert_eq!(
            newer("0.1.0", Channel::Stable, list).unwrap().version,
            "0.1.1"
        );
        assert_eq!(
            newer("not-a-version", Channel::Stable, vec![rel("v1.0.0", false)]),
            None
        );
    }

    #[test]
    fn parses_assets_from_the_api() {
        let json = r#"{"tag_name":"v0.3.0","html_url":"https://x/r","prerelease":false,
            "assets":[{"name":"SHA256SUMS","browser_download_url":"https://x/SHA256SUMS","size":3}]}"#;
        let r: GhRelease = serde_json::from_str(json).unwrap();
        let r = newer("0.2.0", Channel::Stable, vec![r]).unwrap();
        assert_eq!(
            r.assets,
            vec![Asset {
                name: "SHA256SUMS".into(),
                url: "https://x/SHA256SUMS".into()
            }]
        );
    }

    fn release(version: &str, names: &[&str]) -> Release {
        Release {
            version: version.into(),
            url: "https://example.com/release".into(),
            assets: names
                .iter()
                .map(|n| Asset {
                    name: n.to_string(),
                    url: format!("https://example.com/download/{n}"),
                })
                .collect(),
        }
    }

    fn stable_release() -> Release {
        release(
            "0.3.0",
            &[
                "lanlink-0.3.0-macos-arm64.dmg",
                "lanlink-0.3.0-macos-x64.dmg",
                "lanlink-0.3.0-windows-x64-setup.exe",
                "lanlink-0.3.0-windows-x64.zip",
                "SHA256SUMS",
            ],
        )
    }

    fn nightly_release() -> Release {
        let v = "0.3.0-nightly.20261010.14";
        release(
            v,
            &[
                &format!("lanlink-{v}-macos-arm64.dmg"),
                &format!("lanlink-{v}-windows-x64-setup.exe"),
                &format!("lanlink-{v}-windows-x64.zip"),
                "SHA256SUMS",
            ],
        )
    }

    fn selected(r: &Release, os: Os, arch: &str) -> Option<String> {
        select_asset(r, os, arch).map(|a| a.name.clone())
    }

    #[test]
    fn selects_asset_by_platform_and_arch() {
        let r = stable_release();
        assert_eq!(
            selected(&r, Os::Windows, "x86_64").as_deref(),
            Some("lanlink-0.3.0-windows-x64-setup.exe")
        );
        assert_eq!(
            selected(&r, Os::Mac, "aarch64").as_deref(),
            Some("lanlink-0.3.0-macos-arm64.dmg")
        );
        assert_eq!(
            selected(&r, Os::Mac, "x86_64").as_deref(),
            Some("lanlink-0.3.0-macos-x64.dmg")
        );
        assert_eq!(selected(&r, Os::Windows, "aarch64"), None);
        assert_eq!(selected(&r, Os::Other, "x86_64"), None);
    }

    #[test]
    fn nightly_on_intel_mac_has_no_asset() {
        let r = nightly_release();
        assert_eq!(selected(&r, Os::Mac, "x86_64"), None);
        assert_eq!(
            selected(&r, Os::Mac, "aarch64").as_deref(),
            Some("lanlink-0.3.0-nightly.20261010.14-macos-arm64.dmg")
        );
        assert_eq!(
            selected(&r, Os::Windows, "x86_64").as_deref(),
            Some("lanlink-0.3.0-nightly.20261010.14-windows-x64-setup.exe")
        );
        let bundle = Target::Bundle("/Applications/lanlink.app".into());
        assert_eq!(plan_for(&r, Os::Mac, "x86_64", bundle, false), None);
    }

    #[test]
    fn plan_needs_checksums() {
        let mut r = stable_release();
        let plan = plan_for(&r, Os::Windows, "x86_64", Target::Installer, false).unwrap();
        assert_eq!(plan.asset.name, "lanlink-0.3.0-windows-x64-setup.exe");
        assert_eq!(
            plan.sums.as_ref().map(|a| a.url.as_str()),
            Some("https://example.com/download/SHA256SUMS")
        );
        r.assets.retain(|a| a.name != SUMS_ASSET);
        assert_eq!(
            plan_for(&r, Os::Windows, "x86_64", Target::Installer, false),
            None
        );
        assert!(plan_for(&r, Os::Windows, "x86_64", Target::Installer, true).is_some());
    }

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_sha256sums() {
        let sums = format!(
            "{A}  lanlink-0.3.0-macos-arm64.dmg\n{B} *lanlink-0.3.0-windows-x64-setup.exe\r\n\nnot a line\n"
        );
        assert_eq!(
            expected_sha256(&sums, "lanlink-0.3.0-macos-arm64.dmg").as_deref(),
            Some(A)
        );
        assert_eq!(
            expected_sha256(&sums, "lanlink-0.3.0-windows-x64-setup.exe"),
            Some(B.to_ascii_lowercase())
        );
        assert_eq!(expected_sha256(&sums, "lanlink-0.3.0-macos-x64.dmg"), None);
        // No partial matches, and malformed hashes are ignored.
        assert_eq!(expected_sha256(&sums, "lanlink-0.3.0-macos-arm64"), None);
        let bad = "abc  lanlink.dmg\n";
        assert_eq!(expected_sha256(bad, "lanlink.dmg"), None);
    }

    #[test]
    fn sha256_matches_sha256sum_output() {
        let digest = hex(&Sha256::digest(b"lanlink"));
        let sums = format!("{digest}  lanlink.txt\n");
        assert_eq!(expected_sha256(&sums, "lanlink.txt"), Some(digest));
        assert_eq!(
            hex(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn detects_inno_install() {
        let installed = Path::new("C:/Users/me/AppData/Local/Programs/lanlink");
        let exists = |p: &Path| p.parent() == Some(installed);
        assert!(is_inno_install(&installed.join("lanlink.exe"), exists));
        let portable = Path::new("C:/Users/me/Downloads/lanlink/lanlink.exe");
        assert!(!is_inno_install(portable, exists));
        // The uninstaller exe without its data file is not an Inno install.
        let only_exe = |p: &Path| p.file_name().is_some_and(|n| n == "unins000.exe");
        assert!(!is_inno_install(&installed.join("lanlink.exe"), only_exe));
    }

    #[test]
    fn finds_app_bundle() {
        assert_eq!(
            app_bundle(Path::new(
                "/Applications/lanlink.app/Contents/MacOS/lanlink"
            )),
            Some(PathBuf::from("/Applications/lanlink.app"))
        );
        assert_eq!(
            app_bundle(Path::new(
                "/Users/me/Apps/lanlink.app/Contents/MacOS/lanlink"
            )),
            Some(PathBuf::from("/Users/me/Apps/lanlink.app"))
        );
        assert_eq!(
            app_bundle(Path::new("/Users/me/lanlink/target/debug/lanlink-app")),
            None
        );
        assert_eq!(
            app_bundle(Path::new(
                "/Volumes/lanlink 0.3.0/lanlink.app/Contents/MacOS/lanlink"
            )),
            None
        );
        assert_eq!(
            app_bundle(Path::new(
                "/private/var/folders/x/AppTranslocation/1234/d/lanlink.app/Contents/MacOS/lanlink"
            )),
            None
        );
    }

    #[test]
    fn bundle_siblings_are_hidden_next_to_it() {
        let b = Path::new("/Applications/lanlink.app");
        assert_eq!(
            sibling(b, "update"),
            PathBuf::from("/Applications/.lanlink-update.app")
        );
        assert_eq!(
            sibling(b, "old"),
            PathBuf::from("/Applications/.lanlink-old.app")
        );
    }

    #[test]
    fn installer_command_relaunches_either_way() {
        let line = installer_command_line(
            Path::new(r"C:\Users\me\AppData\Roaming\lanlink\updates\setup.exe"),
            Path::new(r"C:\Program Files\lanlink\lanlink.exe"),
        );
        assert_eq!(
            line,
            "/D /S /C \"start \"\" /wait \"C:\\Users\\me\\AppData\\Roaming\\lanlink\\updates\\setup.exe\" \
             /SILENT /SP- /NORESTART /CLOSEAPPLICATIONS /RESTARTAPPLICATIONS & if errorlevel 1 \
             (start \"\" \"C:\\Program Files\\lanlink\\lanlink.exe\" --update-failed) \
             else (start \"\" \"C:\\Program Files\\lanlink\\lanlink.exe\")\""
        );
    }

    #[test]
    fn swaps_bundle_in_place() {
        let dir = std::env::temp_dir().join(format!("lanlink-update-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = dir.join("lanlink.app");
        let staged = sibling(&bundle, "update");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("v"), "old").unwrap();
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("v"), "new").unwrap();

        swap_bundle(&staged, &bundle).unwrap();
        assert_eq!(std::fs::read_to_string(bundle.join("v")).unwrap(), "new");
        assert!(!staged.exists());
        assert!(sibling(&bundle, "old").exists());

        // A missing staged copy leaves the running bundle where it was.
        assert!(swap_bundle(&staged, &bundle).is_err());
        assert_eq!(std::fs::read_to_string(bundle.join("v")).unwrap(), "new");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
