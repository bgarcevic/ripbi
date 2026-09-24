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
    self, AutoDateTimeRow, BrokenArtifactOut, BrokenOut, Finding, ScanOutput, SkipNoticeOut,
    UsedByOut,
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

    // Target: the shared ladder — `--model`, else PATH/config `target`, else
    // derivation from the `--report` anchors, else folder discovery.
    let (paired, model_scan, mut announce) = resolve_target(
        TargetInput {
            model: args.model.as_deref(),
            path: args.path.as_deref(),
            config_target: config.as_ref().and_then(|config| config.target.as_deref()),
            extras: &extras,
            no_input: args.no_input,
            quiet: args.quiet,
            cwd,
        },
        streams,
        palette_err,
    )?;
    let report_paths = dedupe(paired.reports);

    if report_paths.is_empty() {
        if args.allow_no_reports {
            // The skip carries the walk's per-category counts, exactly like
            // the refusal: "no reports" is a fact about the folder, and the
            // notice must explain itself the same way the error would have.
            if !args.quiet {
                let detail = model_scan
                    .as_ref()
                    .map(|scan| format!(" ({})", no_bound_reports_detail(scan)))
                    .unwrap_or_default();
                writeln!(
                    streams.err,
                    "Skipped {}: no connected reports{detail}",
                    paired.model.display()
                )
                .map_err(ScanError::from)?;
            }
            return Ok(EXIT_CLEAN);
        }
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
    dedupe_opaque_skips(&mut skips);

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

    // Type selection narrows what is reported (issue #31): findings it hides
    // are counted in `filtered_out`, and the auto date/time section — which
    // is table-shaped — prints and gates the exit code only when tables are
    // among the reported kinds. `--broken` selects the breakage kind the same
    // way, and is the only selection under which breakage gates the exit
    // code (issue #60: advisory until asked).
    let selected = selected_kinds(args)?;
    let section_visible = selected
        .as_ref()
        .is_none_or(|kinds| kinds.contains("table"));
    let broken_selected = selected
        .as_ref()
        .is_none_or(|kinds| kinds.contains("broken_visual"));
    let artifact_selected = selected
        .as_ref()
        .is_none_or(|kinds| kinds.contains("broken_artifact"));
    // Gating is not reporting: with no flags everything is *reported*, but
    // breakage gates the exit code only when `--broken` explicitly selected
    // it (issue #60: advisory until asked).
    let broken_gating = selected
        .as_ref()
        .is_some_and(|kinds| kinds.contains("broken_visual"));
    // Under a lone `--broken` the findings list is empty by construction —
    // the flag scopes the run to breakage — so the human modes' clean
    // placeholder speaks for breakage, not for the filtered-out list.
    let broken_only = selected.as_ref().is_some_and(|kinds| {
        kinds.len() == 2 && kinds.contains("broken_visual") && kinds.contains("broken_artifact")
    });
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
    // findings are: `[scan].ignore` counts as handled, type selection hides into
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
        let (reason, bound_artifact, bound_artifact_report) = match &binding.reason {
            BrokenReason::BoundArtifactBroken {
                artifact,
                report_index,
            } => (
                "bound_artifact_broken",
                Some(artifact.to_string()),
                report_index.map(|index| report_paths[index].display().to_string()),
            ),
            BrokenReason::TableNotFound => ("table_not_found", None, None),
            BrokenReason::FieldNotFound => ("field_not_found", None, None),
            BrokenReason::MeasureNotFound => ("measure_not_found", None, None),
            BrokenReason::HierarchyNotFound => ("hierarchy_not_found", None, None),
            BrokenReason::LevelNotFound => ("level_not_found", None, None),
        };
        broken.push(BrokenOut {
            target: binding.target.to_string(),
            reason,
            bound_artifact,
            bound_artifact_report,
            provenance: binding.edge.to_string(),
        });
    }

    let mut broken_artifacts = Vec::new();
    let mut broken_artifacts_hidden = 0;
    let mut broken_artifacts_suppressed = 0;
    for artifact in graph.broken_artifacts() {
        if is_ignored(&artifact.id, patterns) {
            ignored += 1;
            continue;
        }
        if hides_a_name {
            broken_artifacts_suppressed += 1;
            continue;
        }
        if !artifact_selected {
            broken_artifacts_hidden += 1;
            continue;
        }
        broken_artifacts.push(BrokenArtifactOut {
            id: artifact.id.to_string(),
            kind: render::kind_of(&artifact.id),
            report: artifact
                .report_index
                .map(|index| report_paths[index].display().to_string()),
            unresolved_references: artifact.unresolved_references.clone(),
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
        broken_artifacts,
        broken_artifacts_hidden,
        broken_artifacts_suppressed,
        broken_artifacts_raw: graph.broken_artifacts().len(),
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
        // Notices are stderr's job in text modes; --json carries them itself.
        if !args.json {
            write_skip_notices(streams.err, &output.skips)?;
        }
    }

    if args.strict && !output.skips.is_empty() {
        Ok(EXIT_ERROR)
    } else if output.findings.is_empty()
        // Breakage gates the exit code only under `--broken` (issue #60):
        // a pipeline gating on unused findings must not start failing
        // because DAX or a visual binding is broken, and a `--broken` gate must not fail
        // on unused findings — the same reported-only rule type selection
        // obeys, applied to breakage.
        && (!broken_gating || (output.broken.is_empty() && output.broken_artifacts.is_empty()))
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
pub(crate) struct ModelScan {
    /// How every discovered report item binds to the model.
    pub(crate) bound: discover::BoundReports,
    /// The folders actually walked, for the refusal message.
    pub(crate) search_roots: Vec<PathBuf>,
}

/// What a command names as its scan inputs, for [`resolve_target`]: the
/// struct a new command fills in to inherit the whole pairing ladder.
pub(crate) struct TargetInput<'a> {
    /// `--model`: model mode; disables the PATH ladder and discovery.
    pub model: Option<&'a Path>,
    /// The positional PATH argument, when the command has one.
    pub path: Option<&'a Path>,
    /// The `ripbi.toml` `target`, already resolved against the config dir.
    pub config_target: Option<&'a Path>,
    /// The `--report` values (or the config `reports`).
    pub extras: &'a [PathBuf],
    /// Never prompt; fail where a picker would appear.
    pub no_input: bool,
    /// Suppress the picker along with the output.
    pub quiet: bool,
    /// The working directory discovery searches.
    pub cwd: &'a Path,
}

/// The single entry point for any command that pairs a semantic model with
/// reports — `scan`, `report`, `deps`, and future ones. Resolves every input
/// shape into one [`discover::Paired`] plus, when a bound-report walk ran,
/// its [`ModelScan`]. The ladder, first match wins:
///
/// 1. `model` (`--model`): model mode — every `--report` anchor attaches,
///    plain folders become search roots, and with no anchors the model's
///    parent folder is walked.
/// 2. `path` or the config `target`: the paired project, plus `--report`
///    anchors; plain folders are search roots only when the path names a
///    semantic model.
/// 3. `--report` anchors alone: the model is derived from the anchors by the
///    same pairing tiers a report PATH gets, and the reports are exactly the
///    anchors — no walk.
/// 4. Folder discovery in `cwd`, with a picker on a TTY.
///
/// Post-pairing policy stays with each command: the zero-report refusal,
/// rendering, and exit codes.
pub(crate) fn resolve_target(
    input: TargetInput<'_>,
    streams: &mut Streams<'_>,
    palette_err: &Palette,
) -> Result<(discover::Paired, Option<ModelScan>, Vec<String>), ScanError> {
    if let Some(model_path) = input.model {
        let (paired, walk) = resolve_model_mode(model_path, input.extras)?;
        return Ok((paired, Some(walk), Vec::new()));
    }
    if let Some(explicit) = input.path.or(input.config_target) {
        let model_named = names_semantic_model(explicit);
        let paired = resolve_explicit(explicit)?;
        let (paired, walk) = attach_extras(paired, input.extras, model_named)?;
        return Ok((paired, walk, Vec::new()));
    }
    if !input.extras.is_empty() {
        let paired = derive_model_from_reports(input.extras)?;
        return Ok((paired, None, Vec::new()));
    }
    let (paired, announce) =
        discover_target(input.no_input, input.quiet, input.cwd, streams, palette_err)?;
    let (paired, walk) = attach_extras(paired, input.extras, false)?;
    Ok((paired, walk, announce))
}

/// Derives the model from `--report` anchors alone — no PATH, no `--model`,
/// no config `target`. Each anchor pairs by the same tiers a report PATH
/// gets (stem sibling, sole sibling, `byPath`, `byConnection` dataset name),
/// every anchor must land on the same model, and the reports are exactly the
/// anchors: naming the reports is the whole point, so nothing else is pulled
/// in by a walk.
fn derive_model_from_reports(extras: &[PathBuf]) -> Result<discover::Paired, ScanError> {
    let mut model: Option<PathBuf> = None;
    let mut reports = Vec::new();
    for extra in extras {
        let paired = match discover::resolve_path(extra)? {
            Resolution::Paired(paired) => paired,
            Resolution::Ambiguous { dir, candidates } => {
                return Err(ambiguous_error(&dir, &candidates));
            }
            Resolution::Archive(archive) => return Err(discover::archive_error(&archive)),
            Resolution::Unrecognized(unrecognized) => {
                return Err(ScanError::new(format!(
                    "--report {} does not name a report or a project",
                    unrecognized.display()
                ))
                .with_hint(
                    "point --report at a .Report folder or a .pbip, or pass --model \
                     with the folder to search",
                ));
            }
        };
        unify_derived_model(extra, &mut model, &paired.model)?;
        reports.extend(paired.reports);
    }
    Ok(discover::Paired {
        model: model.expect("at least one anchor resolved to a model"),
        reports,
    })
}

/// Records an anchor's model, refusing anchors that pair with different
/// models — one scan runs against one semantic model.
fn unify_derived_model(
    anchor: &Path,
    model: &mut Option<PathBuf>,
    derived: &Path,
) -> Result<(), ScanError> {
    if let Some(model) = model {
        if discover::canonical_key(model) != discover::canonical_key(derived) {
            return Err(ScanError::new(format!(
                "--report {} pairs with {}, but the other reports pair with {}",
                anchor.display(),
                derived.display(),
                model.display()
            ))
            .with_hint("one scan runs against one semantic model — pass --model to choose it"));
        }
        return Ok(());
    }
    *model = Some(derived.to_path_buf());
    Ok(())
}

/// Resolves `--model MODE` with its `--report` values: the model, plus every
/// report item the extras name and every one a search-folder walk finds.
fn resolve_model_mode(
    model_path: &Path,
    extras: &[PathBuf],
) -> Result<(discover::Paired, ModelScan), ScanError> {
    let target = discover::resolve_model(model_path)?;
    let mut direct = Vec::new();
    let mut search_roots = Vec::new();
    for extra in extras {
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
    if let Some(error) = &bound.walk_error {
        return Err(error.clone());
    }
    let mut reports = bound.reports.clone();
    reports.extend(direct.iter().cloned());
    Ok((
        discover::Paired {
            model: target.item_root,
            reports,
        },
        ModelScan {
            bound,
            search_roots,
        },
    ))
}

/// Attaches `--report` values to an already-resolved target (PATH argument,
/// config `target`, or discovery): direct report items pair as-is, a `.pbip`
/// expands to its project's reports, and a plain folder becomes a search
/// root only when the target itself is a semantic model (issue #67).
fn attach_extras(
    paired: discover::Paired,
    extras: &[PathBuf],
    model_named: bool,
) -> Result<(discover::Paired, Option<ModelScan>), ScanError> {
    let mut report_paths = paired.reports.clone();
    let mut direct = Vec::new();
    let mut search_roots = Vec::new();
    for extra in extras {
        if is_report_item(extra) {
            direct.push(extra.clone());
            continue;
        }
        if let Some(paired_reports) = pbip_reports(extra)? {
            direct.extend(paired_reports);
            continue;
        }
        if extra.is_dir() {
            if is_report_suffixed(extra) {
                return Err(malformed_report_folder_error(extra));
            }
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
            ScanError::new(format!("--report {} is not a folder", extra.display()))
                .with_hint("point --report at a .Report folder (or a folder holding report.json)"),
        );
    }
    if search_roots.is_empty() {
        report_paths.extend(direct);
        return Ok((
            discover::Paired {
                model: paired.model,
                reports: report_paths,
            },
            None,
        ));
    }
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
    if let Some(error) = &bound.walk_error {
        return Err(error.clone());
    }
    report_paths.extend(direct);
    report_paths.extend(bound.reports.iter().cloned());
    Ok((
        discover::Paired {
            model: paired.model,
            reports: report_paths,
        },
        Some(ModelScan {
            bound,
            search_roots,
        }),
    ))
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

/// Sorts one `--report` value in model mode into a direct report item or a
/// search folder; a `.pbip` expands to its project's reports as direct items.
/// A `.Report`-named folder without a report anchor is malformed — fail
/// before any walk rather than at ingestion.
pub(crate) fn partition_report_value(
    path: &Path,
    direct: &mut Vec<PathBuf>,
    search_roots: &mut Vec<PathBuf>,
) -> Result<(), ScanError> {
    if is_report_item(path) {
        direct.push(path.to_path_buf());
        return Ok(());
    }
    if let Some(reports) = pbip_reports(path)? {
        direct.extend(reports);
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

/// The reports of a `.pbip` passed to `--report`, or `None` when `path` is
/// not a `.pbip` file — the project's reports become direct anchors, its
/// model is discarded (the model names itself, via `--model` or the anchors).
fn pbip_reports(path: &Path) -> Result<Option<Vec<PathBuf>>, ScanError> {
    if !path.is_file() || !file_name_lower(path).ends_with(".pbip") {
        return Ok(None);
    }
    match discover::resolve_path(path)? {
        Resolution::Paired(paired) => Ok(Some(paired.reports)),
        Resolution::Ambiguous { dir, candidates } => Err(ambiguous_error(&dir, &candidates)),
        Resolution::Archive(archive) => Err(discover::archive_error(&archive)),
        Resolution::Unrecognized(unrecognized) => Err(ScanError::new(format!(
            "not a Power BI project: {}",
            unrecognized.display()
        ))
        .with_hint(
            "point --report at a .pbip file, a .Report folder, or an existing folder to search",
        )),
    }
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
    ScanError::new(format!(
        "nothing to scan against: {} has no reports ({})",
        model.display(),
        no_bound_reports_detail(scan)
    ))
    .with_hint(
        "reachability starts from report bindings — pass one with --report <path>, \
         or scan a .pbip project folder",
    )
}

/// The per-category detail behind a zero-connected-report model, shared by
/// the refusal and the `--allow-no-reports` skip notice: where the walk
/// looked and what it found instead.
fn no_bound_reports_detail(scan: &ModelScan) -> String {
    let roots: Vec<String> = scan
        .search_roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    let roots = roots.join(", ");
    if scan.bound.ignored_elsewhere.is_empty() && scan.bound.unresolved.is_empty() {
        format!("no report items found under {roots}")
    } else {
        format!(
            "{} bound to other models, {} unresolved dataset references under {roots}",
            scan.bound.ignored_elsewhere.len(),
            scan.bound.unresolved.len()
        )
    }
}

/// The display name of a report path: its final folder component.
pub(crate) fn report_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The by-name pairing notes collapsed to one line per `initial catalog`,
/// in first-appearance order. Names are listed up to three per catalog; a
/// longer tail becomes `… and N more` so the line stays one line (issue #65).
pub(crate) fn collapse_name_matched(name_matched: &[(PathBuf, String)]) -> Vec<String> {
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
pub(crate) const VERBOSE_POINTER: &str = " — rerun with --verbose to list them";

/// The display form of a name wall: the first three, then the tail as a
/// count — the capping the by-name collapse prints (issue #65), shared with
/// the ignored list so every name list reads the same way.
pub(crate) fn capped_names(names: &[String]) -> String {
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
    no_input: bool,
    quiet: bool,
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
            if no_input || quiet || !streams.stdin_is_tty {
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

/// Drops paths already seen (compared canonically), keeping first occurrence.
pub(crate) fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
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

/// The selected finding kinds: `--broken` unioned with every `--type` value,
/// in the same machine vocabulary `deps --type` speaks. An
/// unknown kind is a usage error, never silence — and `broken_visual` is
/// deliberately outside the vocabulary, because selecting it is what makes
/// breakage gate the exit code, and that is `--broken`'s job alone (issue
/// #60: advisory until asked).
fn selected_kinds(args: &ScanArgs) -> Result<Option<HashSet<&'static str>>, ScanError> {
    let mut picks = args.selected_kinds().unwrap_or_default();
    if picks.is_empty() && args.types.is_empty() {
        return Ok(None);
    }
    for kind in &args.types {
        let Some(static_kind) = render::KINDS.iter().find(|known| known == &kind) else {
            return Err(
                ScanError::new(format!("--type {kind} is not an object type")).with_hint(format!(
                    "one of: {}; broken findings are selected by --broken",
                    render::KINDS.join(", ")
                )),
            );
        };
        picks.insert(static_kind);
    }
    Ok(Some(picks))
}

/// Maps a core skip notice to its output DTO — the shared shape of every
/// command's `skips` array and stderr block.
pub(crate) fn skip_notice_out(notice: &SkipNotice) -> SkipNoticeOut {
    SkipNoticeOut {
        path: notice.path.display().to_string(),
        location: notice.location.clone(),
        kind: skip_kind_out(notice.kind),
        detail: notice.detail.clone(),
    }
}

/// Keeps the first instance of an opaque-source notice while retaining every
/// distinct partition and every notice of another kind.
fn dedupe_opaque_skips(skips: &mut Vec<SkipNoticeOut>) {
    let mut seen = HashSet::new();
    skips.retain(|skip| {
        skip.kind != "opaque_source"
            || seen.insert((
                skip.path.clone(),
                skip.location.clone(),
                skip.detail.clone(),
            ))
    });
}

pub(crate) fn skip_kind_out(kind: SkipKind) -> &'static str {
    match kind {
        SkipKind::UnknownObject => "unknown_object",
        SkipKind::UnknownProperty => "unknown_property",
        SkipKind::MalformedValue => "malformed_value",
        SkipKind::UnresolvedAlias => "unresolved_alias",
        SkipKind::StaleState => "stale_state",
        SkipKind::OpaqueSource => "opaque_source",
    }
}

#[cfg(test)]
mod tests {
    use super::{SkipNoticeOut, dedupe_opaque_skips};

    #[test]
    fn opaque_skip_deduplication_preserves_distinct_partitions_and_other_kinds() {
        let opaque = SkipNoticeOut {
            path: "Sales.tmdl".to_string(),
            location: Some("line 1".to_string()),
            kind: "opaque_source",
            detail: "partition 'Sales'".to_string(),
        };
        let mut skips = vec![
            opaque.clone(),
            opaque.clone(),
            SkipNoticeOut {
                location: Some("line 2".to_string()),
                detail: "partition 'Other'".to_string(),
                ..opaque.clone()
            },
            SkipNoticeOut {
                kind: "malformed_value",
                ..opaque.clone()
            },
            SkipNoticeOut {
                kind: "malformed_value",
                ..opaque
            },
        ];

        dedupe_opaque_skips(&mut skips);

        assert_eq!(skips.len(), 4);
        assert_eq!(skips[0].detail, "partition 'Sales'");
        assert_eq!(skips[1].detail, "partition 'Other'");
    }
}
