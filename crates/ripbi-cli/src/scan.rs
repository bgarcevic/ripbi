//! The `scan` orchestration: resolve a target (PATH, config, or discovery),
//! ingest through `ripbi-core`, build the dependency graph, and hand the
//! findings to `render`. Every decision here is about *which* folders to feed
//! core and *what to say* — no analysis logic lives in this crate.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};

use ripbi_core::graph::{BrokenReason, DependencyGraph};
use ripbi_core::ingest::{self, SkipKind, SkipNotice};
use ripbi_core::{NameKey, ObjectId, ReportModel};

use crate::cli::ScanArgs;
use crate::config;
use crate::discover::{self, Candidate, Resolution};
use crate::error::ScanError;
use crate::glob;
use crate::render::{
    self, AutoDateTimeRow, BrokenOut, Finding, ScanOutput, SkipNoticeOut, UsedByOut,
};
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
    let palette_out = Palette::detect(streams.stdout_is_tty, args.no_color);
    let loaded = config::find_in(cwd)?;
    let config = loaded.map(|loaded| loaded.config);

    // Report roots: --report flags replace the config's `reports`; a plain
    // folder among them becomes a search root in model mode, or when an
    // explicit PATH names a semantic model (issue #67).
    let extras: Vec<PathBuf> = if args.reports.is_empty() {
        config
            .as_ref()
            .map(|config| config.reports.clone())
            .unwrap_or_default()
    } else {
        args.reports.clone()
    };

    // Target: --model (explicit, picker-free), else PATH argument, else config
    // `target`, else folder discovery.
    let mut model_scan: Option<ModelScan> = None;
    let (paired, mut announce) = if let Some(model_path) = &args.model {
        let target = discover::resolve_model(model_path)?;
        let mut direct = Vec::new();
        let mut search_roots = Vec::new();
        for extra in &extras {
            partition_report_value(extra, &mut direct, &mut search_roots)?;
        }
        if direct.is_empty() && search_roots.is_empty() {
            search_roots.push(default_search_root(&target.item_root));
        }
        let excluded: HashSet<PathBuf> = direct
            .iter()
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()))
            .collect();
        let bound = discover::discover_bound_reports(&target, &search_roots, &excluded);
        let mut reports = bound.reports.clone();
        reports.extend(direct.iter().cloned());
        let paired = discover::Paired {
            model: target.item_root,
            reports,
        };
        model_scan = Some(ModelScan {
            bound,
            search_roots,
        });
        (paired, Vec::new())
    } else {
        let explicit = args
            .path
            .clone()
            .or_else(|| config.as_ref().and_then(|target| target.target.clone()));
        let (paired, announce, model_named) = match explicit {
            Some(path) => {
                let model_named = names_semantic_model(&path);
                (resolve_explicit(&path)?, Vec::new(), model_named)
            }
            None => {
                let (paired, announce) = discover_target(args, cwd, streams, palette_err)?;
                (paired, announce, false)
            }
        };
        let mut report_paths = paired.reports.clone();
        let mut direct = Vec::new();
        let mut search_roots = Vec::new();
        for extra in &extras {
            if is_report_item(extra) {
                direct.push(extra.clone());
                continue;
            }
            if extra.is_dir() {
                if is_report_suffixed(extra) {
                    return Err(malformed_report_folder_error(extra));
                }
                // An explicit PATH that names a semantic model makes a plain
                // folder a search root, as in model mode (issue #67); every
                // other target keeps report-items-only `--report`.
                if model_named {
                    search_roots.push(extra.clone());
                    continue;
                }
                return Err(plain_report_folder_error(extra, &paired.model));
            }
            if !extra.exists() {
                return Err(ScanError::new(format!("no such path: {}", extra.display())).with_hint(
                    "point --report at an existing .Report folder (or a folder holding report.json)",
                ));
            }
            return Err(
                ScanError::new(format!("--report {} is not a folder", extra.display())).with_hint(
                    "point --report at a .Report folder (or a folder holding report.json)",
                ),
            );
        }
        if search_roots.is_empty() {
            report_paths.extend(direct);
        } else {
            // The walk never revisits face-value reports: convention siblings
            // and explicit items are excluded from it, so the union stays
            // duplicate-free and none of them produces a walk notice.
            let target = discover::resolve_model(&paired.model)?;
            let mut excluded: HashSet<PathBuf> = direct
                .iter()
                .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()))
                .collect();
            excluded.extend(
                paired
                    .reports
                    .iter()
                    .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone())),
            );
            let bound = discover::discover_bound_reports(&target, &search_roots, &excluded);
            report_paths.extend(direct);
            report_paths.extend(bound.reports.iter().cloned());
            model_scan = Some(ModelScan {
                bound,
                search_roots,
            });
        }
        (
            discover::Paired {
                model: paired.model,
                reports: report_paths,
            },
            announce,
        )
    };
    let report_paths = dedupe(paired.reports);

    if report_paths.is_empty() {
        if let Some(scan) = &model_scan {
            return Err(no_bound_reports_error(&paired.model, scan));
        }
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
        match &model_scan {
            Some(scan) => {
                // The count is the signal; the names are auditability, on
                // request. The pairing *facts* always show — the by-name
                // match is the weakest pairing and a wrong pairing means
                // wrong findings — but capped, so a hundred thin reports
                // read as one line each instead of a wall (issue #65, and
                // the same capping for the ignored list).
                if args.verbose {
                    let names: Vec<String> =
                        report_paths.iter().map(|path| report_name(path)).collect();
                    announce.push(format!(
                        "Scanning {} with {} report(s): {}",
                        paired.model.display(),
                        report_paths.len(),
                        names.join(", ")
                    ));
                    for (path, catalog) in &scan.bound.name_matched {
                        announce.push(format!(
                            "Note: {} matched by dataset name only (byConnection 'initial catalog' = '{catalog}').",
                            path.display()
                        ));
                    }
                } else {
                    announce.push(format!(
                        "Scanning {} with {} report(s)",
                        paired.model.display(),
                        report_paths.len()
                    ));
                    announce.extend(collapse_name_matched(&scan.bound.name_matched));
                }
                if !scan.bound.ignored_elsewhere.is_empty() {
                    let count = scan.bound.ignored_elsewhere.len();
                    if args.verbose {
                        let names: Vec<String> = scan
                            .bound
                            .ignored_elsewhere
                            .iter()
                            .map(|path| report_name(path))
                            .collect();
                        announce.push(format!(
                            "Ignored {count} report(s) bound to other models: {}",
                            names.join(", ")
                        ));
                    } else {
                        let names: Vec<String> = scan
                            .bound
                            .ignored_elsewhere
                            .iter()
                            .map(|path| report_name(path))
                            .collect();
                        let mut line = format!(
                            "Ignored {count} report(s) bound to other models: {}",
                            capped_names(&names)
                        );
                        if count > 3 {
                            line.push_str(VERBOSE_POINTER);
                        }
                        announce.push(line);
                    }
                }
            }
            None => announce.push(format!(
                "Scanning {} with {} report(s)",
                paired.model.display(),
                report_paths.len()
            )),
        }
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
    if let Some(scan) = &model_scan {
        // Unresolved dataset references and anchor-less `.Report` folders are
        // notices like any parser skip: stderr, the JSON `skips` array, and
        // `--strict` all see them. Bound-elsewhere reports deliberately stay
        // out, so a healthy multi-model folder can still pass `--strict`.
        skips.extend(
            scan.bound
                .unresolved
                .iter()
                .map(|(path, detail)| SkipNoticeOut {
                    path: path.display().to_string(),
                    location: None,
                    kind: "unresolved_dataset_reference",
                    detail: detail.clone(),
                }),
        );
        skips.extend(scan.bound.malformed.iter().map(|path| SkipNoticeOut {
            path: path.display().to_string(),
            location: None,
            kind: "malformed_report_item",
            detail: "missing report.json".to_string(),
        }));
        skips.extend(scan.bound.parse_skips.iter().map(skip_notice_out));
    }

    // Analysis: entirely core's job.
    let report_refs: Vec<&ReportModel> = reports.iter().collect();
    let graph = DependencyGraph::build(&model.value, &report_refs);
    let unused = graph.unused_objects();
    let verdicts = graph.auto_date_time_tables(&model.value);
    let objects = graph.object_ids().count();
    let roots = graph.roots().len();
    let reachable = objects - unused.len();

    // The auto date/time machinery's members are never standalone findings
    // (issue #47): GUID-named columns reading as orphans is exactly the noise
    // the section exists to prevent. The section's one row per machinery
    // table — verdict, the date column it serves, and the dead table's own
    // chain — is the deliberate surface; a table only ever goes away with its
    // members.
    let machinery: HashSet<NameKey> = model
        .value
        .tables
        .iter()
        .filter(|table| table.is_local_date_table || table.is_template_date_table)
        .map(|table| NameKey::new(&table.name))
        .collect();

    // Presentation-level suppression: [scan].ignore patterns.
    let patterns: &[String] = config
        .as_ref()
        .map(|config| config.ignore.as_slice())
        .unwrap_or(&[]);
    let mut findings = Vec::new();
    let mut ignored = 0;

    // The type flags narrow what is reported (issue #31): findings they hide
    // are counted in `filtered_out`, and the auto date/time section — which
    // is table-shaped — prints and gates the exit code only when tables are
    // among the reported kinds. `--broken` selects the breakage kind the same
    // way, and is the only selection under which breakage gates the exit
    // code (issue #60: advisory until asked).
    let selected = args.selected_kinds();
    let section_visible = selected
        .as_ref()
        .is_none_or(|kinds| kinds.contains("table"));
    let broken_selected = selected
        .as_ref()
        .is_none_or(|kinds| kinds.contains("broken_visual"));
    // Gating is not reporting: with no flags everything is *reported*, but
    // breakage gates the exit code only when `--broken` explicitly selected
    // it (issue #60: advisory until asked).
    let broken_gating = selected
        .as_ref()
        .is_some_and(|kinds| kinds.contains("broken_visual"));
    // Issue #60's precision bar: a "broken" claim is itself a breakage
    // claim, so it fires only when nothing in the model ingest could have
    // hidden the name the binding wrote. That is exactly the
    // `unknown_object` kind — a skipped table or column never became a
    // node, so a "field not found" verdict would be a guess. Property-level
    // drift (`unknown_property`) cannot hide a name: the object was parsed
    // with it regardless. Report-side skips never suppress either way — a
    // half-parsed report can only under-report breakage, never fabricate it.
    let hides_a_name = model
        .skips
        .iter()
        .any(|skip| matches!(skip.kind, SkipKind::UnknownObject));

    // One verdict row per auto date/time table the ignore list does not
    // suppress; a dead table's own finding is filed under its row so the
    // verdict and the dead-chain note read together. Filed by object identity —
    // display ids are rendering, not keys.
    let mut auto_date_time: Vec<AutoDateTimeRow> = Vec::new();
    let mut row_by_table: HashMap<&ObjectId, usize> = HashMap::new();
    for verdict in &verdicts {
        if !section_visible || is_ignored(&verdict.id, patterns) {
            continue;
        }
        row_by_table.insert(&verdict.id, auto_date_time.len());
        auto_date_time.push(AutoDateTimeRow {
            verdict: render::verdict_of(verdict.verdict),
            id: verdict.id.to_string(),
            source_column: verdict.source_column.as_ref().map(ToString::to_string),
            finding: None,
        });
    }

    // The model table travels with a finding only where a mode reads it: `--json`
    // (the `table` field) and `--summary` (the worst-tables breakdown). Cloning
    // it for the default and `--plain` output would allocate for nothing.
    let want_table = args.json || args.summary;
    let mut filtered_out = 0;
    let mut machinery_members = 0;
    for finding in unused {
        if is_machinery_member(&finding.id, &machinery) {
            machinery_members += 1;
            continue;
        }
        if is_ignored(&finding.id, patterns) {
            ignored += 1;
            continue;
        }
        let kind = render::kind_of(&finding.id);
        if let Some(kinds) = &selected
            && !kinds.contains(kind)
        {
            filtered_out += 1;
            continue;
        }
        let section_row = row_by_table.get(&finding.id).copied();
        let finding = Finding {
            kind,
            id: finding.id.to_string(),
            table: if want_table {
                finding.id.owning_table().cloned()
            } else {
                None
            },
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
        };
        match section_row {
            Some(position) => auto_date_time[position].finding = Some(finding),
            None => findings.push(finding),
        }
    }

    // Broken visual bindings (issue #60), bucketed the same way the unused
    // findings are: `[scan].ignore` counts as handled, type flags hide into
    // their own count, and name-hiding model drift suppresses with its
    // count kept for the summary note.
    let mut broken = Vec::new();
    let mut broken_hidden = 0;
    let mut broken_suppressed = 0;
    for binding in graph.broken_bindings() {
        if is_ignored_display(&binding.target.to_string(), patterns) {
            ignored += 1;
            continue;
        }
        if hides_a_name {
            broken_suppressed += 1;
            continue;
        }
        if !broken_selected {
            broken_hidden += 1;
            continue;
        }
        let (reason, bound_artifact) = match &binding.reason {
            BrokenReason::BoundArtifactBroken { artifact } => {
                ("bound_artifact_broken", Some(artifact.to_string()))
            }
            BrokenReason::TableNotFound => ("table_not_found", None),
            BrokenReason::FieldNotFound => ("field_not_found", None),
            BrokenReason::MeasureNotFound => ("measure_not_found", None),
            BrokenReason::HierarchyNotFound => ("hierarchy_not_found", None),
            BrokenReason::LevelNotFound => ("level_not_found", None),
        };
        broken.push(BrokenOut {
            target: binding.target.to_string(),
            reason,
            bound_artifact,
            provenance: binding.edge.to_string(),
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
        filtered_out,
        machinery_members,
        broken,
        broken_hidden,
        broken_suppressed,
        broken_raw: graph.broken_bindings().len(),
        findings,
        auto_date_time,
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
            render::human(streams.out, &palette_out, &output, args.power_query)
                .map_err(ScanError::from)?;
        }
        // Notices are stderr's job in text modes; --json carries them itself.
        // Not all notices are drift — a stale_state notice reports understood
        // saved state pointing at deleted objects — so the header stays kind-
        // neutral; each line's `[kind]` says which it is.
        if !args.json && !output.skips.is_empty() {
            writeln!(
                streams.err,
                "{} skip notice(s) from parsing:",
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
    } else if output.findings.is_empty()
        // Breakage gates the exit code only under `--broken` (issue #60):
        // a pipeline gating on unused findings must not start failing
        // because one visual is broken, and a `--broken` gate must not fail
        // on unused findings — the same reported-only rule the type flags
        // obey, applied to the new kind.
        && (!broken_gating || output.broken.is_empty())
        && output
            .auto_date_time
            .iter()
            .all(|row| row.verdict == "in_use")
    {
        // Suppressed by [scan].ignore counts as handled; an in-use auto
        // date/time table is informational advice, not a failure. Both the
        // findings and the rows above are already type-flag-filtered, so
        // the exit code describes what was reported.
        Ok(EXIT_CLEAN)
    } else {
        Ok(EXIT_UNUSED)
    }
}

/// A bound-report walk's result, kept for the announcements and the
/// zero-connected-report diagnostics. Always present in model mode; present
/// in PATH mode when a model-naming PATH turns a plain `--report` folder
/// into a search root (issue #67).
struct ModelScan {
    /// How every discovered report item binds to the model.
    bound: discover::BoundReports,
    /// The folders actually walked, for the refusal message.
    search_roots: Vec<PathBuf>,
}

/// Sorts one `--report` value in model mode into a direct report item or a
/// search folder. A `.Report`-named folder without a report anchor is
/// malformed — fail before any walk rather than at ingestion.
fn partition_report_value(
    path: &Path,
    direct: &mut Vec<PathBuf>,
    search_roots: &mut Vec<PathBuf>,
) -> Result<(), ScanError> {
    if is_report_item(path) {
        direct.push(path.to_path_buf());
        return Ok(());
    }
    if path.is_dir() {
        if is_report_suffixed(path) {
            return Err(malformed_report_folder_error(path));
        }
        search_roots.push(path.to_path_buf());
        return Ok(());
    }
    if !path.exists() {
        return Err(ScanError::new(format!("no such path: {}", path.display()))
            .with_hint("pass a report folder or an existing folder to search for bound reports"));
    }
    Err(ScanError::new(format!(
        "--report {} is not a report folder or search folder",
        path.display()
    ))
    .with_hint("point --report at a .Report folder or an existing folder to search"))
}

/// The default search root when `--model` is given with no reports at all:
/// the model's parent folder.
fn default_search_root(item_root: &Path) -> PathBuf {
    match item_root.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        // `--model X.SemanticModel` in the working directory: search here.
        Some(_) if item_root.file_name().is_some() => PathBuf::from("."),
        // `--model .` (or a filesystem root): the model folder *is* the
        // working directory, so its siblings live one level up.
        _ => PathBuf::from(".."),
    }
}

/// The zero-connected-report refusal in model mode, with per-category counts
/// so a mixed search folder explains itself.
fn no_bound_reports_error(model: &Path, scan: &ModelScan) -> ScanError {
    let roots: Vec<String> = scan
        .search_roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    let roots = roots.join(", ");
    let detail = if scan.bound.ignored_elsewhere.is_empty() && scan.bound.unresolved.is_empty() {
        format!("no report items found under {roots}")
    } else {
        format!(
            "{} bound to other models, {} unresolved dataset references under {roots}",
            scan.bound.ignored_elsewhere.len(),
            scan.bound.unresolved.len()
        )
    };
    ScanError::new(format!(
        "nothing to scan against: {} has no reports ({detail})",
        model.display()
    ))
    .with_hint(
        "reachability starts from report bindings — pass one with --report <path>, \
         or scan a .pbip project folder",
    )
}

/// The display name of a report path: its final folder component.
fn report_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The by-name pairing notes collapsed to one line per `initial catalog`,
/// in first-appearance order. Names are listed up to three per catalog; a
/// longer tail becomes `… and N more` so the line stays one line (issue #65).
fn collapse_name_matched(name_matched: &[(PathBuf, String)]) -> Vec<String> {
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
                "Note: {} report(s) matched by dataset name only \
                 (byConnection 'initial catalog' = '{catalog}'): {}",
                names.len(),
                capped_names(&names)
            );
            if names.len() > 3 {
                line.push_str(VERBOSE_POINTER);
            }
            line
        })
        .collect()
}

/// The suffix that points at `--verbose` when a capped list left names out —
/// the one place the full trail lives.
const VERBOSE_POINTER: &str = " — rerun with --verbose to list them";

/// The display form of a name wall: the first three, then the tail as a
/// count — the capping the by-name collapse prints (issue #65), shared with
/// the ignored list so every name list reads the same way.
fn capped_names(names: &[String]) -> String {
    let listed: Vec<String> = names.iter().take(3).cloned().collect();
    let more = names.len() - listed.len();
    if more > 0 {
        format!("{}, … and {more} more", listed.join(", "))
    } else {
        listed.join(", ")
    }
}

/// True when an explicit PATH is classified as a semantic model — the same
/// shapes [`discover::resolve_path`] pairs with `pair_model`, and what
/// `--model` accepts — so a plain `--report` folder can be a search root
/// rather than an error (issue #67). A `resolve_model` probe would be too
/// loose: core's locator accepts any folder with a `definition/` subfolder,
/// `.Report` folders included.
fn names_semantic_model(path: &Path) -> bool {
    path.is_dir()
        && (file_name_lower(path).ends_with(".semanticmodel") || path.join("model.tmdl").is_file())
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

/// The error for a `.Report`-named folder without a report anchor.
fn malformed_report_folder_error(path: &Path) -> ScanError {
    ScanError::new(format!(
        "malformed report folder '{}': missing report.json",
        path.display()
    ))
    .with_hint("a report item needs report.json or definition/report.json directly inside")
}

/// The error for a plain folder passed to `--report` when the scan target is
/// not itself a semantic model — a project, `.pbip`, or `.Report` path, or a
/// cwd-discovered target (issue #67 kept the mode switch for those). The
/// hint names the mode switch and the exact rerun instead of implying the
/// model was not given.
fn plain_report_folder_error(path: &Path, model: &Path) -> ScanError {
    ScanError::new(format!(
        "--report {} is a folder, but not a report item (no report.json or definition/report.json)",
        path.display()
    ))
    .with_hint(format!(
        "plain folders are searched only in --model mode: ripbi scan --model \"{}\" --report \"{}\"",
        model.display(),
        path.display()
    ))
}

/// True when `path` carries the `.Report` item-name convention.
fn is_report_suffixed(path: &Path) -> bool {
    file_name_lower(path).ends_with(".report")
}

/// The lowercase final path component, or an empty string when there is none.
fn file_name_lower(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// True when `path` is a folder directly holding a report anchor — the same
/// property core's report locator accepts (a folder that is a report item,
/// whatever it is named).
fn is_report_item(path: &Path) -> bool {
    path.is_dir()
        && (path.join("report.json").is_file()
            || path.join("definition").join("report.json").is_file())
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

/// True for a finding the machinery verdicts already cover (issue #47): any
/// object owned by an auto date/time table, except the table's own finding,
/// which nests under its row.
fn is_machinery_member(id: &ObjectId, machinery: &HashSet<NameKey>) -> bool {
    !matches!(id, ObjectId::Table { .. })
        && id
            .owning_table()
            .is_some_and(|table| machinery.contains(table))
}

fn is_ignored(id: &ObjectId, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    is_ignored_display(&id.to_string(), patterns)
        || bare_names(id)
            .iter()
            .any(|name| patterns.iter().any(|pattern| glob::matches(pattern, name)))
}

/// The `[scan].ignore` match for a written reference (a broken binding's
/// display form): the pattern matches the display id — e.g. `'Sales'[Color]` —
/// whole. There are no bare names to also try; a binding has no leaf object
/// identity of its own (issue #60).
fn is_ignored_display(display: &str, patterns: &[String]) -> bool {
    !patterns.is_empty()
        && patterns
            .iter()
            .any(|pattern| glob::matches(pattern, display))
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
        SkipKind::StaleState => "stale_state",
    }
}
