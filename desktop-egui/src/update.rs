//! Self-update for release builds, like `ripbi update` for the CLI.
//!
//! A worker checks GitHub's `releases/latest`. When a newer release has a
//! `ripbi-desktop-<version>-<target>` archive for this platform, it downloads
//! it, verifies its sha256 against `sha256sums.txt`, and unpacks the new app
//! into a staging folder beside the installed one. The UI then offers
//! "Restart to update": [`Staged::apply`] swaps the staged copy in and starts
//! it. Nothing is replaced before the user asks.
//!
//! Only builds made by the release workflow update themselves (it sets
//! `RIPBI_DESKTOP_RELEASE`), so `cargo run` never rewrites a build tree.
//! `RIPBI_NO_UPDATE_CHECK` turns the check off, as it does for the CLI.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use eframe::egui;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/bgarcevic/ripbi/releases/latest";
/// The release workflow packages Linux as a tarball, the others as zips.
const ARCHIVE_EXTENSION: &str = if cfg!(target_os = "linux") {
    "tar.gz"
} else {
    "zip"
};
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// How long to wait before checking again while up to date or offline.
const RECHECK: Duration = Duration::from_secs(6 * 60 * 60);
/// Beside the installed app, so every rename stays on one filesystem.
const STAGING: &str = ".ripbi-desktop.update";
/// What the running copy is renamed to while the new one moves in.
const ASIDE: &str = ".ripbi-desktop.old";

/// A verified update, unpacked and waiting for "Restart to update".
#[derive(Debug)]
pub struct Staged {
    pub version: Version,
    /// The new executable (or `.app` bundle on macOS) in the staging folder.
    staged: PathBuf,
    /// The installed executable (or bundle) it replaces.
    installed: PathBuf,
}

/// Starts the background check, or returns `None` for builds that never
/// update themselves. The receiver yields once, when an update is staged;
/// failures and "up to date" retry quietly every few hours.
pub fn spawn(ctx: egui::Context) -> Option<Receiver<Staged>> {
    if option_env!("RIPBI_DESKTOP_RELEASE").is_none()
        || std::env::var_os("RIPBI_NO_UPDATE_CHECK").is_some()
    {
        return None;
    }
    let installed = installed_path().ok()?;
    let target = target_triple()?;
    let current = Version::parse(env!("CARGO_PKG_VERSION")).ok()?;
    sweep(&installed);
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        let client = Client::new();
        loop {
            if let Ok(Some(staged)) = check(&client, &current, target, &installed) {
                let _ = sender.send(staged);
                ctx.request_repaint();
                return;
            }
            std::thread::sleep(RECHECK);
        }
    });
    Some(receiver)
}

impl Staged {
    /// Moves the staged copy into place and starts it. The caller then closes
    /// this window. On failure the installed copy is put back.
    pub fn apply(&self) -> Result<(), String> {
        self.swap()?;
        relaunch(&self.installed)
            .map_err(|error| format!("Updated, but cannot restart ripbi: {error}"))
    }

    fn swap(&self) -> Result<(), String> {
        let aside = sibling(&self.installed, ASIDE);
        remove(&aside);
        fs::rename(&self.installed, &aside)
            .map_err(|error| format!("Cannot move {} aside: {error}", self.installed.display()))?;
        if let Err(error) = fs::rename(&self.staged, &self.installed) {
            let _ = fs::rename(&aside, &self.installed);
            return Err(format!(
                "Cannot install the update at {}: {error}",
                self.installed.display()
            ));
        }
        // Windows keeps the running exe locked; the next start sweeps it.
        remove(&aside);
        Ok(())
    }
}

/// One check: `Ok(None)` when up to date or no archive fits this platform.
fn check(
    client: &Client,
    current: &Version,
    target: &str,
    installed: &Path,
) -> Result<Option<Staged>, String> {
    let release = parse_latest_release(&client.text(LATEST_RELEASE_URL)?)?;
    if release.version <= *current {
        return Ok(None);
    }
    let name = archive_name(&release.version, target);
    let (Some(archive), Some(sums)) = (
        release.assets.get(&name),
        release.assets.get("sha256sums.txt"),
    ) else {
        return Ok(None);
    };
    let data = client.bytes(archive)?;
    let sums = String::from_utf8_lossy(&client.bytes(sums)?).into_owned();
    let expected = parse_sha256sums(&sums, &name)?;
    let actual = sha256_hex(&data);
    if expected != actual {
        return Err(format!("checksum mismatch for {name}"));
    }
    let staging = sibling(installed, STAGING);
    remove(&staging);
    fs::create_dir(&staging).map_err(|error| io_error("create", &staging, &error))?;
    let staged = unpack(&data, &release.version, &staging).inspect_err(|_| remove(&staging))?;
    Ok(Some(Staged {
        version: release.version,
        staged,
        installed: installed.to_path_buf(),
    }))
}

/// What updates replace: the executable, or on macOS its `.app` bundle.
fn installed_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| format!("cannot locate ripbi: {error}"))?;
    if cfg!(target_os = "macos") {
        // ripbi.app/Contents/MacOS/ripbi-desktop
        return exe
            .ancestors()
            .nth(3)
            .filter(|bundle| bundle.extension().is_some_and(|ext| ext == "app"))
            .map(Path::to_path_buf)
            .ok_or_else(|| "ripbi is not running from an .app bundle".to_string());
    }
    Ok(exe)
}

/// Clears what an earlier update left: the moved-aside copy, which Windows
/// would not let it delete while running, and any unfinished staging.
fn sweep(installed: &Path) {
    remove(&sibling(installed, ASIDE));
    remove(&sibling(installed, STAGING));
}

fn sibling(installed: &Path, name: &str) -> PathBuf {
    installed.with_file_name(name)
}

fn remove(path: &Path) {
    let _ = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
}

fn relaunch(installed: &Path) -> io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut open = std::process::Command::new("open");
        open.arg("-n").arg(installed);
        open
    } else {
        std::process::Command::new(installed)
    };
    command.spawn().map(drop)
}

/// The release targets the desktop app is built for (desktop.yml).
fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

fn archive_name(version: &Version, target: &str) -> String {
    format!("ripbi-desktop-{version}-{target}.{ARCHIVE_EXTENSION}")
}

/// Unpacks the app from a verified archive into `staging` and returns it.
fn unpack(data: &[u8], version: &Version, staging: &Path) -> Result<PathBuf, String> {
    if cfg!(target_os = "linux") {
        let binary = format!("ripbi-desktop-{version}/ripbi-desktop");
        unpack_tar_gz(data, &binary, staging)
    } else if cfg!(target_os = "macos") {
        unpack_zip(data, "ripbi.app/", staging)?;
        Ok(staging.join("ripbi.app"))
    } else {
        let binary = format!("ripbi-desktop-{version}/ripbi-desktop.exe");
        unpack_zip(data, &binary, staging)?;
        Ok(staging.join("ripbi-desktop.exe"))
    }
}

/// Extracts the entries under `prefix` (a folder ending in `/`, or one file)
/// into `staging`, keeping their paths and, on Unix, their permissions.
fn unpack_zip(data: &[u8], prefix: &str, staging: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(io::Cursor::new(data))
        .map_err(|error| format!("cannot read the update archive: {error}"))?;
    let mut found = false;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|error| format!("cannot read the update archive: {error}"))?;
        // enclosed_name refuses absolute paths and `..`.
        let Some(path) = file.enclosed_name() else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(prefix.trim_end_matches('/')) else {
            continue;
        };
        let keep = if prefix.ends_with('/') {
            Path::new(prefix.trim_end_matches('/'))
                .file_name()
                .map_or(relative.to_path_buf(), |root| {
                    Path::new(root).join(relative)
                })
        } else {
            PathBuf::from(path.file_name().unwrap_or_default())
        };
        let destination = staging.join(keep);
        if file.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|error| io_error("create", &destination, &error))?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error("create", parent, &error))?;
        }
        let mut out = fs::File::create(&destination)
            .map_err(|error| io_error("create", &destination, &error))?;
        io::copy(&mut file, &mut out).map_err(|error| io_error("write", &destination, &error))?;
        #[cfg(unix)]
        if let Some(mode) = file.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&destination, fs::Permissions::from_mode(mode & 0o777))
                .map_err(|error| io_error("set permissions on", &destination, &error))?;
        }
        found = true;
    }
    if found {
        Ok(())
    } else {
        Err(format!("the update archive has no {prefix}"))
    }
}

/// Extracts one executable from a `.tar.gz` archive into `staging`.
fn unpack_tar_gz(data: &[u8], binary: &str, staging: &Path) -> Result<PathBuf, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(data));
    let entries = archive
        .entries()
        .map_err(|error| format!("cannot read the update archive: {error}"))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|error| format!("cannot read the update archive: {error}"))?;
        let is_binary = entry.path().is_ok_and(|path| path == Path::new(binary));
        if !is_binary || !entry.header().entry_type().is_file() {
            continue;
        }
        let name = Path::new(binary).file_name().unwrap_or_default();
        let destination = staging.join(name);
        // unpack keeps the archived mode, so the binary stays executable.
        entry
            .unpack(&destination)
            .map_err(|error| io_error("write", &destination, &error))?;
        return Ok(destination);
    }
    Err(format!("the update archive has no {binary}"))
}

#[derive(Debug)]
struct Release {
    version: Version,
    /// Asset name → download URL.
    assets: HashMap<String, String>,
}

fn parse_latest_release(body: &str) -> Result<Release, String> {
    #[derive(Deserialize)]
    struct Raw {
        tag_name: String,
        #[serde(default)]
        assets: Vec<RawAsset>,
    }
    #[derive(Deserialize)]
    struct RawAsset {
        name: String,
        browser_download_url: String,
    }
    let raw: Raw = serde_json::from_str(body)
        .map_err(|error| format!("cannot parse the GitHub release metadata: {error}"))?;
    let version = Version::parse(raw.tag_name.trim().trim_start_matches('v'))
        .map_err(|error| format!("release tag {:?} is not a version: {error}", raw.tag_name))?;
    let assets = raw
        .assets
        .into_iter()
        .map(|asset| (asset.name, asset.browser_download_url))
        .collect();
    Ok(Release { version, assets })
}

/// Finds the `<hash>  <archive-name>` line in `sha256sums.txt`.
fn parse_sha256sums(contents: &str, name: &str) -> Result<String, String> {
    contents
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?, fields.next()?))
        })
        .find(|(_, file)| file.trim_start_matches('*') == name)
        .map(|(hash, _)| hash)
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("sha256sums.txt has no valid entry for {name}"))
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn io_error(action: &str, path: &Path, error: &io::Error) -> String {
    format!("cannot {action} {}: {error}", path.display())
}

struct Client {
    agent: ureq::Agent,
}

impl Client {
    fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(300)))
            .user_agent(concat!("ripbi-desktop/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent }
    }

    fn text(&self, url: &str) -> Result<String, String> {
        self.agent
            .get(url)
            .call()
            .map_err(|error| format!("cannot reach {url}: {error}"))?
            .into_body()
            .read_to_string()
            .map_err(|error| format!("cannot read {url}: {error}"))
    }

    fn bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        self.agent
            .get(url)
            .call()
            .map_err(|error| format!("cannot reach {url}: {error}"))?
            .into_body()
            .with_config()
            .limit(MAX_DOWNLOAD_BYTES)
            .read_to_vec()
            .map_err(|error| format!("cannot download {url}: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn release_metadata_names_the_version_and_assets() {
        let release = parse_latest_release(
            r#"{"tag_name":"v0.12.0","assets":[{"name":"sha256sums.txt","browser_download_url":"https://x/s"}]}"#,
        )
        .unwrap();
        assert_eq!(release.version, Version::new(0, 12, 0));
        assert_eq!(release.assets["sha256sums.txt"], "https://x/s");
        assert_eq!(
            archive_name(&release.version, "x86_64-pc-windows-msvc"),
            format!("ripbi-desktop-0.12.0-x86_64-pc-windows-msvc.{ARCHIVE_EXTENSION}")
        );
    }

    #[test]
    fn checksums_match_by_archive_name_only() {
        let hash = sha256_hex(b"archive");
        let sums = format!(
            "{}  ripbi-0.12.0-x.zip\n{hash} *ripbi-desktop-0.12.0-x.zip\n",
            "0".repeat(64)
        );
        assert_eq!(
            parse_sha256sums(&sums, "ripbi-desktop-0.12.0-x.zip").unwrap(),
            hash
        );
        assert!(parse_sha256sums(&sums, "ripbi-desktop-9.9.9-x.zip").is_err());
    }

    #[test]
    fn zip_unpacks_one_file_or_a_whole_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let data = zip_of(&[
            ("ripbi-desktop-0.12.0/ripbi-desktop.exe", b"new"),
            ("ripbi-desktop-0.12.0/LICENSE-MIT", b"license"),
        ]);
        unpack_zip(&data, "ripbi-desktop-0.12.0/ripbi-desktop.exe", dir.path()).unwrap();
        assert_eq!(
            fs::read(dir.path().join("ripbi-desktop.exe")).unwrap(),
            b"new"
        );
        assert!(!dir.path().join("LICENSE-MIT").exists());

        let data = zip_of(&[
            ("ripbi.app/Contents/Info.plist", b"plist"),
            ("ripbi.app/Contents/MacOS/ripbi-desktop", b"bin"),
        ]);
        unpack_zip(&data, "ripbi.app/", dir.path()).unwrap();
        let bundle = dir.path().join("ripbi.app/Contents");
        assert_eq!(
            fs::read(bundle.join("MacOS/ripbi-desktop")).unwrap(),
            b"bin"
        );
        assert_eq!(fs::read(bundle.join("Info.plist")).unwrap(), b"plist");
        assert!(unpack_zip(&data, "elsewhere/", dir.path()).is_err());
    }

    #[test]
    fn tarball_unpacks_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, data) in [
            ("ripbi-desktop-0.12.0/ripbi.desktop", &b"entry"[..]),
            ("ripbi-desktop-0.12.0/ripbi-desktop", &b"bin"[..]),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, data).unwrap();
        }
        let data = builder.into_inner().unwrap().finish().unwrap();
        let staged =
            unpack_tar_gz(&data, "ripbi-desktop-0.12.0/ripbi-desktop", dir.path()).unwrap();
        assert_eq!(fs::read(staged).unwrap(), b"bin");
    }

    #[test]
    fn apply_swaps_the_staged_copy_in() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("app");
        let staging = dir.path().join(STAGING);
        fs::write(&installed, b"old").unwrap();
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("app"), b"new").unwrap();
        let staged = Staged {
            version: Version::new(0, 12, 0),
            staged: staging.join("app"),
            installed: installed.clone(),
        };
        staged.swap().unwrap();
        assert_eq!(fs::read(&installed).unwrap(), b"new");
        assert!(!dir.path().join(ASIDE).exists());
    }
}
