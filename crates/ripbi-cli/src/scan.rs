//! The `scan` command: flags in, rendered findings and an exit code out. The
//! pipeline itself — target resolution, ingestion, the dependency graph, and
//! bucketing the findings — is `ripbi_app::scan`; this module turns the
//! flags into a [`ScanRequest`], prints what the run reports on stderr, and
//! renders the outcome in the mode the flags pick.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use ripbi_app::scan::{
    CompareRequest, Discovered, Note, NoteLevel, Observer, Scan, ScanOutcome, ScanRequest,
    SkipReason, Stage, Target, Verdict,
};
use ripbi_core::ObjectId;
use ripbi_core::ingest;

use crate::cli::{EmitFormat, ScanArgs, SortKey};
use crate::discover::{self, Candidate};
use crate::error::ScanError;
use crate::markdown;
use crate::picker;
use crate::progress::{self, Progress};
use crate::render::{self, ScanOutput, SkipNoticeOut};
use crate::sarif::{self, Locator};
use crate::style::Palette;
use crate::tx_rm;
use crate::vso;

pub use ripbi_app::scan::{
    ModelScan, TargetInput, capped_names, dedupe, query_log_item, query_log_path, report_name,
    skip_notice_out,
};

/// Exit code: no unused objects.
pub const EXIT_CLEAN: i32 = 0;
/// Exit code: unused objects were found.
pub const EXIT_UNUSED: i32 = 1;
/// Exit code: the scan could not run, or `--strict` surfaced skip notices.
pub const EXIT_ERROR: i32 = 2;

/// The streams one scan reads and writes. Injectable so tests drive `scan`
/// against in-memory buffers instead of the process's terminals.
pub struct Streams<'a> {
    /// Machine-readable results (findings, summary, JSON).
    pub out: &'a mut dyn io::Write,
    /// Messages: announcements, the coverage caveat, notices, errors.
    pub err: &'a mut dyn io::Write,
    /// The picker reads project choices from here.
    pub input: &'a mut dyn io::Read,
    /// Whether `input` is interactive. A generic reader cannot answer this
    /// itself, so the caller states it (the binary inspects the real stdin).
    pub stdin_is_tty: bool,
    /// Whether `out` is a terminal. The renderer picks the plain-text or ANSI
    /// palette from this, not from the process stdout — a test driving an
    /// in-memory buffer must not inherit the terminal it happens to run in.
    pub stdout_is_tty: bool,
    /// Whether `err` is a terminal. The ambient update notifier only prints
    /// on an interactive stderr, so the caller states this too.
    pub stderr_is_tty: bool,
}

/// Runs `scan` against the process's working directory.
#[must_use = "the return value is the process exit code"]
pub fn run(args: &ScanArgs, streams: &mut Streams<'_>) -> i32 {
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

/// Runs `scan` against an explicit working directory (tests pass one).
#[must_use = "the return value is the process exit code"]
pub fn run_in(args: &ScanArgs, cwd: &Path, streams: &mut Streams<'_>) -> i32 {
    let palette_err = Palette::detect(streams.stderr_is_tty, args.no_color);
    match scan(args, cwd, streams, &palette_err) {
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

/// One scan: the request built from the flags goes to [`ripbi_app::scan::run`],
/// whose notes, stages, and picker land on stderr through [`CliObserver`];
/// the outcome is rendered here and mapped to an exit code.
fn scan(
    args: &ScanArgs,
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
        Progress::wanted(
            streams,
            palette_err,
            args.quiet,
            args.json || args.plain || args.sarif || args.azure_devops || args.emit.is_some(),
        ),
        *palette_err,
        progress::SCAN,
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
    let scanned = ripbi_app::scan::run(
        &request(args, cwd),
        &mut CliObserver {
            streams: &mut *streams,
            palette: palette_err,
            progress: Some(&progress),
            quiet: args.quiet,
            verbose: Some(args.verbose),
        },
    )?;
    let outcome = match scanned {
        Scan::Done(outcome) => *outcome,
        Scan::Skipped { model, reason } => {
            if !args.quiet {
                let reason = match reason {
                    SkipReason::NoReports { detail } => format!(
                        "no connected reports{}",
                        detail
                            .map(|detail| format!(" ({detail})"))
                            .unwrap_or_default()
                    ),
                    SkipReason::OnlyStubs => "no connected reports (only ripbi stubs)".to_string(),
                };
                writeln!(streams.err, "Skipped {}: {reason}", model.display())
                    .map_err(ScanError::from)?;
            }
            return Ok(EXIT_CLEAN);
        }
    };
    let verdict = outcome.verdict(args.strict);
    let broken_only = outcome.broken_only();
    let ScanOutcome {
        mut output,
        existing: kept_existing,
        locator,
        removable,
        model,
        config,
        workbooks,
        ..
    } = outcome;
    output.elapsed = progress.elapsed();
    // The header already named the model; the elapsed time rides on the
    // verdict line, so the ticker just clears.
    progress.finish(streams.err).map_err(ScanError::from)?;

    write_side_outputs(args, cwd, &output, &kept_existing, locator.as_ref())?;

    let human =
        !(args.json || args.plain || args.sarif || args.azure_devops || args.emit.is_some());
    if !args.quiet {
        // Notices are stderr's job in text modes; --json carries them itself.
        // They print above the report, so the verdict stays the last line.
        if !args.json {
            write_skip_notices(streams.err, &output.skips)?;
        }
        if human {
            writeln!(streams.err).map_err(ScanError::from)?;
        }
        if args.json {
            render::json(streams.out, &output).map_err(ScanError::from)?;
        } else if let Some(locator) = locator.as_ref().filter(|_| args.sarif || args.azure_devops) {
            if args.azure_devops {
                vso::write(streams.out, &output, locator, cwd)
            } else {
                sarif::write(streams.out, &output, &kept_existing, locator, cwd)
            }
            .map_err(ScanError::from)?;
        } else if args.emit == Some(EmitFormat::TxRm) {
            let removable: Vec<&ObjectId> = removable.iter().collect();
            tx_rm::write(streams.out, &output.target, &model, &removable)
                .map_err(ScanError::from)?;
        } else if args.plain {
            render::plain(streams.out, &output).map_err(ScanError::from)?;
        } else if args.summary {
            render::human_summary(streams.out, &palette_out, &output, broken_only)
                .map_err(ScanError::from)?;
        } else {
            render::human(
                streams.out,
                &palette_out,
                &output,
                args.power_query,
                broken_only,
            )
            .map_err(ScanError::from)?;
        }
        if !args.json && !output.findings.is_empty() {
            let vouched = config
                .as_ref()
                .is_some_and(|config| config.no_external_consumers);
            if !vouched {
                let hint = consumer_hint(
                    query_log_path(args.queries_from.as_deref(), config.as_ref()).is_some(),
                    workbooks,
                    "removing",
                );
                writeln!(streams.err, "  {}", notes.0.dim(&hint))?;
            }
            if output.compare.is_some() {
                writeln!(streams.err, "hint: {}", render::KEEP_HINT)?;
            }
        }
    }

    Ok(match verdict {
        Verdict::Clean => EXIT_CLEAN,
        Verdict::Findings => EXIT_UNUSED,
        Verdict::Incomplete => EXIT_ERROR,
    })
}

/// The scan the flags ask for. `--compare-root` adds the same request with
/// every relative flag path rebased onto the other checkout.
fn request(args: &ScanArgs, cwd: &Path) -> ScanRequest {
    let wants_sarif = args.sarif || args.sarif_file.is_some();
    let compare = args.compare_root.as_ref().map(|root| {
        let dir = cwd.join(root);
        let rebase = |path: &PathBuf| {
            if path.is_absolute() {
                path.clone()
            } else {
                dir.join(path)
            }
        };
        let before = ScanRequest {
            cwd: dir.clone(),
            path: args.path.as_ref().map(rebase),
            model: args.model.as_ref().map(rebase),
            reports: args.reports.iter().map(rebase).collect(),
            quiet: true,
            no_input: true,
            // Sizes never change a verdict; skip the storage source entirely.
            stats_from: Some(PathBuf::from("none")),
            locate: false,
            keep_existing: false,
            compare: None,
            ..request(
                &ScanArgs {
                    compare_root: None,
                    ..args.clone()
                },
                &dir,
            )
        };
        CompareRequest {
            root: root.clone(),
            label: args.compare_label.clone(),
            before: Box::new(before),
        }
    });
    ScanRequest {
        cwd: cwd.to_path_buf(),
        model: args.model.clone(),
        path: args.path.clone(),
        reports: args.reports.clone(),
        no_input: args.no_input,
        quiet: args.quiet,
        verbose: args.verbose,
        allow_no_reports: args.allow_no_reports,
        broken: args.broken,
        types: args.types.clone(),
        stats_from: args.stats_from.clone(),
        queries_from: args.queries_from.clone(),
        queries_item: args.queries_item.clone(),
        sort_by_size: args.sort == SortKey::Size,
        // The model table travels with a finding only where a mode reads it:
        // `--json` (the `table` field) and `--summary` and Markdown (the
        // worst-tables breakdown).
        with_tables: args.json
            || args.summary
            || args.json_file.is_some()
            || args.markdown_file.is_some(),
        // `--sarif` (and `--sarif-file`) and `--azure-devops` need every
        // finding's source file; SARIF also keeps what already existed as
        // suppressed results (issue #142).
        locate: wants_sarif || args.azure_devops,
        keep_existing: wants_sarif,
        compare,
    }
}

/// The CLI side of a running scan: stages drive the ticker, notes and the
/// header print on stderr (unless `-q`), and the picker prompts on a TTY.
pub(crate) struct CliObserver<'s, 'a> {
    pub(crate) streams: &'s mut Streams<'a>,
    pub(crate) palette: &'s Palette,
    pub(crate) progress: Option<&'s Progress>,
    pub(crate) quiet: bool,
    /// `None` for a command without `--verbose`.
    pub(crate) verbose: Option<bool>,
}

impl Observer for CliObserver<'_, '_> {
    fn stage(&mut self, stage: Stage) {
        if let Some(progress) = self.progress {
            progress.stage(stage as usize);
        }
    }

    fn resolved(&mut self, target: &Target<'_>) -> Result<(), ScanError> {
        if self.quiet {
            return Ok(());
        }
        // The header names the model and its report count; every line under
        // it is a tiered note — `!` when it changes how to read the result,
        // `·` for background — so the verdict, last, is what stands out.
        let mut announce: Vec<String> =
            target.discovered.map(discovered_line).into_iter().collect();
        announce.extend(pairing_announce(
            Notes(*self.palette),
            target.model,
            target.reports,
            target.bound,
            self.verbose,
        ));
        for line in &announce {
            writeln!(self.streams.err, "{line}").map_err(ScanError::from)?;
        }
        Ok(())
    }

    /// `report --allow-no-model`: the header names the reports alone.
    fn unpaired(&mut self, reports: &[PathBuf]) -> Result<(), ScanError> {
        if self.quiet {
            return Ok(());
        }
        let label = match reports {
            [only] => report_name(only),
            paths => format!("{} reports", paths.len()),
        };
        writeln!(self.streams.err, "{} · no model", self.palette.bold(&label))
            .map_err(ScanError::from)
    }

    fn note(&mut self, note: Note) -> Result<(), ScanError> {
        if self.quiet {
            return Ok(());
        }
        writeln!(self.streams.err, "{}", Notes(*self.palette).render(&note))
            .map_err(ScanError::from)
    }

    /// The interactive project picker (the shared [`picker`]). Only on a TTY
    /// stdin; anything else refuses with the candidate list.
    fn pick(&mut self, dir: &Path, candidates: &[Candidate]) -> Result<Option<usize>, ScanError> {
        if !self.streams.stdin_is_tty {
            return Ok(None);
        }
        let labels: Vec<String> = candidates
            .iter()
            .map(|candidate| candidate.label.clone())
            .collect();
        picker::pick(
            &format!("Multiple projects in {}:", dir.display()),
            "Select a project",
            &labels,
            self.streams,
            self.palette,
        )
        .map(Some)
    }
}

/// The line that names a target cwd discovery found.
fn discovered_line(discovered: &Discovered) -> String {
    match discovered {
        Discovered::Only(label) => format!("Discovered '{label}'."),
        Discovered::Selected(label) => format!("Selected '{label}'."),
    }
}

/// Prints the grouped skip-notice block on stderr — the one every text-mode
/// command shares. Not all notices are drift — a `stale_state` notice reports
/// understood saved state pointing at deleted objects — so the header stays
/// kind-neutral; each line's `[kind]` says which it is.
pub(crate) fn write_skip_notices(
    err: &mut dyn io::Write,
    skips: &[SkipNoticeOut],
) -> Result<(), ScanError> {
    if skips.is_empty() {
        return Ok(());
    }
    writeln!(err, "{} skip notice(s) from parsing:", skips.len()).map_err(ScanError::from)?;
    for skip in skips {
        let location = skip
            .location
            .as_ref()
            .map(|location| format!(":{location}"))
            .unwrap_or_default();
        writeln!(
            err,
            "  - {}{location} [{}] {}",
            skip.path, skip.kind, skip.detail
        )
        .map_err(ScanError::from)?;
    }
    Ok(())
}

/// The by-name pairing notes collapsed to one line per `initial catalog`,
/// in first-appearance order. Names are listed up to three per catalog; a
/// longer tail becomes `… and N more` so the line stays one line (issue #65).
pub(crate) fn collapse_name_matched(
    name_matched: &[(PathBuf, String)],
    notes: Notes,
    has_verbose: bool,
) -> Vec<String> {
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    let mut positions: HashMap<&str, usize> = HashMap::new();
    for (path, catalog) in name_matched {
        match positions.get(catalog.as_str()) {
            Some(&position) => grouped[position].1.push(report_name(path)),
            None => {
                positions.insert(catalog, grouped.len());
                grouped.push((catalog.clone(), vec![report_name(path)]));
            }
        }
    }
    grouped
        .into_iter()
        .map(|(catalog, names)| {
            let mut line = format!(
                "{} report(s) matched by dataset name only \
                 (byConnection 'initial catalog' = '{catalog}'): {}",
                names.len(),
                capped_names(&names)
            );
            if names.len() > 3 && has_verbose {
                line.push_str(VERBOSE_POINTER);
            }
            notes.warn(&line)
        })
        .collect()
}

/// The suffix that points at `--verbose` when a capped list left names out —
/// the one place the full trail lives.
pub(crate) const VERBOSE_POINTER: &str = " — rerun with --verbose to list them";

/// How every model command prints its stderr notes, tiered under its
/// header: `!` for what changes how to read the result, `·` for background,
/// so the closing line stands out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Notes(pub(crate) Palette);

impl Notes {
    /// Background context. `text` carries no trailing period.
    pub(crate) fn info(self, text: &str) -> String {
        self.0.info_note(text)
    }

    /// A note that changes how to read the result.
    pub(crate) fn warn(self, text: &str) -> String {
        self.0.warn_note(text)
    }

    /// A scan [`Note`] at its level.
    pub(crate) fn render(self, note: &Note) -> String {
        match note.level {
            NoteLevel::Info => self.info(&note.text),
            NoteLevel::Warn => self.warn(&note.text),
        }
    }
}

/// The header and pairing notes `scan`, `report`, and `deps` open with: the
/// model and its report count, then the pairing facts a walk made. The count
/// is the signal; the names are auditability, on request. The pairing facts
/// always show — the by-name match is the weakest pairing and a wrong pairing
/// means wrong results — but capped, so a hundred thin reports read as one
/// line each instead of a wall (issue #65, and the same capping for the
/// ignored list). `verbose` is `None` for a command without `--verbose`.
pub(crate) fn pairing_announce(
    notes: Notes,
    model: &Path,
    reports: &[PathBuf],
    bound: Option<&discover::BoundReports>,
    verbose: Option<bool>,
) -> Vec<String> {
    let full = verbose == Some(true);
    let mut lines = Vec::new();
    let name = if full {
        model.display().to_string()
    } else {
        report_name(model)
    };
    let count = match reports.len() {
        0 => "no reports".to_string(),
        1 => "1 report".to_string(),
        n => format!("{n} reports"),
    };
    let header = format!("{} · {count}", notes.0.bold(&name));
    if full && !reports.is_empty() {
        let names: Vec<String> = reports.iter().map(|path| report_name(path)).collect();
        lines.push(format!("{header}: {}", names.join(", ")));
    } else {
        lines.push(header);
    }
    let workbooks: Vec<String> = reports
        .iter()
        .filter(|path| path.is_file() && ingest::is_workbook(path))
        .map(|path| report_name(path))
        .collect();
    if !workbooks.is_empty() {
        lines.push(notes.info(&format!(
            "read {} Excel workbook(s): {} — every OLAP connection in them counts as \
             querying this model",
            workbooks.len(),
            if full {
                workbooks.join(", ")
            } else {
                capped_names(&workbooks)
            }
        )));
    }
    let Some(bound) = bound else {
        return lines;
    };
    if full {
        for (path, catalog) in &bound.name_matched {
            lines.push(notes.warn(&format!(
                "{} matched by dataset name only (byConnection 'initial catalog' = '{catalog}')",
                path.display()
            )));
        }
    } else {
        lines.extend(collapse_name_matched(
            &bound.name_matched,
            notes,
            verbose.is_some(),
        ));
    }
    if !bound.apps.is_empty() {
        let names: Vec<String> = bound
            .apps
            .iter()
            .map(|(path, pairing)| {
                let how = match pairing {
                    discover::AppPairing::Mapped => "ripbi.toml [apps]".to_string(),
                    discover::AppPairing::ServiceId => "fabric.yaml itemId".to_string(),
                    discover::AppPairing::Name(alias) => {
                        format!("fabric.yaml model name '{alias}'")
                    }
                };
                format!("{} ({how})", report_name(path))
            })
            .collect();
        lines.push(notes.info(&format!(
            "included {} Fabric app(s) from the search folder: {}",
            names.len(),
            if full {
                names.join(", ")
            } else {
                capped_names(&names)
            }
        )));
    }
    if !bound.apps_elsewhere.is_empty() {
        let names: Vec<String> = bound
            .apps_elsewhere
            .iter()
            .map(|(path, aliases)| {
                if aliases.is_empty() {
                    report_name(path)
                } else {
                    format!("{} ({})", report_name(path), discover::quoted_list(aliases))
                }
            })
            .collect();
        lines.push(notes.info(&format!(
            "ignored {} Fabric app(s) whose fabric.yaml names other models: {} — \
             if one reads this model, pair it in ripbi.toml [apps]",
            names.len(),
            if full {
                names.join(", ")
            } else {
                capped_names(&names)
            }
        )));
    }
    if !bound.ignored_elsewhere.is_empty() {
        let count = bound.ignored_elsewhere.len();
        let names: Vec<String> = bound
            .ignored_elsewhere
            .iter()
            .map(|path| report_name(path))
            .collect();
        let mut line = format!(
            "ignored {count} report(s) bound to other models: {}",
            if full {
                names.join(", ")
            } else {
                capped_names(&names)
            }
        );
        if count > 3 && verbose == Some(false) {
            line.push_str(VERBOSE_POINTER);
        }
        lines.push(notes.info(&line));
    }
    lines
}

/// The hint under a result whose meaning depends on consumers ripbi cannot
/// see: `scan`'s findings ("before removing"), `deps`'s Impact view ("before
/// changing it"). A query log narrows the blind spot to the consumers
/// outside its window. `[scan].external_consumers = "none"` vouches there
/// are none.
///
/// Workbooks passed as reports narrow the hint to the workbooks that were
/// not (issue #172).
pub(crate) fn consumer_hint(with_query_log: bool, with_workbooks: bool, action: &str) -> String {
    if with_query_log {
        format!(
            "check consumers outside the query log's window before {action}; ripbi can't see them"
        )
    } else if with_workbooks {
        format!(
            "check thin reports, other Excel workbooks and XMLA clients before {action}; \
             ripbi can't see them"
        )
    } else {
        format!("check thin reports, Excel and XMLA clients before {action}; ripbi can't see them")
    }
}

/// Writes the `--sarif-file`, `--json-file`, and `--markdown-file` outputs:
/// the same scan rendered to files alongside whatever stdout shows, so a CI
/// job gets its annotations, counts, and summary from one scan. `-q` silences
/// the streams, never a file the user asked for. Paths resolve against `cwd`.
fn write_side_outputs(
    args: &ScanArgs,
    cwd: &Path,
    output: &ScanOutput,
    kept_existing: &sarif::Existing,
    locator: Option<&Locator>,
) -> Result<(), ScanError> {
    if let (Some(path), Some(locator)) = (&args.sarif_file, locator) {
        write_file("--sarif-file", path, cwd, |out| {
            sarif::write(out, output, kept_existing, locator, cwd)
        })?;
    }
    if let Some(path) = &args.json_file {
        write_file("--json-file", path, cwd, |out| render::json(out, output))?;
    }
    if let Some(path) = &args.markdown_file {
        write_file("--markdown-file", path, cwd, |out| {
            markdown::write(out, output)
        })?;
    }
    Ok(())
}

/// Renders one side output into memory, then writes it to `path` (resolved
/// against `cwd`) in one go.
fn write_file(
    flag: &str,
    path: &Path,
    cwd: &Path,
    render: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
) -> Result<(), ScanError> {
    let mut buffer = Vec::new();
    render(&mut buffer).map_err(ScanError::from)?;
    std::fs::write(cwd.join(path), buffer)
        .map_err(|error| ScanError::new(format!("cannot write {flag} {}: {error}", path.display())))
}
