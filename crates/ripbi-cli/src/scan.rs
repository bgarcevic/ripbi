//! The `scan` orchestration: resolve a target (PATH, config, or discovery),
//! ingest through `ripbi-core`, build the dependency graph, and hand the
//! findings to `render`. Every decision here is about *which* folders to feed
//! core and *what to say* — no analysis logic lives in this crate.

use std::collections::HashSet;
use std::io::{self, BufRead, IsTerminal};
use std::path::{Path, PathBuf};

use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest::{self, SkipKind, SkipNotice};
use ripbi_core::{ObjectId, ReportModel};

use crate::cli::ScanArgs;
use crate::config;
use crate::discover::{self, Candidate, Resolution};
use crate::error::ScanError;
use crate::glob;
use crate::render::{self, Finding, ScanOutput, SkipNoticeOut, UsedByOut};
use crate::style::Palette;

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
    let palette_err = Palette::detect(io::stderr().is_terminal(), args.no_color);
    match scan(args, cwd, streams, &palette_err) {
        Ok(code) => code,
        Err(error) => {
            if !args.quiet {
                let _ = writeln!(
                    streams.err,
                    "{} {}",
                    palette_err.red("error:"),
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

fn scan(
    args: &ScanArgs,
    cwd: &Path,
    streams: &mut Streams<'_>,
    palette_err: &Palette,
) -> Result<i32, ScanError> {
    let palette_out = Palette::detect(io::stdout().is_terminal(), args.no_color);
    let loaded = config::find_in(cwd)?;
    let config = loaded.map(|loaded| loaded.config);

    // Target: PATH argument, else config `target`, else folder discovery.
    let explicit = args
        .path
        .clone()
        .or_else(|| config.as_ref().and_then(|target| target.target.clone()));
    let (paired, mut announce) = match explicit {
        Some(path) => (resolve_explicit(&path)?, Vec::new()),
        None => discover_target(args, cwd, streams, palette_err)?,
    };

    // Report roots: discovered siblings, plus --report flags (which replace
    // the config's `reports`), deduplicated.
    let extras: Vec<PathBuf> = if args.reports.is_empty() {
        config
            .as_ref()
            .map(|config| config.reports.clone())
            .unwrap_or_default()
    } else {
        args.reports.clone()
    };
    let mut report_paths = paired.reports.clone();
    for extra in &extras {
        if !is_report_dir(extra) {
            return Err(ScanError::new(format!(
                "--report {} is not a report folder",
                extra.display()
            ))
            .with_hint(
                "point --report at a .Report folder (or any folder holding a report.json)",
            ));
        }
        report_paths.push(extra.clone());
    }
    let report_paths = dedupe(report_paths);

    if report_paths.is_empty() {
        return Err(ScanError::new(format!(
            "nothing to scan against: {} has no reports",
            paired.model.display()
        ))
        .with_hint(
            "reachability starts from report bindings — pass one with --report <path>, \
             or scan a .pbip project folder",
        ));
    }

    if !args.quiet {
        announce.push(format!(
            "Scanning {} with {} report(s)",
            paired.model.display(),
            report_paths.len()
        ));
        for line in &announce {
            writeln!(streams.err, "{line}").map_err(ScanError::from)?;
        }
        writeln!(
            streams.err,
            "Note: analysis covers only the ingested reports; \
             external consumers (thin reports, Excel, XMLA) are invisible."
        )
        .map_err(ScanError::from)?;
    }

    // Ingest: the model, then every report as a root.
    let model = ingest::semantic_model(&paired.model).map_err(|error| {
        ScanError::new(format!("cannot ingest {}: {error}", paired.model.display()))
    })?;
    let mut skips: Vec<SkipNoticeOut> = model.skips.iter().map(skip_notice_out).collect();
    let mut reports = Vec::new();
    for path in &report_paths {
        let ingested = ingest::report(path).map_err(|error| {
            ScanError::new(format!("cannot ingest report {}: {error}", path.display()))
        })?;
        skips.extend(ingested.skips.iter().map(skip_notice_out));
        reports.push(ingested.value);
    }

    // Analysis: entirely core's job.
    let report_refs: Vec<&ReportModel> = reports.iter().collect();
    let graph = DependencyGraph::build(&model.value, &report_refs);
    let unused = graph.unused_objects();
    let objects = graph.object_ids().count();
    let roots = graph.roots().len();
    let reachable = objects - unused.len();

    // Presentation-level suppression: [scan].ignore patterns.
    let patterns: &[String] = config
        .as_ref()
        .map(|config| config.ignore.as_slice())
        .unwrap_or(&[]);
    let mut findings = Vec::new();
    let mut ignored = 0;
    for finding in unused {
        if is_ignored(&finding.id, patterns) {
            ignored += 1;
            continue;
        }
        findings.push(Finding {
            kind: render::kind_of(&finding.id),
            id: finding.id.to_string(),
            used_by: finding
                .used_by
                .iter()
                .map(|used| UsedByOut {
                    id: used.id.to_string(),
                    provenance: used.provenance.to_string(),
                    also_unused: used.also_unused,
                })
                .collect(),
            named_in_power_query: render::power_query_labels(&finding.named_by_m),
        });
    }

    let output = ScanOutput {
        target: paired.model.display().to_string(),
        reports: report_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        objects,
        reachable,
        roots,
        unused_raw: objects - reachable,
        ignored,
        findings,
        skips,
    };

    if !args.quiet {
        if args.json {
            render::json(streams.out, &output).map_err(ScanError::from)?;
        } else if args.plain {
            render::plain(streams.out, &output).map_err(ScanError::from)?;
        } else if args.summary {
            render::human_summary(streams.out, &palette_out, &output).map_err(ScanError::from)?;
        } else {
            render::human(streams.out, &palette_out, &output).map_err(ScanError::from)?;
        }
        // Notices are stderr's job in text modes; --json carries them itself.
        if !args.json && !output.skips.is_empty() {
            writeln!(
                streams.err,
                "{} skip notice(s) from parsing (unexpected schema drift):",
                output.skips.len()
            )
            .map_err(ScanError::from)?;
            for skip in &output.skips {
                let location = skip
                    .location
                    .as_ref()
                    .map(|location| format!(":{location}"))
                    .unwrap_or_default();
                writeln!(
                    streams.err,
                    "  - {}{location} [{}] {}",
                    skip.path, skip.kind, skip.detail
                )
                .map_err(ScanError::from)?;
            }
        }
    }

    if args.strict && !output.skips.is_empty() {
        Ok(EXIT_ERROR)
    } else if output.findings.is_empty() {
        Ok(EXIT_CLEAN)
    } else {
        Ok(EXIT_UNUSED)
    }
}

/// Resolves an explicit PATH (argument or config `target`).
fn resolve_explicit(path: &Path) -> Result<discover::Paired, ScanError> {
    if !path.exists() {
        return Err(ScanError::new(format!("no such path: {}", path.display()))
            .with_hint("pass a .pbip file, a project folder, a .SemanticModel, or a .Report"));
    }
    match discover::resolve_path(path)? {
        Resolution::Paired(paired) => Ok(paired),
        Resolution::Ambiguous { dir, candidates } => Err(ambiguous_error(&dir, &candidates)),
        Resolution::Archive(archive) => Err(discover::archive_error(&archive)),
        Resolution::Unrecognized(unrecognized) => Err(ScanError::new(format!(
            "not a Power BI project: {}",
            unrecognized.display()
        ))
        .with_hint("point PATH at a .pbip file, a project folder, a .SemanticModel, or a .Report")),
    }
}

/// Finds the scan target in `cwd` when no PATH or config target names one.
fn discover_target(
    args: &ScanArgs,
    cwd: &Path,
    streams: &mut Streams<'_>,
    palette_err: &Palette,
) -> Result<(discover::Paired, Vec<String>), ScanError> {
    let candidates = discover::discover(cwd);
    match candidates.as_slice() {
        [] => Err(
            ScanError::new(format!("no Power BI projects found in {}", cwd.display()))
                .with_hint("run inside a .pbip project folder, or pass a PATH"),
        ),
        [only] => {
            let paired = discover::pair(only)?;
            Ok((paired, vec![format!("Discovered '{}'.", only.label)]))
        }
        many => {
            if args.no_input || args.quiet || !streams.stdin_is_tty {
                return Err(ambiguous_error(cwd, many));
            }
            let candidate = pick(many.to_vec(), cwd, streams, palette_err)?;
            let paired = discover::pair(&candidate)?;
            Ok((paired, vec![format!("Selected '{}'.", candidate.label)]))
        }
    }
}

/// The interactive project picker: a numbered list on stderr, a choice read
/// from stdin. Only reached on a TTY stdin.
fn pick(
    mut candidates: Vec<Candidate>,
    dir: &Path,
    streams: &mut Streams<'_>,
    palette_err: &Palette,
) -> Result<Candidate, ScanError> {
    writeln!(
        streams.err,
        "{}",
        palette_err.bold(&format!("Multiple projects in {}:", dir.display()))
    )
    .map_err(ScanError::from)?;
    for (index, candidate) in candidates.iter().enumerate() {
        writeln!(streams.err, "  {}. {}", index + 1, candidate.label).map_err(ScanError::from)?;
    }
    let mut reader = io::BufReader::new(&mut *streams.input);
    loop {
        write!(streams.err, "Select a project [1-{}]: ", candidates.len())
            .map_err(ScanError::from)?;
        streams.err.flush().map_err(ScanError::from)?;
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(ScanError::from)?;
        if read == 0 {
            return Err(ScanError::new("no project selected"));
        }
        match line.trim().parse::<usize>() {
            Ok(choice) if (1..=candidates.len()).contains(&choice) => {
                return Ok(candidates.remove(choice - 1));
            }
            _ => writeln!(
                streams.err,
                "Enter a number between 1 and {}.",
                candidates.len()
            )
            .map_err(ScanError::from)?,
        }
    }
}

fn ambiguous_error(dir: &Path, candidates: &[Candidate]) -> ScanError {
    let listing: String = candidates
        .iter()
        .map(|candidate| format!("\n  - {}", candidate.label))
        .collect();
    ScanError::new(format!("multiple projects in {}:{listing}", dir.display()))
        .with_hint("pass a PATH to scan one project; --no-input keeps this non-interactive")
}

fn is_report_dir(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.ends_with(".report")
        || path.join("report.json").is_file()
        || path.join("definition").join("report.json").is_file()
}

fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut kept = Vec::new();
    for path in paths {
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            kept.push(path);
        }
    }
    kept
}

fn is_ignored(id: &ObjectId, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let display = id.to_string();
    patterns.iter().any(|pattern| {
        glob::matches(pattern, &display)
            || bare_names(id)
                .iter()
                .any(|name| glob::matches(pattern, name))
    })
}

/// The bare names an ignore pattern can match: the leaf name, and for
/// relationships both endpoint columns.
fn bare_names(id: &ObjectId) -> Vec<&str> {
    match id {
        ObjectId::Table { table } => vec![table.as_str()],
        ObjectId::Column { column, .. } => vec![column.as_str()],
        ObjectId::Measure { measure, .. } => vec![measure.as_str()],
        ObjectId::Hierarchy { hierarchy, .. } => vec![hierarchy.as_str()],
        ObjectId::Partition { partition, .. } => vec![partition.as_str()],
        ObjectId::Relationship {
            from_column,
            to_column,
            ..
        } => vec![from_column.as_str(), to_column.as_str()],
        ObjectId::Role { role } => vec![role.as_str()],
        ObjectId::CalculationItem { item, .. } => vec![item.as_str()],
        ObjectId::Expression { name } => vec![name.as_str()],
        ObjectId::Function { name } => vec![name.as_str()],
        ObjectId::ReportMeasure { measure } => vec![measure.as_str()],
    }
}

fn skip_notice_out(notice: &SkipNotice) -> SkipNoticeOut {
    SkipNoticeOut {
        path: notice.path.display().to_string(),
        location: notice.location.clone(),
        kind: skip_kind_out(notice.kind),
        detail: notice.detail.clone(),
    }
}

fn skip_kind_out(kind: SkipKind) -> &'static str {
    match kind {
        SkipKind::UnknownObject => "unknown_object",
        SkipKind::UnknownProperty => "unknown_property",
        SkipKind::MalformedValue => "malformed_value",
        SkipKind::UnresolvedAlias => "unresolved_alias",
    }
}
