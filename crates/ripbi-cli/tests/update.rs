//! Integration tests for `ripbi update` and the ambient daily notifier.
//! Everything runs in-process against a `FakeClient`, a temp install dir, and
//! in-memory streams — no test touches the network.

#[allow(dead_code)]
mod common;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use ripbi_cli::cli::UpdateArgs;
use ripbi_cli::error::ScanError;
use ripbi_cli::notify::{self, NotifyContext, UpdateState};
use ripbi_cli::scan::Streams;
use ripbi_cli::update::{self, AssetInfo, ReleaseClient, ReleaseInfo};
use sha2::{Digest, Sha256};

use common::TempDir;

/// The test double for the network: canned release metadata and downloads,
/// plus call counters so tests can pin "no request happened".
struct FakeClient {
    latest: Option<ReleaseInfo>,
    latest_error: Option<ScanError>,
    downloads: HashMap<String, Vec<u8>>,
    latest_calls: AtomicUsize,
    download_calls: AtomicUsize,
}

impl FakeClient {
    fn with(release: ReleaseInfo, downloads: HashMap<String, Vec<u8>>) -> Self {
        Self {
            latest: Some(release),
            latest_error: None,
            downloads,
            latest_calls: AtomicUsize::new(0),
            download_calls: AtomicUsize::new(0),
        }
    }

    /// A client that fails the test if it is ever used.
    fn unreachable() -> Self {
        Self {
            latest: None,
            latest_error: Some(ScanError::new("the network must not be called")),
            downloads: HashMap::new(),
            latest_calls: AtomicUsize::new(0),
            download_calls: AtomicUsize::new(0),
        }
    }
}

impl ReleaseClient for FakeClient {
    fn latest_release(&self) -> Result<ReleaseInfo, ScanError> {
        self.latest_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = &self.latest_error {
            return Err(error.clone());
        }
        self.latest
            .clone()
            .ok_or_else(|| ScanError::new("FakeClient has no release configured"))
    }

    fn download(&self, url: &str) -> Result<Vec<u8>, ScanError> {
        self.download_calls.fetch_add(1, Ordering::SeqCst);
        self.downloads
            .get(url)
            .cloned()
            .ok_or_else(|| ScanError::new(format!("FakeClient has no download for {url}")))
    }
}

fn binary(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn archive_name(version: &str) -> String {
    let target = update::target_triple().expect("the test host has a prebuilt target");
    let extension = if cfg!(windows) { "zip" } else { "tar.gz" };
    format!("ripbi-{version}-{target}.{extension}")
}

fn build_tar_gz(version: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("ripbi-{version}/{name}"), *data)
            .expect("append tar member");
    }
    builder
        .into_inner()
        .expect("tar writer")
        .finish()
        .expect("gz finish")
}

fn build_zip(version: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, data) in files {
        writer
            .start_file(format!("ripbi-{version}/{name}"), options)
            .expect("start zip member");
        writer.write_all(data).expect("write zip member");
    }
    writer.finish().expect("zip finish").into_inner()
}

fn build_archive(version: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    if cfg!(windows) {
        build_zip(version, files)
    } else {
        build_tar_gz(version, files)
    }
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A release fixture: the archive carries `files`, and its asset map plus the
/// `sha256sums.txt` asset make a self-consistent update.
fn fixture(version: &str, files: &[(&str, &[u8])]) -> (ReleaseInfo, HashMap<String, Vec<u8>>) {
    let archive = build_archive(version, files);
    let name = archive_name(version);
    let sums = format!("{}  {name}\n", sha256_hex(&archive));
    let archive_url = format!("https://example.test/{name}");
    let sums_url = "https://example.test/sha256sums.txt".to_string();

    let mut assets = HashMap::new();
    assets.insert(
        name,
        AssetInfo {
            url: archive_url.clone(),
            size: Some(archive.len() as u64),
        },
    );
    assets.insert(
        "sha256sums.txt".to_string(),
        AssetInfo {
            url: sums_url.clone(),
            size: Some(sums.len() as u64),
        },
    );
    let release = ReleaseInfo {
        tag: semver::Version::parse(version).expect("fixture version"),
        assets,
    };
    let mut downloads = HashMap::new();
    downloads.insert(archive_url, archive);
    downloads.insert(sums_url, sums.into_bytes());
    (release, downloads)
}

/// A temp install directory with both binaries at their old contents.
fn install_dir(temp: &TempDir, name: &str) -> PathBuf {
    let dir = temp.mkdir(name);
    fs::write(dir.join(binary("ripbi")), b"old-ripbi").expect("write old ripbi");
    fs::write(dir.join(binary("rib")), b"old-rib").expect("write old rib");
    dir
}

fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(dir).expect("read install dir") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_file() {
            entries.insert(name, fs::read(entry.path()).expect("read file"));
        } else if entry.path().is_dir() {
            entries.insert(format!("{name}/"), Vec::new());
        }
    }
    entries
}

fn run_update_with_home(
    args: &UpdateArgs,
    client: &dyn ReleaseClient,
    exe: &Path,
    cargo_home: Option<&Path>,
) -> (i32, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut input = std::io::empty();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        input: &mut input,
        stdin_is_tty: false,
        stderr_is_tty: false,
    };
    let code = update::run_in_with_home(args, client, exe, cargo_home, &mut streams);
    (
        code,
        String::from_utf8(out).expect("stdout is utf-8"),
        String::from_utf8(err).expect("stderr is utf-8"),
    )
}

fn run_update(args: &UpdateArgs, client: &dyn ReleaseClient, exe: &Path) -> (i32, String, String) {
    run_update_with_home(args, client, exe, None)
}

#[test]
fn up_to_date_reports_and_exits_zero() {
    let temp = TempDir::new("up-to-date");
    let dir = install_dir(&temp, "bin");
    let current = env!("CARGO_PKG_VERSION");
    let (release, downloads) = fixture(current, &[]);
    let client = FakeClient::with(release, downloads);

    let (code, stdout, stderr) =
        run_update(&UpdateArgs::default(), &client, &dir.join(binary("ripbi")));

    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.is_empty(), "data stays off stdout: {stdout}");
    assert!(stderr.contains("up to date"), "stderr: {stderr}");
    assert_eq!(client.latest_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_running_version_newer_than_latest_stays_zero() {
    let temp = TempDir::new("newer-than-latest");
    let dir = install_dir(&temp, "bin");
    let (release, downloads) = fixture("0.0.1", &[]);
    let client = FakeClient::with(release, downloads);

    let (code, _, stderr) = run_update(&UpdateArgs::default(), &client, &dir.join(binary("ripbi")));

    assert_eq!(code, 0);
    assert!(
        stderr.contains("newer than the latest release"),
        "stderr: {stderr}"
    );
}

#[test]
fn check_with_a_newer_release_exits_one_and_writes_nothing() {
    let temp = TempDir::new("check-newer");
    let dir = install_dir(&temp, "bin");
    let (release, downloads) = fixture("99.0.0", &[]);
    let client = FakeClient::with(release, downloads);
    let before = snapshot(&dir);
    let args = UpdateArgs {
        check: true,
        ..UpdateArgs::default()
    };

    let (code, stdout, _) = run_update(&args, &client, &dir.join(binary("ripbi")));

    assert_eq!(code, 1);
    assert!(stdout.contains(&format!("current {}", env!("CARGO_PKG_VERSION"))));
    assert!(stdout.contains("latest 99.0.0"));
    assert_eq!(snapshot(&dir), before, "nothing may be written");
    assert_eq!(
        client.download_calls.load(Ordering::SeqCst),
        0,
        "checksum and archive must not be downloaded"
    );
}

#[test]
fn full_update_replaces_both_binaries_and_exits_zero() {
    let temp = TempDir::new("full-update");
    let dir = install_dir(&temp, "bin");
    let ripbi = binary("ripbi");
    let rib = binary("rib");
    let files: [(&str, &[u8]); 2] = [
        (&ripbi, b"new-ripbi".as_slice()),
        (&rib, b"new-rib".as_slice()),
    ];
    let (release, downloads) = fixture("99.0.0", &files);
    let client = FakeClient::with(release, downloads);

    let (code, stdout, stderr) = run_update(&UpdateArgs::default(), &client, &dir.join(&ripbi));

    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.is_empty());
    assert_eq!(fs::read(dir.join(&ripbi)).expect("new ripbi"), b"new-ripbi");
    assert_eq!(fs::read(dir.join(&rib)).expect("new rib"), b"new-rib");
    assert!(
        stderr.contains(&format!(
            "Updated ripbi {} → 99.0.0",
            env!("CARGO_PKG_VERSION")
        )),
        "stderr: {stderr}"
    );
    assert!(
        !dir.join(".ripbi.update.tmp").exists(),
        "the staging directory is cleaned up"
    );
}

#[test]
fn quiet_update_prints_nothing() {
    let temp = TempDir::new("quiet-update");
    let dir = install_dir(&temp, "bin");
    let ripbi = binary("ripbi");
    let rib = binary("rib");
    let files: [(&str, &[u8]); 2] = [
        (&ripbi, b"new-ripbi".as_slice()),
        (&rib, b"new-rib".as_slice()),
    ];
    let (release, downloads) = fixture("99.0.0", &files);
    let client = FakeClient::with(release, downloads);
    let args = UpdateArgs {
        quiet: true,
        ..UpdateArgs::default()
    };

    let (code, stdout, stderr) = run_update(&args, &client, &dir.join(&ripbi));

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert_eq!(fs::read(dir.join(&ripbi)).expect("new ripbi"), b"new-ripbi");
}

#[test]
fn checksum_mismatch_aborts_before_any_write() {
    let temp = TempDir::new("bad-checksum");
    let dir = install_dir(&temp, "bin");
    let ripbi = binary("ripbi");
    let rib = binary("rib");
    let files: [(&str, &[u8]); 2] = [
        (&ripbi, b"new-ripbi".as_slice()),
        (&rib, b"new-rib".as_slice()),
    ];
    let (release, mut downloads) = fixture("99.0.0", &files);
    let sums = format!("{}  {}\n", "0".repeat(64), archive_name("99.0.0"));
    downloads.insert(
        "https://example.test/sha256sums.txt".to_string(),
        sums.into_bytes(),
    );
    let client = FakeClient::with(release, downloads);
    let before = snapshot(&dir);

    let (code, _, stderr) = run_update(&UpdateArgs::default(), &client, &dir.join(&ripbi));

    assert_eq!(code, 2);
    assert!(stderr.contains("checksum mismatch"), "stderr: {stderr}");
    assert_eq!(snapshot(&dir), before, "no file may change");
    assert!(
        !dir.join(".ripbi.update.tmp").exists(),
        "no staging directory"
    );
}

#[test]
fn cargo_managed_installs_get_guidance_without_any_request() {
    let temp = TempDir::new("cargo-managed");
    let home = temp.mkdir("cargo-home");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).expect("cargo bin");
    let exe = bin.join(binary("ripbi"));
    fs::write(&exe, b"old").expect("old binary");
    let client = FakeClient::unreachable();

    let (code, stdout, stderr) =
        run_update_with_home(&UpdateArgs::default(), &client, &exe, Some(&home));

    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("cargo install ripbi --force"),
        "stderr: {stderr}"
    );
    assert_eq!(
        client.latest_calls.load(Ordering::SeqCst),
        0,
        "cargo installs never self-check"
    );
}

#[test]
fn source_builds_get_guidance_without_any_request() {
    let temp = TempDir::new("source-build");
    let dir = temp.mkdir("repo/target/debug");
    let exe = dir.join(binary("ripbi"));
    fs::write(&exe, b"old").expect("old binary");
    let client = FakeClient::unreachable();

    let (code, _, stderr) = run_update(&UpdateArgs::default(), &client, &exe);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stderr.contains("build tree") || stderr.contains("cargo install --path"),
        "stderr: {stderr}"
    );
    assert_eq!(client.latest_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn network_errors_exit_two_with_a_hint() {
    let temp = TempDir::new("network-error");
    let dir = install_dir(&temp, "bin");
    let client = FakeClient {
        latest: None,
        latest_error: Some(
            ScanError::new("cannot reach GitHub: host not found")
                .with_hint("check your internet connection"),
        ),
        downloads: HashMap::new(),
        latest_calls: AtomicUsize::new(0),
        download_calls: AtomicUsize::new(0),
    };

    let (code, _, stderr) = run_update(&UpdateArgs::default(), &client, &dir.join(binary("ripbi")));

    assert_eq!(code, 2);
    assert!(
        stderr.contains("error: cannot reach GitHub"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("hint: check your internet connection"));
}

fn notify_context(temp: &TempDir, exe: &Path, now: i64) -> NotifyContext {
    NotifyContext {
        exe: exe.to_path_buf(),
        cargo_home: None,
        state_path: temp.0.join("state/update-check.json"),
        quiet: false,
        no_update_check: false,
        ci: false,
        stderr_is_tty: true,
        now,
    }
}

fn seed_state(context: &NotifyContext, state: &UpdateState) {
    fs::create_dir_all(context.state_path.parent().expect("state parent")).expect("state dir");
    fs::write(
        &context.state_path,
        serde_json::to_string(state).expect("serialize state"),
    )
    .expect("write state");
}

fn read_state(context: &NotifyContext) -> UpdateState {
    serde_json::from_str(&fs::read_to_string(&context.state_path).expect("read state"))
        .expect("parse state")
}

fn run_notify(context: &NotifyContext) -> (String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut input = std::io::empty();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        input: &mut input,
        stdin_is_tty: false,
        stderr_is_tty: context.stderr_is_tty,
    };
    notify::after_command_in(context, &mut streams);
    (
        String::from_utf8(out).expect("stdout is utf-8"),
        String::from_utf8(err).expect("stderr is utf-8"),
    )
}

#[test]
fn a_seeded_state_prints_the_notice_then_rate_limits_it() {
    let temp = TempDir::new("notify-once");
    let bin = temp.mkdir("bin");
    let exe = bin.join(binary("ripbi"));
    fs::write(&exe, b"exe").expect("fake exe");
    let context = notify_context(&temp, &exe, 1_000_000);
    seed_state(
        &context,
        &UpdateState {
            last_check: context.now,
            latest: Some("99.0.0".to_string()),
            last_notify: 0,
            notified_version: None,
        },
    );

    let (stdout, stderr) = run_notify(&context);

    assert!(stdout.is_empty());
    assert!(
        stderr.contains("ripbi 99.0.0 is available (you have"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("run 'ripbi update'"), "stderr: {stderr}");
    let state = read_state(&context);
    assert_eq!(state.last_notify, context.now);
    assert_eq!(state.notified_version.as_deref(), Some("99.0.0"));

    // The same version the same day: silence.
    let (_, stderr) = run_notify(&context);
    assert!(stderr.is_empty(), "repeat notice: {stderr}");
}

#[test]
fn suppression_rows_silence_the_notice() {
    let rows = [
        ("quiet", true, false, false, true),
        ("opt-out", false, true, false, true),
        ("ci", false, false, true, true),
        ("not-a-tty", false, false, false, false),
    ];
    for (index, (label, quiet, no_update_check, ci, tty)) in rows.into_iter().enumerate() {
        let temp = TempDir::new(&format!("notify-suppress-{index}-{label}"));
        let bin = temp.mkdir("bin");
        let exe = bin.join(binary("ripbi"));
        fs::write(&exe, b"exe").expect("fake exe");
        let mut context = notify_context(&temp, &exe, 1_000_000);
        context.quiet = quiet;
        context.no_update_check = no_update_check;
        context.ci = ci;
        context.stderr_is_tty = tty;
        seed_state(
            &context,
            &UpdateState {
                last_check: context.now,
                latest: Some("99.0.0".to_string()),
                last_notify: 0,
                notified_version: None,
            },
        );

        let (_, stderr) = run_notify(&context);

        assert!(
            stderr.is_empty(),
            "{label} must suppress the notice: {stderr}"
        );
    }
}

#[test]
fn the_notice_tail_follows_the_install_channel() {
    let temp = TempDir::new("notify-cargo-tail");
    let home = temp.mkdir("cargo-home");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).expect("cargo bin");
    let exe = bin.join(binary("ripbi"));
    fs::write(&exe, b"exe").expect("fake exe");
    let mut context = notify_context(&temp, &exe, 1_000_000);
    context.cargo_home = Some(home);
    seed_state(
        &context,
        &UpdateState {
            last_check: context.now,
            latest: Some("99.0.0".to_string()),
            last_notify: 0,
            notified_version: None,
        },
    );

    let (_, stderr) = run_notify(&context);

    assert!(
        stderr.contains("run 'cargo install ripbi --force'"),
        "stderr: {stderr}"
    );
}

#[test]
fn the_hidden_child_writes_the_state_and_exits_zero() {
    let temp = TempDir::new("update-check-child");
    let state_path = temp.0.join("state/update-check.json");
    let (release, downloads) = fixture("99.0.0", &[]);
    let client = FakeClient::with(release, downloads);

    let code = notify::run_check_in(&client, &state_path);

    assert_eq!(code, 0);
    assert_eq!(client.latest_calls.load(Ordering::SeqCst), 1);
    let state: UpdateState =
        serde_json::from_str(&fs::read_to_string(&state_path).expect("state file"))
            .expect("parse state");
    assert_eq!(state.latest.as_deref(), Some("99.0.0"));
    assert!(state.last_check > 0);
}

#[test]
fn the_hidden_child_exits_zero_even_when_the_fetch_fails() {
    let temp = TempDir::new("update-check-fail");
    let state_path = temp.0.join("state/update-check.json");
    let client = FakeClient::unreachable();

    let code = notify::run_check_in(&client, &state_path);

    assert_eq!(code, 0, "the child never propagates failure");
    assert!(!state_path.exists(), "a failed check writes nothing");
}
