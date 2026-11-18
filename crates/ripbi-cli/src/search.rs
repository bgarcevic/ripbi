//! The `search` command: flags in, matching expression lines out. The search
//! itself — target resolution, ingestion, and matching — is
//! `ripbi_app::search`; this module turns the flags into a [`SearchRequest`]
//! and renders the hits. Inspection only, so the exit code is 0 for every
//! produced result (no matches included) and 2 for errors.

use std::io;
use std::path::Path;
use std::time::Duration;

use ripbi_app::search::{Hit, SearchOutput, SearchRequest};

use crate::cli::SearchArgs;
use crate::error::ScanError;
use crate::progress::{self, Progress};
use crate::render::format_elapsed;
use crate::scan::{self, CliObserver};
use crate::style::Palette;

/// Exit code: the search ran, with or without matches.
pub const EXIT_OK: i32 = 0;
/// Exit code: usage, discovery, or ingestion error.
pub const EXIT_ERROR: i32 = 2;

/// Runs `search` against the process's working directory.
#[must_use = "the return value is the process exit code"]
pub fn run(args: &SearchArgs, streams: &mut crate::scan::Streams<'_>) -> i32 {
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

/// Runs `search` against an explicit working directory (tests pass one).
#[must_use = "the return value is the process exit code"]
pub fn run_in(args: &SearchArgs, cwd: &Path, streams: &mut crate::scan::Streams<'_>) -> i32 {
    let palette_err = Palette::detect(streams.stderr_is_tty, args.no_color);
    match search(args, cwd, streams, &palette_err) {
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

fn search(
    args: &SearchArgs,
    cwd: &Path,
    streams: &mut crate::scan::Streams<'_>,
    palette_err: &Palette,
) -> Result<i32, ScanError> {
    let palette_out = Palette::detect(streams.stdout_is_tty, args.no_color);
    let progress = Progress::start(
        Progress::wanted(streams, palette_err, args.quiet, args.json || args.plain),
        *palette_err,
        progress::SEARCH,
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
    let outcome = ripbi_app::search::run(
        &request(args, cwd),
        &mut CliObserver {
            streams: &mut *streams,
            palette: palette_err,
            progress: Some(&progress),
            quiet: args.quiet,
            verbose: Some(args.verbose),
        },
    )?;
    let elapsed = progress.elapsed();
    progress.finish(streams.err).map_err(ScanError::from)?;

    if !args.quiet {
        // A skipped object was never searched, so the notices matter in
        // every mode; they go to stderr, above the output.
        scan::write_skip_notices(streams.err, &outcome.skips)?;
        if args.json {
            ripbi_app::search::write_json(streams.out, &outcome.output).map_err(ScanError::from)?;
        } else if args.plain {
            plain(streams.out, &outcome.output).map_err(ScanError::from)?;
        } else {
            writeln!(streams.err).map_err(ScanError::from)?;
            human(streams.out, &palette_out, &outcome.output, elapsed).map_err(ScanError::from)?;
        }
    }
    Ok(EXIT_OK)
}

/// The flags as a [`SearchRequest`]; the output-mode flags stay here.
fn request(args: &SearchArgs, cwd: &Path) -> SearchRequest {
    SearchRequest {
        cwd: cwd.to_path_buf(),
        pattern: args.pattern.clone(),
        regex: args.regex,
        case_sensitive: args.case_sensitive,
        model: args.model.clone(),
        reports: args.reports.clone(),
        dax: args.dax,
        m: args.m,
        types: args.types.clone(),
        no_input: args.no_input,
        quiet: args.quiet,
    }
}

/// One block per expression — the owner, then its matching lines as
/// `line:column  text` with the match picked out — and a closing count.
fn human(
    out: &mut dyn io::Write,
    palette: &Palette,
    output: &SearchOutput,
    elapsed: Option<Duration>,
) -> io::Result<()> {
    let mut previous: Option<(&str, Option<&str>, &str)> = None;
    for hit in &output.hits {
        let block = (hit.id.as_str(), hit.report.as_deref(), hit.property);
        if previous != Some(block) {
            if previous.is_some() {
                writeln!(out)?;
            }
            writeln!(
                out,
                "{}{}",
                palette.bold(&hit.id),
                heading_detail(palette, hit)
            )?;
            previous = Some(block);
        }
        let position = format!("{}:{}", hit.line, hit.column);
        writeln!(
            out,
            "  {}  {}",
            palette.dim(&format!("{position:>6}")),
            highlighted(palette, hit)
        )?;
    }
    if !output.hits.is_empty() {
        writeln!(out)?;
    }
    let elapsed = elapsed
        .map(|elapsed| format!("  {}", palette.dim(&format_elapsed(elapsed))))
        .unwrap_or_default();
    match output.hits.len() {
        0 => writeln!(out, "No matches for {:?}{elapsed}", output.pattern),
        hits => writeln!(
            out,
            "{hits} matching {} in {} {}{elapsed}",
            if hits == 1 { "line" } else { "lines" },
            output.objects,
            if output.objects == 1 {
                "object"
            } else {
                "objects"
            },
        ),
    }
}

/// What follows the owner's id: its kind, the property when it is not the
/// main expression, `m` for Power Query, and a report measure's report.
fn heading_detail(palette: &Palette, hit: &Hit) -> String {
    // Ids of everything but columns and measures already lead with the kind
    // (`calculation item 'Time'[YoY]`).
    let kind = hit.kind.replace('_', " ");
    let mut parts = Vec::new();
    if !hit.id.starts_with(&kind) {
        parts.push(kind);
    }
    if hit.property != "expression" {
        parts.push(hit.property.replace('_', " "));
    }
    if hit.language == "m" {
        parts.push("Power Query".to_string());
    }
    let mut detail = if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", palette.dim(&parts.join(" · ")))
    };
    if let Some(report) = &hit.report {
        detail.push_str(&format!(" — in report {report}"));
    }
    detail
}

/// The line without its indentation, the match in the accent ink.
fn highlighted(palette: &Palette, hit: &Hit) -> String {
    let text = &hit.text;
    let indent = text.len() - text.trim_start().len();
    let start = hit.span.start.max(indent);
    let end = hit.span.end.max(start);
    format!(
        "{}{}{}",
        &text[indent.min(start)..start],
        palette.warn(&text[start..end]),
        text[end..].trim_end()
    )
}

/// `<type>\t<id>\t<language>\t<property>\t<line>\t<column>\t<text>`, one
/// record per matching line; tabs inside the line become spaces.
fn plain(out: &mut dyn io::Write, output: &SearchOutput) -> io::Result<()> {
    for hit in &output.hits {
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            hit.kind,
            hit.id,
            hit.language,
            hit.property,
            hit.line,
            hit.column,
            hit.text.replace('\t', " ").trim_end()
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(text: &str, span: std::ops::Range<usize>) -> Hit {
        Hit {
            id: "'Sales'[Total]".to_string(),
            kind: "measure",
            report: None,
            language: "dax",
            property: "expression",
            line: 2,
            column: text[..span.start].chars().count() + 1,
            text: text.to_string(),
            span,
        }
    }

    fn output(hits: Vec<Hit>) -> SearchOutput {
        SearchOutput {
            pattern: "sum".to_string(),
            target: "Sales.SemanticModel".to_string(),
            objects: usize::from(!hits.is_empty()),
            hits,
        }
    }

    #[test]
    fn human_groups_lines_under_their_owner_and_counts() {
        let mut out = Vec::new();
        let hits = vec![
            hit("    SUM(Sales[Amount])", 4..7),
            hit("  + SUM(Sales[Tax])", 4..7),
        ];
        human(&mut out, &Palette::plain(), &output(hits), None).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "'Sales'[Total]  measure\n     2:5  SUM(Sales[Amount])\n     2:5  + SUM(Sales[Tax])\n\n\
             2 matching lines in 1 object\n"
        );
    }

    #[test]
    fn human_says_so_when_nothing_matches() {
        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output(Vec::new()), None).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "No matches for \"sum\"\n");
    }

    #[test]
    fn plain_is_one_tab_separated_record_per_line() {
        let mut out = Vec::new();
        plain(&mut out, &output(vec![hit("\tSUM(x)", 1..4)])).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "measure\t'Sales'[Total]\tdax\texpression\t2\t2\t SUM(x)\n"
        );
    }
}
