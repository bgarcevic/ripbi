//! The `deps` command: flags in, a rendered dependency view out. The
//! exploration itself — target resolution, ingestion, the object lookup, and
//! the graph slices — is `ripbi_app::deps`; this module turns the flags into
//! a [`DepsRequest`], prints the header and notes on stderr, and hands the
//! slices to `deps::render`. Inspection only, so the exit code is 0 for every
//! produced view (including an empty one) and 2 for errors.

use std::path::Path;

use ripbi_app::deps::DepsRequest;

use crate::cli::DepsArgs;
use crate::error::ScanError;
use crate::progress::{self, Progress};
use crate::scan::{self, CliObserver};
use crate::style::Palette;

mod graph_view;
mod render;
mod tree;

/// Exit code: the requested view was produced, empty or not — inspection has
/// no findings code.
pub const EXIT_OK: i32 = 0;
/// Exit code: usage, discovery, lookup, or analysis error.
pub const EXIT_ERROR: i32 = 2;

/// Runs `deps` against the process's working directory.
#[must_use = "the return value is the process exit code"]
pub fn run(args: &DepsArgs, streams: &mut crate::scan::Streams<'_>) -> i32 {
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

/// Runs `deps` against an explicit working directory (tests pass one).
#[must_use = "the return value is the process exit code"]
pub fn run_in(args: &DepsArgs, cwd: &Path, streams: &mut crate::scan::Streams<'_>) -> i32 {
    let palette_err = Palette::detect(streams.stderr_is_tty, args.no_color);
    match explore(args, cwd, streams, &palette_err) {
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

fn explore(
    args: &DepsArgs,
    cwd: &Path,
    streams: &mut crate::scan::Streams<'_>,
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
        progress::DEPS,
    );
    let mut err = progress.writer(streams.err);
    let streams = &mut crate::scan::Streams {
        out: &mut *streams.out,
        err: &mut err,
        input: &mut *streams.input,
        stdin_is_tty: streams.stdin_is_tty,
        stdout_is_tty: streams.stdout_is_tty,
        stderr_is_tty: streams.stderr_is_tty,
    };
    let outcome = ripbi_app::deps::run(
        &request(args, cwd),
        &mut CliObserver {
            streams: &mut *streams,
            palette: palette_err,
            progress: Some(&progress),
            quiet: args.quiet,
            verbose: Some(args.verbose),
        },
    )?;
    let output = &outcome.output;
    // The header already named the model; the elapsed time rides on the
    // closing line, so the ticker just clears.
    let elapsed = progress.elapsed();
    progress.finish(streams.err).map_err(ScanError::from)?;

    if !args.quiet {
        // The deps JSON documents the graph, not the run, so skip notices are
        // stderr's job in every mode. They print above the output, so the
        // closing line stays last.
        scan::write_skip_notices(streams.err, &outcome.skips)?;
        let human = !(args.json || args.plain);
        if human {
            writeln!(streams.err).map_err(ScanError::from)?;
        }
        if args.json {
            render::json(streams.out, output)?;
        } else if args.graph {
            render::graph(streams.out, &palette_out, output).map_err(ScanError::from)?;
        } else if args.plain {
            render::plain(streams.out, output).map_err(ScanError::from)?;
        } else {
            render::human(streams.out, &palette_out, output).map_err(ScanError::from)?;
        }
        if human {
            render::closing(streams.out, &palette_out, output, elapsed).map_err(ScanError::from)?;
        }
        // Impact lists what ripbi sees; a consumer it cannot see is exactly
        // the one an empty or short Impact view would hide.
        if human && output.shows_impact() {
            let hint = scan::consumer_hint(outcome.query_logged, outcome.workbooks, "changing it");
            writeln!(streams.err, "  {}", palette_err.dim(&hint)).map_err(ScanError::from)?;
        }
    }
    Ok(EXIT_OK)
}

/// The flags as a [`DepsRequest`]; the output-mode flags stay here.
fn request(args: &DepsArgs, cwd: &Path) -> DepsRequest {
    DepsRequest {
        cwd: cwd.to_path_buf(),
        object: args.object.clone(),
        model: args.model.clone(),
        reports: args.reports.clone(),
        queries_from: args.queries_from.clone(),
        queries_item: args.queries_item.clone(),
        dependencies: args.dependencies,
        impact: args.impact,
        depth: args.depth.clone(),
        table: args.table.clone(),
        types: args.types.clone(),
        consumer: args.consumer.clone(),
        in_report: args.in_report.clone(),
        on_page: args.on_page.clone(),
        no_input: args.no_input,
        quiet: args.quiet,
    }
}
