//! `ripbi fetch`: flags in, a fetched project and an exit code out. The flow
//! itself — sign-in, model resolution, the downstream forecast, the
//! incomplete-set rule, the downloads, and the on-disk layout — is
//! `ripbi_app::fetch`; this module turns the flags into a [`FetchRequest`],
//! prints the forecast and summary on stderr, runs the pickers and the
//! confirmation, and with `--scan` scans the result.
//!
//! The forecast always prints, an interactive run asks before downloading an
//! incomplete set, and a non-interactive one refuses (exit `2`) unless
//! `--allow-incomplete`.

mod render;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ripbi_app::fetch::{FetchOutcome, FetchRequest, Observer};
use ripbi_service::Service;
use ripbi_service::auth::TokenSource;
use ripbi_service::downstream::{Downstream, Verification};
use ripbi_service::http::UreqTransport;

use crate::cli::{AuthMode, FetchArgs, ScanArgs};
use crate::error::ScanError;
use crate::picker::{confirm, pick};
use crate::progress::{self, Progress};
use crate::scan::{self, Streams};
use crate::style::Palette;

pub use ripbi_app::fetch::{MANIFEST_PATH, Manifest, ReportStatus};

/// Exit code: fetched (or forecast) successfully.
pub const EXIT_OK: i32 = 0;
/// Exit code: usage, sign-in, network, or an incomplete set not allowed.
pub const EXIT_ERROR: i32 = 2;

/// One request's budget. A definition export answers within seconds; the
/// long wait is polled, never held open.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs `fetch` against the real tenant, from the process's working directory.
#[must_use = "the return value is the process exit code"]
pub fn run(args: &FetchArgs, streams: &mut Streams<'_>) -> i32 {
    let palette = Palette::detect(streams.stderr_is_tty, args.no_color);
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            return report_error(
                args,
                streams,
                &palette,
                &ScanError::new(format!("cannot read the current directory: {error}")),
            );
        }
    };
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let tokens = match token_source(args.auth, &env) {
        Ok(tokens) => tokens,
        Err(error) => return report_error(args, streams, &palette, &error),
    };
    let service = Service::new(Box::new(UreqTransport::new(REQUEST_TIMEOUT)), tokens);
    run_with(args, &service, &cwd, streams)
}

/// Runs `fetch` with the service and working directory injected (tests).
#[must_use = "the return value is the process exit code"]
pub fn run_with(args: &FetchArgs, service: &Service, cwd: &Path, streams: &mut Streams<'_>) -> i32 {
    let palette = Palette::detect(streams.stderr_is_tty, args.no_color);
    match fetch(args, service, cwd, streams, &palette) {
        Ok(Fetched::Forecast) => EXIT_OK,
        Ok(Fetched::Written { root, exit }) => {
            if args.scan && exit == EXIT_OK {
                scan_fetched(args, &root, streams)
            } else {
                exit
            }
        }
        Err(error) => report_error(args, streams, &palette, &error),
    }
}

/// Picks the token source for `--auth` (see [`ripbi_app::fetch::token_source`]).
///
/// # Errors
///
/// When `--auth env` or `--auth sp` names variables that are unset.
pub fn token_source(
    mode: AuthMode,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Box<dyn TokenSource>, ScanError> {
    let mode = match mode {
        AuthMode::Auto => ripbi_app::fetch::AuthMode::Auto,
        AuthMode::Az => ripbi_app::fetch::AuthMode::Az,
        AuthMode::Env => ripbi_app::fetch::AuthMode::Env,
        AuthMode::Sp => ripbi_app::fetch::AuthMode::Sp,
    };
    ripbi_app::fetch::token_source(mode, env)
}

/// What a successful run produced.
enum Fetched {
    /// `--dry-run`: the forecast only.
    Forecast,
    /// A project on disk, and the exit code it earned.
    Written { root: PathBuf, exit: i32 },
}

/// The whole flow, errors left to the caller.
fn fetch(
    args: &FetchArgs,
    service: &Service,
    cwd: &Path,
    streams: &mut Streams<'_>,
    palette: &Palette,
) -> Result<Fetched, ScanError> {
    let started = Instant::now();
    let request = FetchRequest {
        cwd: cwd.to_path_buf(),
        link: args.link.clone(),
        workspace: args.workspace.clone(),
        model_name: args.model_name.clone(),
        out: args.out.clone(),
        verify: args.verify,
        dry_run: args.dry_run,
        allow_incomplete: args.allow_incomplete,
        force: args.force,
        interactive: streams.stdin_is_tty && !args.no_input && !args.quiet,
    };
    let mut observer = CliObserver {
        streams: &mut *streams,
        palette,
        quiet: args.quiet,
        progress: None,
    };
    let outcome = ripbi_app::fetch::run(&request, service, &mut observer)?;
    let progress = observer.progress.take();
    drop(observer);
    let FetchOutcome::Written {
        root,
        written,
        refused,
    } = outcome
    else {
        return Ok(Fetched::Forecast);
    };
    if let Some(progress) = progress {
        progress.finish(streams.err)?;
    }

    let exit = if refused { EXIT_ERROR } else { EXIT_OK };
    if !args.quiet {
        render::summary(
            streams.err,
            palette,
            &written,
            &display_path(cwd, &root),
            started.elapsed(),
            refused,
        )?;
        if !written.config_written {
            writeln!(
                streams.err,
                "{}",
                palette.warn_note(
                    "kept the existing ripbi.toml (not written by fetch); point its target and reports at the fetched folders"
                )
            )?;
        }
        writeln!(streams.out, "{}", display_path(cwd, &root))?;
    }
    Ok(Fetched::Written { root, exit })
}

/// The fetch's stderr: the sign-in line, the forecast, the pickers and the
/// confirmation, and the export ticker.
struct CliObserver<'s, 'a> {
    streams: &'s mut Streams<'a>,
    palette: &'s Palette,
    quiet: bool,
    /// Started at the first export: most of the wall time is the service
    /// preparing the exports, so the ticker keeps the wait visible.
    progress: Option<Progress>,
}

impl Observer for CliObserver<'_, '_> {
    fn signed_in(&mut self, auth: &str, tenant: Option<&str>) -> Result<(), ScanError> {
        if !self.quiet {
            let tenant = tenant
                .map(|tenant| format!(" (tenant {tenant})"))
                .unwrap_or_default();
            writeln!(
                self.streams.err,
                "{}",
                self.palette
                    .info_note(&format!("Signed in via {auth}{tenant}"))
            )?;
        }
        Ok(())
    }

    fn pick(&mut self, title: &str, prompt: &str, labels: &[String]) -> Result<usize, ScanError> {
        pick(title, prompt, labels, self.streams, self.palette)
    }

    fn forecast(
        &mut self,
        found: &Downstream,
        verification: Option<&Verification>,
    ) -> Result<(), ScanError> {
        if !self.quiet {
            render::forecast(self.streams.err, self.palette, found, verification)?;
        }
        Ok(())
    }

    fn confirm_incomplete(&mut self, _unreadable: usize, _total: usize) -> Result<bool, ScanError> {
        confirm(self.streams, "Continue with the readable reports? [y/N] ")
    }

    fn exporting(&mut self, done: usize, total: usize) {
        let progress = self.progress.get_or_insert_with(|| {
            Progress::start(
                Progress::wanted(self.streams, self.palette, self.quiet, false),
                *self.palette,
                progress::FETCH,
            )
        });
        progress.detail(format!("{done}/{total} definitions"));
    }

    fn writing(&mut self) {
        if let Some(progress) = &self.progress {
            progress.stage(1);
        }
    }
}

/// `--scan`: the bare `ripbi scan` a user would type next, run from inside
/// the fetched folder so its `ripbi.toml` supplies the model and reports.
fn scan_fetched(args: &FetchArgs, root: &Path, streams: &mut Streams<'_>) -> i32 {
    let scan_args = ScanArgs {
        quiet: args.quiet,
        no_color: args.no_color,
        no_input: true,
        ..ScanArgs::default()
    };
    if !args.quiet {
        let _ = writeln!(streams.err);
    }
    scan::run_in(&scan_args, root, streams)
}

/// The root as the user should type it: relative to the cwd when inside it.
fn display_path(cwd: &Path, root: &Path) -> String {
    let shown = root.strip_prefix(cwd).map_or_else(
        |_| root.to_path_buf(),
        |relative| Path::new(".").join(relative),
    );
    shown.display().to_string()
}

fn report_error(
    args: &FetchArgs,
    streams: &mut Streams<'_>,
    palette: &Palette,
    error: &ScanError,
) -> i32 {
    if !args.quiet {
        let _ = writeln!(streams.err, "{} {}", palette.alert("error:"), error.message);
        if let Some(hint) = &error.hint {
            let _ = writeln!(streams.err, "hint: {hint}");
        }
    }
    EXIT_ERROR
}
