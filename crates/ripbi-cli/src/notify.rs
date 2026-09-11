//! The ambient daily update notification.
//!
//! Every foreground command calls [`after_command`]: at most once per day it
//! spawns a detached `ripbi __update-check` child, which fetches
//! `releases/latest` and caches the version in a state file; after the command,
//! one dim stderr line announces a newer release. The check is a plain `GET`
//! of public release metadata — no data is sent — and every failure is silent.
//! The notifier never changes an exit code and is suppressed under
//! `RIPBI_NO_UPDATE_CHECK`, `CI`, `-q`, or when stderr is not a TTY.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::ScanError;
use crate::scan::Streams;
use crate::style::Palette;
use crate::update::{self, CHECK_TIMEOUT, Channel, HttpReleaseClient, ReleaseClient};

/// How often the background check may run, and how often the same notice may
/// repeat while the release stays newer.
const DAY_SECS: i64 = 24 * 60 * 60;
/// The state directory under `$XDG_STATE_HOME` / `%LOCALAPPDATA%`.
const STATE_DIR: &str = "ripbi";
/// The state file name.
const STATE_FILE: &str = "update-check.json";

/// The cached outcome of the last background check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    /// Unix seconds of the last completed check; 0 means never.
    #[serde(default)]
    pub last_check: i64,
    /// The latest release tag seen, semver without the leading `v`.
    #[serde(default)]
    pub latest: Option<String>,
    /// Unix seconds of the last printed notice; 0 means never.
    #[serde(default)]
    pub last_notify: i64,
    /// The release the last printed notice named.
    #[serde(default)]
    pub notified_version: Option<String>,
}

/// Everything [`after_command`] needs beyond the streams. Public so
/// integration tests drive the decision logic with a synthetic executable,
/// state file, and clock instead of the process environment.
#[derive(Debug, Clone)]
pub struct NotifyContext {
    /// The running executable: channel detection and the detached child.
    pub exe: PathBuf,
    /// `$CARGO_HOME`, when known.
    pub cargo_home: Option<PathBuf>,
    /// The state file to read and write.
    pub state_path: PathBuf,
    /// The command's `-q`.
    pub quiet: bool,
    /// `RIPBI_NO_UPDATE_CHECK` is set.
    pub no_update_check: bool,
    /// `CI` is set.
    pub ci: bool,
    /// Whether stderr is a terminal.
    pub stderr_is_tty: bool,
    /// Unix seconds to evaluate the rate limits against.
    pub now: i64,
}

/// The post-command hook: decide, spawn the detached check when due, and print
/// the notice when one is due. Called for every foreground command.
pub fn after_command(quiet: bool, streams: &mut Streams<'_>) {
    let Some(exe) = std::env::current_exe().ok() else {
        return;
    };
    let Some(state_path) = state_path() else {
        return;
    };
    let context = NotifyContext {
        exe,
        cargo_home: update::default_cargo_home(),
        state_path,
        quiet,
        no_update_check: std::env::var_os("RIPBI_NO_UPDATE_CHECK").is_some(),
        ci: std::env::var_os("CI").is_some(),
        stderr_is_tty: streams.stderr_is_tty,
        now: now_unix(),
    };
    after_command_in(&context, streams);
}

/// The injectable body of [`after_command`].
pub fn after_command_in(context: &NotifyContext, streams: &mut Streams<'_>) {
    if suppressed(
        context.no_update_check,
        context.ci,
        context.quiet,
        context.stderr_is_tty,
    ) {
        return;
    }

    let state = read_state(&context.state_path).unwrap_or_default();
    if should_check(&state, context.now) {
        maybe_spawn_check(&context.exe);
    }

    // The child may have finished while the command ran; re-read before
    // deciding so the notice appears in the same run when possible.
    let state = read_state(&context.state_path).unwrap_or_default();
    let current = update::current_version();
    if !should_notify(&state, context.now, &current) {
        return;
    }
    let Some(latest) = state.latest.as_deref().and_then(parse_version) else {
        return;
    };
    let channel = update::detect_channel(&context.exe, context.cargo_home.as_deref());
    let palette = Palette::detect(context.stderr_is_tty, false);
    let _ = writeln!(
        streams.err,
        "{}",
        palette.dim(&notice(&latest, &current, channel))
    );
    mark_notified(&context.state_path, context.now, &latest);
}

/// Fetches the latest release and rewrites the state file. The hidden
/// `__update-check` child's body: silent and always exit 0.
#[must_use = "the return value is the process exit code"]
pub fn run_check() -> i32 {
    let Some(state_path) = state_path() else {
        return 0;
    };
    let client = HttpReleaseClient::new(CHECK_TIMEOUT);
    run_check_in(&client, &state_path)
}

/// The injectable body of [`run_check`]. Always returns 0: the child must
/// never make a foreground command's exit code depend on the network.
#[must_use = "the return value is the process exit code"]
pub fn run_check_in(client: &dyn ReleaseClient, state_path: &Path) -> i32 {
    let _ = refresh(client, state_path);
    0
}

/// Fetches and records the latest release, preserving the notify bookkeeping.
fn refresh(client: &dyn ReleaseClient, state_path: &Path) -> Result<(), ScanError> {
    let release = client.latest_release()?;
    let mut state = read_state(state_path).unwrap_or_default();
    state.last_check = now_unix();
    state.latest = Some(release.tag.to_string());
    write_state(state_path, &state)
}

/// At most one check per day.
fn should_check(state: &UpdateState, now: i64) -> bool {
    now.saturating_sub(state.last_check) >= DAY_SECS
}

/// At most one notice per day per newer release. A different (newer) release
/// than the one last announced may notify immediately.
fn should_notify(state: &UpdateState, now: i64, current: &Version) -> bool {
    let Some(latest) = state.latest.as_deref().and_then(parse_version) else {
        return false;
    };
    if latest <= *current {
        return false;
    }
    let notified = state.notified_version.as_deref().and_then(parse_version);
    if notified.as_ref() == Some(&latest) {
        now.saturating_sub(state.last_notify) >= DAY_SECS
    } else {
        true
    }
}

/// The opt-outs, all pure so the matrix is testable without process state.
fn suppressed(no_update_check: bool, ci: bool, quiet: bool, stderr_is_tty: bool) -> bool {
    no_update_check || ci || quiet || !stderr_is_tty
}

/// Spawns the detached hidden child; every error is swallowed and the parent
/// never waits.
fn maybe_spawn_check(exe: &Path) {
    let mut command = Command::new(exe);
    command
        .arg("__update-check")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window may flash for the background check.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = command.spawn();
}

/// The one-line notice, with the next command adapted to the install channel.
fn notice(latest: &Version, current: &Version, channel: Channel) -> String {
    let command = match channel {
        Channel::Cargo => "cargo install ripbi --force",
        Channel::SourceBuild | Channel::SelfManaged => "ripbi update",
    };
    format!("ripbi {latest} is available (you have {current}) — run '{command}'")
}

/// Records that the notice was printed, preserving any fields the background
/// child rewrote concurrently.
fn mark_notified(state_path: &Path, now: i64, latest: &Version) {
    let mut state = read_state(state_path).unwrap_or_default();
    state.last_notify = now;
    state.notified_version = Some(latest.to_string());
    let _ = write_state(state_path, &state);
}

/// The state file's location: `$XDG_STATE_HOME/ripbi/update-check.json` on
/// Unix (falling back to `~/.local/state`), `%LOCALAPPDATA%\ripbi\...` on
/// Windows. `None` when the environment names no home.
#[must_use]
pub fn state_path() -> Option<PathBuf> {
    state_path_for(
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
        std::env::var_os("LOCALAPPDATA"),
    )
}

/// The pure location rule, so tests inject a temp directory.
fn state_path_for(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
    local_app_data: Option<OsString>,
) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let _ = (xdg_state_home, home);
        let base = local_app_data.filter(|value| !value.is_empty())?;
        Some(PathBuf::from(base).join(STATE_DIR).join(STATE_FILE))
    }
    #[cfg(not(windows))]
    {
        let _ = local_app_data;
        let base = xdg_state_home
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                home.filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })?;
        Some(base.join(STATE_DIR).join(STATE_FILE))
    }
}

/// Reads the state, or `None` when the file is missing, unreadable, or
/// malformed — all treated as "never checked".
fn read_state(state_path: &Path) -> Option<UpdateState> {
    serde_json::from_str(&fs::read_to_string(state_path).ok()?).ok()
}

/// Writes the state, creating the directory. Failures are the caller's to
/// swallow: an unwritable state directory must never break a command.
fn write_state(state_path: &Path, state: &UpdateState) -> Result<(), ScanError> {
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            ScanError::new(format!("cannot create {}: {error}", parent.display()))
        })?;
    }
    let json = serde_json::to_string(state)
        .map_err(|error| ScanError::new(format!("cannot serialize the update state: {error}")))?;
    fs::write(state_path, json)
        .map_err(|error| ScanError::new(format!("cannot write {}: {error}", state_path.display())))
}

fn parse_version(text: &str) -> Option<Version> {
    Version::parse(text.trim_start_matches('v')).ok()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("test version")
    }

    fn state(latest: Option<&str>, notified: Option<&str>, last_notify: i64) -> UpdateState {
        UpdateState {
            last_check: 0,
            latest: latest.map(str::to_string),
            last_notify,
            notified_version: notified.map(str::to_string),
        }
    }

    #[test]
    fn check_rate_limit() {
        assert!(should_check(&state(None, None, 0), 1_000_000));
        // Checked one hour ago: not yet.
        assert!(!should_check(
            &UpdateState {
                last_check: 1_000_000 - 3_600,
                ..UpdateState::default()
            },
            1_000_000
        ));
        assert!(should_check(
            &UpdateState {
                last_check: 1_000_000 - DAY_SECS,
                ..UpdateState::default()
            },
            1_000_000
        ));
        assert!(should_check(
            &UpdateState {
                last_check: 1_000_000 - DAY_SECS - 1,
                ..UpdateState::default()
            },
            1_000_000
        ));
    }

    #[test]
    fn notify_rate_limit_is_per_newer_version() {
        let current = version("0.1.0");
        let now = 1_000_000;

        // No known latest, or not newer: never.
        assert!(!should_notify(&state(None, None, 0), now, &current));
        assert!(!should_notify(
            &state(Some("0.1.0"), None, 0),
            now,
            &current
        ));
        assert!(!should_notify(
            &state(Some("0.0.9"), None, 0),
            now,
            &current
        ));

        // Newer and never announced: notify.
        assert!(should_notify(&state(Some("0.2.0"), None, 0), now, &current));

        // Announced the same version recently: hold.
        assert!(!should_notify(
            &state(Some("0.2.0"), Some("0.2.0"), now - 1),
            now,
            &current
        ));
        // ... but not forever: after a day the same notice may repeat.
        assert!(should_notify(
            &state(Some("0.2.0"), Some("0.2.0"), now - DAY_SECS),
            now,
            &current
        ));

        // A different newer release may notify immediately.
        assert!(should_notify(
            &state(Some("0.3.0"), Some("0.2.0"), now - 1),
            now,
            &current
        ));
    }

    #[test]
    fn suppression_matrix() {
        assert!(!suppressed(false, false, false, true));
        for (no_update_check, ci, quiet, tty) in [
            (true, false, false, true),
            (false, true, false, true),
            (false, false, true, true),
            (false, false, false, false),
        ] {
            assert!(
                suppressed(no_update_check, ci, quiet, tty),
                "({no_update_check}, {ci}, {quiet}, {tty}) must suppress"
            );
        }
    }

    #[test]
    fn state_round_trips_through_json() {
        let original = UpdateState {
            last_check: 42,
            latest: Some("0.2.0".to_string()),
            last_notify: 7,
            notified_version: Some("0.2.0".to_string()),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: UpdateState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, original);
        // Missing fields fall back to the never-checked state.
        let sparse: UpdateState = serde_json::from_str("{}").expect("empty is valid");
        assert_eq!(sparse, UpdateState::default());
    }

    #[cfg(not(windows))]
    #[test]
    fn state_path_prefers_xdg_then_home() {
        let path = state_path_for(
            Some(OsString::from("/xdg/state")),
            Some(OsString::from("/home/u")),
            None,
        );
        assert_eq!(
            path,
            Some(PathBuf::from("/xdg/state/ripbi/update-check.json"))
        );

        let path = state_path_for(None, Some(OsString::from("/home/u")), None);
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/home/u/.local/state/ripbi/update-check.json"
            ))
        );

        assert_eq!(state_path_for(None, None, None), None);
    }

    #[test]
    fn notice_adapts_to_the_channel() {
        assert_eq!(
            notice(&version("0.2.0"), &version("0.1.0"), Channel::SelfManaged),
            "ripbi 0.2.0 is available (you have 0.1.0) — run 'ripbi update'"
        );
        assert_eq!(
            notice(&version("0.2.0"), &version("0.1.0"), Channel::Cargo),
            "ripbi 0.2.0 is available (you have 0.1.0) — run 'cargo install ripbi --force'"
        );
    }
}
