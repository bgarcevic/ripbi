//! `ripbi update`: check GitHub Releases, verify the release archive's sha256
//! against `sha256sums.txt`, and atomically replace the installed binaries.
//!
//! Only self-managed installs (the install scripts) are replaced in place.
//! Cargo-managed installs and source builds under `target/` get guidance and
//! exit 0 without touching the network. Exit codes mirror `scan`: 0 success or
//! up to date, 1 update available (only from `--check`), 2 error.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::cli::UpdateArgs;
use crate::error::ScanError;
use crate::scan::Streams;
use crate::style::Palette;

/// Exit code: up to date, updated, or nothing to do.
pub const EXIT_OK: i32 = 0;
/// Exit code: `--check` found a newer release.
pub const EXIT_AVAILABLE: i32 = 1;
/// Exit code: the update could not run or complete.
pub const EXIT_ERROR: i32 = 2;

/// The repository whose releases feed `ripbi update`.
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/bgarcevic/ripbi/releases/latest";
/// Where the docs explain every install path.
const INSTALL_DOC: &str = "https://bgarcevic.github.io/ripbi/installation.html";
/// The release archive is a tarball everywhere except Windows.
const ARCHIVE_EXTENSION: &str = if cfg!(windows) { "zip" } else { "tar.gz" };
/// Hard cap on a downloaded archive, so a hostile redirect cannot exhaust RAM.
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// The interactive update's overall network budget.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// The background notifier child's shorter budget.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
/// The staging directory the archive is unpacked into, beside the install dir.
const STAGING_DIR: &str = ".ripbi.update.tmp";

/// A release as parsed from the GitHub API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// The release tag, without the leading `v` (for example `0.2.0`).
    pub tag: Version,
    /// Asset name → download URL, from the API's `assets[]`.
    pub assets: HashMap<String, AssetInfo>,
}

/// One release asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetInfo {
    /// The asset's `browser_download_url`.
    pub url: String,
    /// The asset's `size` in bytes, when the API reported it.
    pub size: Option<u64>,
}

/// The network operations `update` needs, behind a trait so tests can serve
/// fixtures without touching the network.
pub trait ReleaseClient {
    /// Fetches `releases/latest` from the GitHub API.
    fn latest_release(&self) -> Result<ReleaseInfo, ScanError>;
    /// Downloads one asset by URL.
    fn download(&self, url: &str) -> Result<Vec<u8>, ScanError>;
}

/// The real HTTP client: a synchronous `ureq` agent with a fixed overall
/// timeout, `.tar.gz`-friendly defaults, proxy environment support, and
/// redirects followed (ureq's defaults).
pub struct HttpReleaseClient {
    agent: ureq::Agent,
}

impl HttpReleaseClient {
    /// Builds the real client with the given overall timeout.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .user_agent(concat!("ripbi/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent }
    }
}

impl ReleaseClient for HttpReleaseClient {
    fn latest_release(&self) -> Result<ReleaseInfo, ScanError> {
        let response = self
            .agent
            .get(LATEST_RELEASE_URL)
            .call()
            .map_err(http_error)?;
        let body = response
            .into_body()
            .read_to_string()
            .map_err(|error| ScanError::new(format!("cannot read the GitHub response: {error}")))?;
        parse_latest_release(&body)
    }

    fn download(&self, url: &str) -> Result<Vec<u8>, ScanError> {
        let response = self.agent.get(url).call().map_err(http_error)?;
        response
            .into_body()
            .with_config()
            .limit(MAX_DOWNLOAD_BYTES)
            .read_to_vec()
            .map_err(|error| ScanError::new(format!("cannot download {url}: {error}")))
    }
}

/// How this executable was installed, classified from its canonical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Under `$CARGO_HOME/bin` — cargo owns the file.
    Cargo,
    /// Under a `target` build directory — a source checkout.
    SourceBuild,
    /// Anything else — the install scripts' `~/.local/bin` and friends.
    SelfManaged,
}

/// Runs `ripbi update` against the process's own executable.
#[must_use = "the return value is the process exit code"]
pub fn run(args: &UpdateArgs, streams: &mut Streams<'_>) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            if !args.quiet {
                let palette = Palette::detect(streams.stderr_is_tty, args.no_color);
                let _ = writeln!(
                    streams.err,
                    "{} cannot determine the running executable: {error}",
                    palette.red("error:")
                );
            }
            return EXIT_ERROR;
        }
    };
    let client = HttpReleaseClient::new(DEFAULT_TIMEOUT);
    run_in(args, &client, &exe, streams)
}

/// Runs `update` against an injected executable (tests pass one) and the
/// process's `$CARGO_HOME`.
#[must_use = "the return value is the process exit code"]
pub fn run_in(
    args: &UpdateArgs,
    client: &dyn ReleaseClient,
    exe: &Path,
    streams: &mut Streams<'_>,
) -> i32 {
    run_in_with_home(args, client, exe, default_cargo_home().as_deref(), streams)
}

/// Runs `update` with every ambient input injectable: the executable and the
/// cargo home both feed the channel heuristic, and the client is the test
/// seam.
#[must_use = "the return value is the process exit code"]
pub fn run_in_with_home(
    args: &UpdateArgs,
    client: &dyn ReleaseClient,
    exe: &Path,
    cargo_home: Option<&Path>,
    streams: &mut Streams<'_>,
) -> i32 {
    let palette = Palette::detect(streams.stderr_is_tty, args.no_color);
    match update(args, client, exe, cargo_home, streams) {
        Ok(code) => code,
        Err(error) => {
            if !args.quiet {
                let _ = writeln!(streams.err, "{} {}", palette.red("error:"), error.message);
                if let Some(hint) = &error.hint {
                    let _ = writeln!(streams.err, "hint: {hint}");
                }
            }
            EXIT_ERROR
        }
    }
}

/// The whole update flow with the error rendering left to the caller.
fn update(
    args: &UpdateArgs,
    client: &dyn ReleaseClient,
    exe: &Path,
    cargo_home: Option<&Path>,
    streams: &mut Streams<'_>,
) -> Result<i32, ScanError> {
    // 1. Channel: cargo and source builds never self-replace or go online.
    let channel = detect_channel(exe, cargo_home);
    if let Some(guidance) = channel_guidance(channel) {
        if !args.quiet {
            writeln!(streams.err, "{guidance}").map_err(ScanError::from)?;
        }
        return Ok(EXIT_OK);
    }

    // 2. Latest release.
    if !args.quiet {
        writeln!(streams.err, "Checking GitHub Releases…").map_err(ScanError::from)?;
    }
    let release = client.latest_release()?;
    let current = current_version();
    match plan(&release.tag, &current, args.check) {
        Plan::UpToDate => {
            if !args.quiet {
                if release.tag < current {
                    writeln!(
                        streams.err,
                        "ripbi {current} is newer than the latest release {}.",
                        release.tag
                    )
                    .map_err(ScanError::from)?;
                } else {
                    writeln!(streams.err, "ripbi {current} is up to date.")
                        .map_err(ScanError::from)?;
                }
            }
            Ok(EXIT_OK)
        }
        Plan::Available => {
            if !args.quiet {
                writeln!(streams.out, "current {current}").map_err(ScanError::from)?;
                writeln!(streams.out, "latest {}", release.tag).map_err(ScanError::from)?;
            }
            Ok(EXIT_AVAILABLE)
        }
        Plan::Update => self_update(args, client, exe, &current, &release, streams),
    }
}

/// What `--check` and the running version say to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// Latest is not newer than current.
    UpToDate,
    /// `--check` requested, a newer release exists.
    Available,
    /// A newer release exists and this is a real update run.
    Update,
}

fn plan(latest: &Version, current: &Version, check_only: bool) -> Plan {
    if latest <= current {
        Plan::UpToDate
    } else if check_only {
        Plan::Available
    } else {
        Plan::Update
    }
}

/// Downloads, verifies, unpacks, and replaces the installed binaries.
fn self_update(
    args: &UpdateArgs,
    client: &dyn ReleaseClient,
    exe: &Path,
    current: &Version,
    release: &ReleaseInfo,
    streams: &mut Streams<'_>,
) -> Result<i32, ScanError> {
    // The target triple is only needed here, so `--check` works anywhere.
    let target = target_triple()?;
    let archive_name = format!("ripbi-{}-{target}.{ARCHIVE_EXTENSION}", release.tag);
    let archive = release.assets.get(&archive_name).ok_or_else(|| {
        ScanError::new(format!(
            "the ripbi {} release has no {archive_name} asset",
            release.tag
        ))
        .with_hint(format!("build from source instead: {INSTALL_DOC}"))
    })?;
    let sums = release.assets.get("sha256sums.txt").ok_or_else(|| {
        ScanError::new(format!(
            "the ripbi {} release has no sha256sums.txt asset",
            release.tag
        ))
        .with_hint("the release looks incomplete; retry, or build from source")
    })?;

    // Print before the slow network work, per the CLI guidelines.
    if !args.quiet {
        let size = archive
            .size
            .map(|bytes| format!(" ({})", human_size(bytes)))
            .unwrap_or_default();
        writeln!(streams.err, "Downloading ripbi {}{size}…", release.tag)
            .map_err(ScanError::from)?;
    }
    let archive_bytes = client.download(&archive.url)?;
    let sums_bytes = client.download(&sums.url)?;

    // Verify before any filesystem write.
    if !args.quiet {
        writeln!(streams.err, "Verifying checksum…").map_err(ScanError::from)?;
    }
    let expected = parse_sha256sums(&String::from_utf8_lossy(&sums_bytes), &archive_name)?;
    let actual = sha256_hex(&archive_bytes);
    if expected != actual {
        return Err(
            ScanError::new(format!("checksum mismatch for {archive_name}")).with_hint(format!(
                "expected {expected}, got {actual}; nothing was installed"
            )),
        );
    }

    let install_dir = exe.parent().ok_or_else(|| {
        ScanError::new(format!(
            "cannot find the install directory of {}",
            exe.display()
        ))
    })?;
    if !args.quiet {
        writeln!(streams.err, "Installing…").map_err(ScanError::from)?;
    }
    install_archive(&archive_bytes, &release.tag, install_dir, exe)?;
    if !args.quiet {
        writeln!(
            streams.err,
            "Updated ripbi {current} → {} at {}",
            release.tag,
            exe.display()
        )
        .map_err(ScanError::from)?;
    }
    Ok(EXIT_OK)
}

/// Unpacks the verified archive into a staging directory beside the install
/// directory and renames the binaries into place. The staging directory shares
/// the install directory's filesystem, so every rename is atomic.
fn install_archive(
    archive_bytes: &[u8],
    version: &Version,
    install_dir: &Path,
    exe: &Path,
) -> Result<(), ScanError> {
    let staging = install_dir.join(STAGING_DIR);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)
        .map_err(|error| io_error("cannot create the staging directory", &staging, error))?;
    let result = (|| {
        extract_archive(archive_bytes, version, &staging)?;
        set_staged_permissions(&staging, exe);
        replace_binaries(&staging, install_dir, exe)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

/// Extracts `ripbi-<version>/ripbi` and `.../rib` into `staging`, by archive
/// kind per platform.
fn extract_archive(data: &[u8], version: &Version, staging: &Path) -> Result<(), ScanError> {
    if cfg!(windows) {
        extract_zip(data, version, staging)
    } else {
        extract_tar_gz(data, version, staging)
    }
}

/// Extracts the two binaries from a `.tar.gz` release archive.
fn extract_tar_gz(data: &[u8], version: &Version, staging: &Path) -> Result<(), ScanError> {
    let prefix = format!("ripbi-{version}/");
    let decoder = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| ScanError::new(format!("cannot read the release archive: {error}")))?;
    let mut remaining = binary_names();
    for entry in entries {
        let mut entry = entry
            .map_err(|error| ScanError::new(format!("cannot read the release archive: {error}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| {
                ScanError::new(format!("cannot read an archive member path: {error}"))
            })?
            .into_owned();
        if !path.starts_with(&prefix) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !remaining.iter().any(|candidate| candidate == name) {
            continue;
        }
        let destination = staging.join(name);
        entry.unpack(&destination).map_err(|error| {
            io_error(
                "cannot unpack the release archive into",
                &destination,
                error,
            )
        })?;
        remaining.retain(|candidate| candidate != name);
    }
    ensure_members_present(&remaining)
}

/// Extracts the two binaries from a `.zip` release archive (Windows releases).
fn extract_zip(data: &[u8], version: &Version, staging: &Path) -> Result<(), ScanError> {
    let prefix = format!("ripbi-{version}/");
    let reader = io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|error| ScanError::new(format!("cannot read the release archive: {error}")))?;
    let mut remaining = binary_names();
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|error| ScanError::new(format!("cannot read an archive member: {error}")))?;
        if !file.is_file() {
            continue;
        }
        let full_name = file.name().to_string();
        let path = Path::new(&full_name);
        if !path.starts_with(&prefix) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !remaining.iter().any(|candidate| candidate == name) {
            continue;
        }
        let destination = staging.join(name);
        let mut out = fs::File::create(&destination)
            .map_err(|error| io_error("cannot create", &destination, error))?;
        io::copy(&mut file, &mut out).map_err(|error| {
            io_error(
                "cannot unpack the release archive into",
                &destination,
                error,
            )
        })?;
        remaining.retain(|candidate| candidate != name);
    }
    ensure_members_present(&remaining)
}

/// The names the release archive carries: the tool and its short alias, with
/// the platform's executable suffix.
fn binary_names() -> Vec<String> {
    vec![
        format!("ripbi{}", std::env::consts::EXE_SUFFIX),
        format!("rib{}", std::env::consts::EXE_SUFFIX),
    ]
}

fn ensure_members_present(remaining: &[String]) -> Result<(), ScanError> {
    if remaining.is_empty() {
        return Ok(());
    }
    Err(ScanError::new(format!(
        "the release archive is missing {}",
        remaining.join(" and ")
    ))
    .with_hint(format!(
        "the release asset looks malformed; retry, or install from {INSTALL_DOC}"
    )))
}

/// Gives the staged binaries the installed executable's permissions. Tar
/// unpacking already preserves the archive's mode; this keeps the install
/// consistent even when the current binary was chmodded.
fn set_staged_permissions(staging: &Path, exe: &Path) {
    #[cfg(unix)]
    {
        let Ok(permissions) = fs::metadata(exe).map(|metadata| metadata.permissions()) else {
            return;
        };
        for name in binary_names() {
            let path = staging.join(name);
            if path.is_file() {
                let _ = fs::set_permissions(&path, permissions.clone());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (staging, exe);
}

/// Renames each staged binary over its installed sibling, skipping names the
/// install does not have. Unix `rename` is atomic per file. On Windows the
/// running executable cannot be overwritten while it runs, so it is renamed
/// to `<name>.old` first; the `.old` removal is best-effort and documented.
fn replace_binaries(staging: &Path, install_dir: &Path, exe: &Path) -> Result<(), ScanError> {
    #[cfg(windows)]
    let running = canonical(exe);
    #[cfg(not(windows))]
    let _ = exe;
    for name in binary_names() {
        let staged = staging.join(&name);
        if !staged.is_file() {
            continue;
        }
        let target = install_dir.join(&name);
        if !target.is_file() {
            continue;
        }
        #[cfg(windows)]
        if canonical(&target) == running {
            let old = install_dir.join(format!("{name}.old"));
            let _ = fs::remove_file(&old);
            fs::rename(&target, &old).map_err(|error| {
                io_error("cannot move the running binary aside", &target, error)
            })?;
            fs::rename(&staged, &target)
                .map_err(|error| io_error("cannot install the new binary", &target, error))?;
            // The old image is still mapped and cannot be deleted while this
            // process runs; the next update cleans it up.
            let _ = fs::remove_file(&old);
            continue;
        }
        fs::rename(&staged, &target)
            .map_err(|error| io_error("cannot install the new binary", &target, error))?;
    }
    Ok(())
}

/// Classifies an executable path against the cargo home. Pure, so tests feed
/// synthetic paths.
#[must_use]
pub fn detect_channel(exe: &Path, cargo_home: Option<&Path>) -> Channel {
    let exe = canonical(exe);
    if let Some(home) = cargo_home {
        let bin = canonical(&home.join("bin"));
        if exe.starts_with(&bin) {
            return Channel::Cargo;
        }
    }
    if exe
        .components()
        .any(|component| component.as_os_str() == "target")
    {
        return Channel::SourceBuild;
    }
    Channel::SelfManaged
}

/// The guidance printed for installs this tool never self-replaces.
fn channel_guidance(channel: Channel) -> Option<String> {
    match channel {
        Channel::Cargo => Some(
            "ripbi is managed by cargo here; update it with `cargo install ripbi --force`."
                .to_string(),
        ),
        Channel::SourceBuild => Some(format!(
            "this ripbi runs from a build tree; re-run the install script or `cargo install --path crates/ripbi-cli`. See {INSTALL_DOC}."
        )),
        Channel::SelfManaged => None,
    }
}

/// The running version, from the crate manifest.
pub(crate) fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("CARGO_PKG_VERSION is always a semantic version")
}

/// `$CARGO_HOME`, or `~/.cargo` (`%USERPROFILE%\.cargo` on Windows).
pub(crate) fn default_cargo_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CARGO_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(home_var)
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".cargo"))
}

/// The release target triple, or an error naming the build-from-source path on
/// platforms without prebuilt binaries.
pub fn target_triple() -> Result<&'static str, ScanError> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    target_for(os, arch).ok_or_else(|| {
        ScanError::new(format!("no prebuilt ripbi binary for {os}/{arch}"))
            .with_hint(format!("build from source instead: {INSTALL_DOC}"))
    })
}

fn target_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

/// The sha256 of `data` as lowercase hex.
fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

/// Finds the `<hash>  <archive-name>` line in `sha256sums.txt`. The `*` marker
/// sha256sum uses for binary mode is accepted.
fn parse_sha256sums(contents: &str, archive_name: &str) -> Result<String, ScanError> {
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let (Some(hash), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        if name.trim_start_matches('*') != archive_name {
            continue;
        }
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ScanError::new(format!(
                "sha256sums.txt has a malformed hash for {archive_name}"
            ))
            .with_hint("the release metadata looks corrupt; retry, or verify manually"));
        }
        return Ok(hash.to_ascii_lowercase());
    }
    Err(
        ScanError::new(format!("sha256sums.txt has no entry for {archive_name}"))
            .with_hint("the release asset looks incomplete; retry, or install from the docs"),
    )
}

/// Parses the `releases/latest` JSON into the tag and asset map.
fn parse_latest_release(body: &str) -> Result<ReleaseInfo, ScanError> {
    #[derive(Deserialize)]
    struct RawRelease {
        tag_name: String,
        #[serde(default)]
        assets: Vec<RawAsset>,
    }
    #[derive(Deserialize)]
    struct RawAsset {
        name: String,
        browser_download_url: String,
        #[serde(default)]
        size: Option<u64>,
    }

    let raw: RawRelease = serde_json::from_str(body).map_err(|error| {
        ScanError::new(format!("cannot parse the GitHub release metadata: {error}"))
    })?;
    let tag = raw.tag_name.trim().trim_start_matches('v');
    let tag = Version::parse(tag).map_err(|error| {
        ScanError::new(format!(
            "GitHub reported the release tag {:?}, which is not a semantic version: {error}",
            raw.tag_name
        ))
    })?;
    let assets = raw
        .assets
        .into_iter()
        .map(|asset| {
            (
                asset.name,
                AssetInfo {
                    url: asset.browser_download_url,
                    size: asset.size,
                },
            )
        })
        .collect();
    Ok(ReleaseInfo { tag, assets })
}

/// Rewrites a ureq failure for humans, per the CLI guidelines.
fn http_error(error: ureq::Error) -> ScanError {
    match &error {
        ureq::Error::StatusCode(403) => {
            ScanError::new("GitHub refused the request (HTTP 403): the anonymous rate limit may be exhausted")
                .with_hint(format!("wait a few minutes, then retry; or get the release from {INSTALL_DOC}"))
        }
        ureq::Error::StatusCode(404) => {
            ScanError::new("GitHub has no ripbi release at that address (HTTP 404)").with_hint(
                format!("check https://github.com/bgarcevic/ripbi/releases, or build from source: {INSTALL_DOC}"),
            )
        }
        ureq::Error::StatusCode(status) => {
            ScanError::new(format!("GitHub returned HTTP {status} for the release metadata"))
                .with_hint("retry in a moment; if it persists, report it at https://github.com/bgarcevic/ripbi/issues")
        }
        ureq::Error::Timeout(_) | ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => {
            ScanError::new(format!("cannot reach GitHub: {error}"))
                .with_hint("check your internet connection and HTTPS_PROXY, then retry")
        }
        _ => ScanError::new(format!("cannot reach GitHub: {error}"))
            .with_hint("check your internet connection and HTTPS_PROXY, then retry"),
    }
}

/// A short human size for the progress line: `132 B`, `1.4 MB`.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn io_error(action: &str, path: &Path, error: io::Error) -> ScanError {
    ScanError::new(format!("{action} {}: {error}", path.display()))
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("test version")
    }

    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ripbi-update-unit-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch dir");
        path
    }

    fn archive_files() -> Vec<(String, Vec<u8>)> {
        binary_names()
            .into_iter()
            .map(|name| {
                let data = format!("new contents of {name}").into_bytes();
                (name, data)
            })
            .collect()
    }

    fn tar_gz(files: &[(String, Vec<u8>)], version: &Version) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    format!("ripbi-{version}/{name}"),
                    data.as_slice(),
                )
                .expect("append tar member");
        }
        builder
            .into_inner()
            .expect("tar writer")
            .finish()
            .expect("gz finish")
    }

    fn zip_bytes(files: &[(String, Vec<u8>)], version: &Version) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in files {
            writer
                .start_file(format!("ripbi-{version}/{name}"), options)
                .expect("start zip member");
            writer.write_all(data).expect("write zip member");
        }
        writer.finish().expect("zip finish").into_inner()
    }

    #[test]
    fn channel_detection_classifies_by_path() {
        let cargo_home = Path::new("/home/u/.cargo");
        assert_eq!(
            detect_channel(Path::new("/home/u/.cargo/bin/ripbi"), Some(cargo_home)),
            Channel::Cargo
        );
        assert_eq!(
            detect_channel(
                Path::new("/home/u/repo/target/debug/ripbi"),
                Some(cargo_home)
            ),
            Channel::SourceBuild
        );
        assert_eq!(
            detect_channel(Path::new("/home/u/.local/bin/ripbi"), Some(cargo_home)),
            Channel::SelfManaged
        );
        // The `target` test is a path segment, not a substring.
        assert_eq!(
            detect_channel(Path::new("/home/u/targeted/ripbi"), Some(cargo_home)),
            Channel::SelfManaged
        );
    }

    #[test]
    fn sha256sums_parsing() {
        let wanted = "ripbi-0.2.0-x.tar.gz";
        let hash = "a".repeat(64);
        let contents = format!("{}  other.tar.gz\n{hash}  {wanted}\n", "b".repeat(64));
        assert_eq!(parse_sha256sums(&contents, wanted).expect("entry"), hash);
        assert_eq!(
            parse_sha256sums(&format!("{hash} *{wanted}\n"), wanted).expect("binary marker"),
            hash
        );
        assert!(parse_sha256sums("", wanted).is_err(), "missing entry");
        assert!(
            parse_sha256sums(&format!("zz  {wanted}\n"), wanted).is_err(),
            "malformed hash"
        );
    }

    #[test]
    fn plan_decides_between_up_to_date_check_and_update() {
        assert_eq!(
            plan(&version("0.1.0"), &version("0.1.0"), false),
            Plan::UpToDate
        );
        assert_eq!(
            plan(&version("0.0.9"), &version("0.1.0"), false),
            Plan::UpToDate
        );
        assert_eq!(
            plan(&version("0.2.0"), &version("0.1.0"), true),
            Plan::Available
        );
        assert_eq!(
            plan(&version("0.2.0"), &version("0.1.0"), false),
            Plan::Update
        );
    }

    #[test]
    fn target_for_maps_the_released_triples() {
        assert_eq!(
            target_for("linux", "x86_64"),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(target_for("macos", "aarch64"), Some("aarch64-apple-darwin"));
        assert_eq!(
            target_for("windows", "x86_64"),
            Some("x86_64-pc-windows-msvc")
        );
        assert_eq!(target_for("linux", "aarch64"), None);
    }

    #[test]
    fn tar_gz_extraction_round_trips() {
        let dir = scratch("tar-round-trip");
        let version = version("9.9.9");
        let files = archive_files();
        let data = tar_gz(&files, &version);
        extract_tar_gz(&data, &version, &dir).expect("extract");
        for (name, expected) in &files {
            assert_eq!(&fs::read(dir.join(name)).expect("extracted file"), expected);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn zip_extraction_round_trips() {
        let dir = scratch("zip-round-trip");
        let version = version("9.9.9");
        let files = archive_files();
        let data = zip_bytes(&files, &version);
        extract_zip(&data, &version, &dir).expect("extract");
        for (name, expected) in &files {
            assert_eq!(&fs::read(dir.join(name)).expect("extracted file"), expected);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extraction_rejects_an_archive_missing_a_binary() {
        let dir = scratch("tar-missing");
        let version = version("9.9.9");
        let only_ripbi = vec![(
            format!("ripbi{}", std::env::consts::EXE_SUFFIX),
            b"one".to_vec(),
        )];
        let data = tar_gz(&only_ripbi, &version);
        let error = extract_tar_gz(&data, &version, &dir).expect_err("must reject");
        assert!(error.message.contains("missing"), "got: {}", error.message);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn latest_release_parsing_strips_the_tag_prefix() {
        let body = r#"{
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "ripbi-0.2.0-x.tar.gz", "browser_download_url": "https://example.test/a", "size": 12},
                {"name": "sha256sums.txt", "browser_download_url": "https://example.test/s"}
            ]
        }"#;
        let release = parse_latest_release(body).expect("parse");
        assert_eq!(release.tag, version("0.2.0"));
        assert_eq!(release.assets["ripbi-0.2.0-x.tar.gz"].size, Some(12));
        assert_eq!(
            release.assets["sha256sums.txt"].url,
            "https://example.test/s"
        );
        assert!(parse_latest_release("{}").is_err(), "tag_name is required");
        assert!(
            parse_latest_release(r#"{"tag_name": "not-a-version"}"#).is_err(),
            "bad tag"
        );
    }

    #[test]
    fn human_size_is_short() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn http_errors_name_the_likely_cause() {
        assert!(
            http_error(ureq::Error::StatusCode(403))
                .message
                .contains("rate limit")
        );
        assert!(
            http_error(ureq::Error::StatusCode(404))
                .message
                .contains("404")
        );
        assert!(
            http_error(ureq::Error::StatusCode(500))
                .message
                .contains("500")
        );
    }
}
