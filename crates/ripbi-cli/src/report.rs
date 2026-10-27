//! The `report` command (issue #33): flags in, the report inventory —
//! pages → visuals → fields — out, as data. The inventory itself is
//! `ripbi_app::report`; this module turns the flags into a
//! [`ReportRequest`], prints the header and notes on stderr, and renders the
//! inventory in the mode the flags pick. Informational by design: nothing
//! here gates the exit code, so `scan`'s findings contract stays untouched.

pub(crate) mod render;

use std::path::Path;

use ripbi_app::report::ReportRequest;

use crate::cli::ReportArgs;
use crate::error::ScanError;
use crate::progress::{self, Progress};
use crate::scan::{CliObserver, Notes, Streams, write_skip_notices};
use crate::style::Palette;

/// Exit code: the inventory printed (or `--quiet` suppressed it). The command
/// is informational — unresolved bindings are listed, never failed on.
pub const EXIT_OK: i32 = 0;
/// Exit code: the inventory could not be produced.
pub const EXIT_ERROR: i32 = 2;

/// Runs `report` against the process's working directory.
#[must_use = "the return value is the process exit code"]
pub fn run(args: &ReportArgs, streams: &mut Streams<'_>) -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let _ = writeln!(
                streams.err,
                "error: cannot determine the working directory: {error}"
            );
            return EXIT_ERROR;
        }
    };
    run_in(args, &cwd, streams)
}

/// Runs `report` against an explicit working directory (tests pass one).
#[must_use = "the return value is the process exit code"]
pub fn run_in(args: &ReportArgs, cwd: &Path, streams: &mut Streams<'_>) -> i32 {
    let palette_err = Palette::detect(streams.stderr_is_tty, args.no_color);
    match inventory(args, cwd, streams, &palette_err) {
        Ok(code) => code,
        Err(error) => {
            if !args.quiet {
                let _ = writeln!(
                    streams.err,
                    "{} {}",
                    palette_err.alert("error:"),
                    error.message
                );
                if let Some(hint) = &error.hint {
                    let _ = writeln!(streams.err, "hint: {hint}");
                }
            }
            EXIT_ERROR
        }
    }
}

fn inventory(
    args: &ReportArgs,
    cwd: &Path,
    streams: &mut Streams<'_>,
    palette_err: &Palette,
) -> Result<i32, ScanError> {
    let palette_out = Palette::detect(streams.stdout_is_tty, args.no_color);
    // The stage ticker (human mode on a terminal only), started before
    // target resolution so a slow discovery walk has motion too. For the
    // whole run stderr is the ticker-aware writer: it erases the transient
    // line before every write, so no note lands on top of it.
    let progress = Progress::start(
        Progress::wanted(streams, palette_err, args.quiet, args.json || args.plain),
        *palette_err,
        progress::REPORT,
    );
    let mut err = progress.writer(streams.err);
    let streams = &mut Streams {
        out: &mut *streams.out,
        err: &mut err,
        input: &mut *streams.input,
        stdin_is_tty: streams.stdin_is_tty,
        stdout_is_tty: streams.stdout_is_tty,
        stderr_is_tty: streams.stderr_is_tty,
    };
    let notes = Notes(*palette_err);
    let request = request(args, cwd);
    let outcome = ripbi_app::report::run(
        &request,
        &mut CliObserver {
            streams: &mut *streams,
            palette: palette_err,
            progress: Some(&progress),
            quiet: args.quiet,
            verbose: None,
        },
    )?;
    let output = outcome.inventory;
    // The header already named what was read; the elapsed time rides on the
    // closing line, so the ticker just clears.
    let elapsed = progress.elapsed();
    progress.finish(streams.err).map_err(ScanError::from)?;

    if !args.quiet {
        // Notices and notes are stderr's job in text modes; --json carries
        // the notices itself. They print above the inventory, so the closing
        // line stays last.
        if !args.json {
            write_skip_notices(streams.err, &output.skips)?;
            if let Some(message) = ripbi_app::report::empty_note(&request, &output) {
                writeln!(streams.err, "{}", notes.warn(&message)).map_err(ScanError::from)?;
            }
        }
        if !args.json && !args.plain {
            writeln!(streams.err).map_err(ScanError::from)?;
        }
        if args.json {
            render::json(streams.out, &output).map_err(ScanError::from)?;
        } else if args.plain {
            render::plain(streams.out, &output).map_err(ScanError::from)?;
        } else {
            // The views render into a buffer first, so an empty one (filters
            // that matched nothing) leaves no stray blank line above the
            // closing line.
            let stdout = &mut *streams.out;
            let mut body: Vec<u8> = Vec::new();
            let streams_out: &mut dyn std::io::Write = &mut body;
            let mut table = false;
            if args.pages {
                render::pages_view(streams_out, &output).map_err(ScanError::from)?;
                table = true;
            }
            if args.visuals {
                if table {
                    writeln!(streams_out).map_err(ScanError::from)?;
                }
                render::visuals_view(streams_out, &output).map_err(ScanError::from)?;
                table = true;
            }
            if args.fields {
                if table {
                    writeln!(streams_out).map_err(ScanError::from)?;
                }
                render::fields_view(streams_out, &output).map_err(ScanError::from)?;
                table = true;
            }
            if args.used {
                if table {
                    writeln!(streams_out).map_err(ScanError::from)?;
                }
                render::used_view(streams_out, &output).map_err(ScanError::from)?;
                table = true;
            }
            if !table {
                render::human(streams_out, &palette_out, &output).map_err(ScanError::from)?;
            }
            stdout.write_all(&body).map_err(ScanError::from)?;
            if !body.is_empty() {
                writeln!(stdout).map_err(ScanError::from)?;
            }
            render::closing(stdout, &palette_out, &output, outcome.paired, elapsed)
                .map_err(ScanError::from)?;
            // The next-command suggestion is stderr's job, and it comes after
            // every stdout line: the streams can buffer independently.
            let unresolved: usize = output.reports.iter().map(|r| r.unresolved_count).sum();
            if unresolved > 0 && !args.broken {
                writeln!(
                    streams.err,
                    "  {}",
                    palette_err.dim("see them with: ripbi report --broken")
                )
                .map_err(ScanError::from)?;
            }
        }
    }
    Ok(EXIT_OK)
}

/// The flags as a [`ReportRequest`]; the output-mode flags stay here.
fn request(args: &ReportArgs, cwd: &Path) -> ReportRequest {
    ReportRequest {
        cwd: cwd.to_path_buf(),
        path: args.path.clone(),
        model: args.model.clone(),
        reports: args.reports.clone(),
        no_input: args.no_input,
        quiet: args.quiet,
        allow_no_model: args.allow_no_model,
        used: args.used,
        broken: args.broken,
        page: args.page.clone(),
        visual: args.visual.clone(),
        site: args.site.clone(),
        kind: args.kind.clone(),
        matches: args.matches.clone(),
    }
}
