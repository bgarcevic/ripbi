//! One scan, front-end neutral: resolve a target (PATH, `--model`, config, or
//! discovery), ingest through `ripbi-core`, build the dependency graph, and
//! bucket what it finds into a [`ScanOutput`]. Nothing here prints: progress,
//! notes, and the project picker go through an [`Observer`] the caller
//! supplies, so the CLI and a desktop UI each present them their own way.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ripbi_core::graph::{BrokenReason, DependencyGraph};
use ripbi_core::ingest::{self, SkipKind, SkipNotice};
use ripbi_core::usage::{self, QueryLog};
use ripbi_core::{
    NameKey, ObjectId, PartitionSource, ReportModel, SizeBasis, StorageStats, TabularDatabase,
};

use crate::compare::{self, Baseline, Detected, Entry, Status};
use crate::config::{self, Config};
use crate::discover::{self, Candidate, Resolution};
use crate::error::ScanError;
use crate::glob;
use crate::locate::{Existing, Locator};
use crate::manifest::{MANIFEST_PATH, Manifest, ReportStatus};
use crate::output::{
    self, AutoDateTimeRow, BrokenArtifactOut, BrokenOut, CompareOut, Finding, ScanOutput,
    SkipNoticeOut, UnusedStorage, UsedByOut,
};

/// The pipeline stages a scan passes through after target resolution, in
/// order — what a progress display names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The target resolved; the model is being ingested.
    Model = 1,
    /// The model is in; the reports are being ingested.
    Reports = 2,
    /// Everything is ingested; the dependency graph is being built.
    Graph = 3,
    /// The graph is built; findings are being bucketed.
    Findings = 4,
}

/// How much a [`Note`] matters to reading the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteLevel {
    /// Background context.
    Info,
    /// Changes how to read the result.
    Warn,
}

/// One message about the run — never a finding, never an error. `text`
/// carries no trailing period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub level: NoteLevel,
    pub text: String,
}

impl Note {
    #[must_use]
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            level: NoteLevel::Info,
            text: text.into(),
        }
    }

    #[must_use]
    pub fn warn(text: impl Into<String>) -> Self {
        Self {
            level: NoteLevel::Warn,
            text: text.into(),
        }
    }
}

/// How a target was found when nothing named it: the only project in the
/// working directory, or the one the picker returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovered {
    Only(String),
    Selected(String),
}

/// The scan target once pairing is done, before anything is ingested.
pub struct Target<'a> {
    /// The semantic model.
    pub model: &'a Path,
    /// The reports scanned against, deduplicated.
    pub reports: &'a [PathBuf],
    /// The bound-report walk, when one ran.
    pub bound: Option<&'a discover::BoundReports>,
    /// How cwd discovery found the target, when it did.
    pub discovered: Option<&'a Discovered>,
}

/// What a front end sees of a running scan. Every method has a silent
/// default, so `&mut ()` runs a scan with no observer at all. Calls arrive in
/// pipeline order; a returned error aborts the scan with it.
pub trait Observer {
    /// The scan entered `stage`.
    fn stage(&mut self, _stage: Stage) {}

    /// Pairing is done and the scan will run against `target`.
    ///
    /// # Errors
    /// Whatever the front end fails with; the scan stops.
    fn resolved(&mut self, _target: &Target<'_>) -> Result<(), ScanError> {
        Ok(())
    }

    /// No model paired and the command lists `reports` on their own
    /// (`report` under `allow_no_model`); a warning note follows.
    ///
    /// # Errors
    /// Whatever the front end fails with; the run stops.
    fn unpaired(&mut self, _reports: &[PathBuf]) -> Result<(), ScanError> {
        Ok(())
    }

    /// A note about the run, in the order the scan makes it.
    ///
    /// # Errors
    /// Whatever the front end fails with; the scan stops.
    fn note(&mut self, _note: Note) -> Result<(), ScanError> {
        Ok(())
    }

    /// Several projects in `dir` and nothing named one: the index of the one
    /// to scan, or `None` to refuse with the candidate list.
    ///
    /// # Errors
    /// Whatever the front end fails with; the scan stops.
    fn pick(&mut self, _dir: &Path, _candidates: &[Candidate]) -> Result<Option<usize>, ScanError> {
        Ok(None)
    }
}

impl Observer for () {}

/// Everything one scan is asked to do. `Default` is a plain discovery scan
/// of the current directory.
#[derive(Debug, Clone, Default)]
pub struct ScanRequest {
    /// The working directory: discovery, `ripbi.toml`, and relative paths
    /// resolve against it.
    pub cwd: PathBuf,
    /// The model to scan (`--model`): model mode, with report discovery.
    pub model: Option<PathBuf>,
    /// A project, model, or report to scan (the CLI's PATH argument).
    pub path: Option<PathBuf>,
    /// Reports, projects, or search folders; replaces the config's `reports`.
    pub reports: Vec<PathBuf>,
    /// Never ask the observer to pick a project.
    pub no_input: bool,
    /// The caller shows nothing: also never asks to pick a project.
    pub quiet: bool,
    /// Notes name sources by their full path.
    pub verbose: bool,
    /// A model with no connected reports ends in [`Scan::Skipped`] instead of
    /// an error.
    pub allow_no_reports: bool,
    /// Report broken bindings and DAX artifacts (the CLI's `--broken`): with
    /// `types`, narrows the report; with neither, everything is reported.
    pub broken: bool,
    /// Object types to report, validated against [`output::KINDS`].
    pub types: Vec<String>,
    /// The storage source: an `.abf`/`.pbix`/`.vpax`, or `none`; overrides
    /// the config's `stats_from`.
    pub stats_from: Option<PathBuf>,
    /// A workspace-monitoring query log; overrides the config's `queries_from`.
    pub queries_from: Option<PathBuf>,
    /// Which model's rows of the query log to read.
    pub queries_item: Option<String>,
    /// Order findings by storage size, largest first.
    pub sort_by_size: bool,
    /// Fill each finding's owning `table`.
    pub with_tables: bool,
    /// Record where each finding lives ([`ScanOutcome::locator`]).
    pub locate: bool,
    /// Under `compare`, keep findings that already existed in
    /// [`ScanOutcome::existing`] instead of dropping them.
    pub keep_existing: bool,
    /// Compare against the same scan in another checkout.
    pub compare: Option<CompareRequest>,
}

/// The `--compare-root` comparison (issue #141).
#[derive(Debug, Clone)]
pub struct CompareRequest {
    /// The other checkout, as the user wrote it: what the output shows.
    pub root: PathBuf,
    /// What the output calls it; defaults to `root`.
    pub label: Option<String>,
    /// The scan to run there: the same request with its paths rebased onto
    /// the other checkout.
    pub before: Box<ScanRequest>,
}

/// Why a scan ran no analysis, under [`ScanRequest::allow_no_reports`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// No report is connected; `detail` explains the walk when one ran.
    NoReports { detail: Option<String> },
    /// The only reports are ripbi stubs, which bind nothing.
    OnlyStubs,
}

/// What a scan produced.
#[derive(Debug)]
pub enum Scan {
    /// No analysis: the model has nothing to scan against.
    Skipped { model: PathBuf, reason: SkipReason },
    /// The analysis.
    Done(Box<ScanOutcome>),
}

/// One finished scan.
#[derive(Debug)]
pub struct ScanOutcome {
    /// The findings, counts, and notices. `elapsed` is left for the caller.
    pub output: ScanOutput,
    /// Findings that already existed in the compared checkout, kept under
    /// [`ScanRequest::keep_existing`].
    pub existing: Existing,
    /// Where every finding lives, under [`ScanRequest::locate`].
    pub locator: Option<Locator>,
    /// The reported findings plus dead auto date/time tables, in that order:
    /// everything a removal tool may delete.
    pub removable: Vec<ObjectId>,
    /// The ingested model.
    pub model: TabularDatabase,
    /// The `ripbi.toml` the scan read, if any.
    pub config: Option<Config>,
    /// The kinds selected for reporting; `None` reports everything.
    pub selected: Option<HashSet<&'static str>>,
    /// Whether an Excel workbook was among the reports read (issue #172):
    /// the consumer hint then names only the workbooks that were not.
    pub workbooks: bool,
}

/// What a finished scan means for a gate: the CLI's exit code, a GUI's
/// pass/fail badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing reported needs action.
    Clean,
    /// Reported findings need action.
    Findings,
    /// Under `strict`, ingestion or the report walk skipped something, so the
    /// result cannot be trusted as complete.
    Incomplete,
}

impl ScanOutcome {
    /// The gate's verdict. `strict` fails on any skip notice. Breakage gates
    /// only when the selection asked for it (issue #60): a pipeline gating on
    /// unused findings must not start failing because DAX or a visual binding
    /// is broken, and a `--broken` gate must not fail on unused findings — the
    /// same reported-only rule type selection obeys, applied to breakage.
    /// Suppressed by `[scan].ignore` counts as handled, and an in-use auto
    /// date/time table is advice, not a failure. Everything here is already
    /// type-filtered, so the verdict describes what was reported.
    #[must_use]
    pub fn verdict(&self, strict: bool) -> Verdict {
        let output = &self.output;
        let broken_gating = self
            .selected
            .as_ref()
            .is_some_and(|kinds| kinds.contains("broken_visual"));
        if strict && !output.skips.is_empty() {
            Verdict::Incomplete
        } else if output.findings.is_empty()
            && (!broken_gating || (output.broken.is_empty() && output.broken_artifacts.is_empty()))
            && output
                .auto_date_time
                .iter()
                .all(|row| row.verdict == "in_use")
        {
            Verdict::Clean
        } else {
            Verdict::Findings
        }
    }

    /// Whether the selection is breakage alone (a lone `--broken`). The
    /// findings list is then empty by construction — the selection scopes the
    /// run to breakage — so a clean placeholder should speak for breakage, not
    /// for the filtered-out list.
    #[must_use]
    pub fn broken_only(&self) -> bool {
        self.selected.as_ref().is_some_and(|kinds| {
            kinds.len() == 2 && kinds.contains("broken_visual") && kinds.contains("broken_artifact")
        })
    }
}

/// Runs one scan.
///
/// # Errors
/// Discovery, pairing, ingestion, an invalid selection, an unreadable query
/// log or explicit stats source, and whatever the observer fails with.
pub fn run(request: &ScanRequest, observer: &mut dyn Observer) -> Result<Scan, ScanError> {
    let config = config::find_in(&request.cwd)?.map(|loaded| loaded.config);
    let before = match &request.compare {
        Some(compare) => Some(baseline(compare, observer)?),
        None => None,
    };
    Ok(analyze(request, config, before.as_ref(), observer, None)?
        .expect("a scan without a probe always produces a result"))
}

/// The "before" pass of `--compare-root` (issue #141): the same scan, run
/// silently in the other checkout — its own `ripbi.toml`, discovery, and
/// pairing — collecting every finding it detects. A checkout where the scan
/// cannot run (a model this change adds, say) has nothing to compare against:
/// that is a note, and every finding is new. A checkout where the model had
/// no connected reports is a skipped baseline: also a note, and the usage
/// findings on objects it had are unverified.
fn baseline(compare: &CompareRequest, observer: &mut dyn Observer) -> Result<Baseline, ScanError> {
    let dir = &compare.before.cwd;
    if !dir.is_dir() {
        return Err(
            ScanError::new(format!("--compare-root {} is not a folder", dir.display())).with_hint(
                "check out the base revision first, e.g. `git worktree add ../base origin/main`",
            ),
        );
    }
    let mut baseline = Baseline::scanned(Detected::new());
    let result = config::find_in(dir).and_then(|config| {
        analyze(
            &compare.before,
            config.map(|loaded| loaded.config),
            None,
            &mut (),
            Some(&mut baseline),
        )
    });
    if let Err(error) = result {
        observer.note(Note::warn(format!(
            "nothing to compare in {}: {}; every finding counts as new",
            dir.display(),
            error.message
        )))?;
        return Ok(Baseline::unavailable(error.message));
    }
    if let Some(detail) = &baseline.detail {
        observer.note(Note::warn(format!(
            "baseline skipped in {}: {detail}; unused findings on objects that existed \
             there are unverified and do not gate",
            dir.display()
        )))?;
    }
    Ok(baseline)
}

/// One scan. With `probe`, this is the silent "before" pass of
/// `--compare-root` (issue #141): every finding the scan detects is written
/// to `probe`, and `None` comes back. A model with no connected reports is
/// not skipped there: its analysis runs with no report roots and the baseline
/// is marked skipped, so the comparison still knows which objects existed
/// without vouching for their usage.
fn analyze(
    request: &ScanRequest,
    config: Option<Config>,
    before: Option<&Baseline>,
    observer: &mut dyn Observer,
    probe: Option<&mut Baseline>,
) -> Result<Option<Scan>, ScanError> {
    let cwd = request.cwd.as_path();

    // Report roots: `reports` replace the config's `reports`; a plain
    // folder among them becomes a search root in model mode, or when an
    // explicit PATH names a semantic model (issue #67).
    let extras: Vec<PathBuf> = if request.reports.is_empty() {
        config
            .as_ref()
            .map(|config| config.reports.clone())
            .unwrap_or_default()
    } else {
        request.reports.clone()
    };

    // Target: the shared ladder — `--model`, else PATH/config `target`, else
    // derivation from the `--report` anchors, else folder discovery.
    let resolved = resolve_target(
        TargetInput {
            model: request.model.as_deref(),
            path: request.path.as_deref(),
            config_target: config.as_ref().and_then(|config| config.target.as_deref()),
            app_mappings: config.as_ref().map_or(&[], |config| config.apps.as_slice()),
            extras: &extras,
            no_input: request.no_input,
            quiet: request.quiet,
            cwd,
        },
        observer,
    )?;
    observer.stage(Stage::Model);
    let paired = resolved.paired;
    let model_scan = resolved.walk;
    let report_paths = dedupe(paired.reports);
    let workbooks = has_workbooks(&report_paths);

    // The "before" pass when the model had no connected reports: why its
    // usage is unknown. Outside a probe this is the refusal or the skip.
    let mut uncovered: Option<String> = None;
    if report_paths.is_empty() && probe.is_some() {
        uncovered = Some(format!(
            "{} had no connected reports there",
            paired.model.display()
        ));
    } else if report_paths.is_empty() {
        if request.allow_no_reports {
            // The skip carries the walk's per-category counts, exactly like
            // the refusal: "no reports" is a fact about the folder, and the
            // notice must explain itself the same way the error would have.
            return Ok(Some(Scan::Skipped {
                model: paired.model,
                reason: SkipReason::NoReports {
                    detail: model_scan.as_ref().map(no_bound_reports_detail),
                },
            }));
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

    observer.resolved(&Target {
        model: &paired.model,
        reports: &report_paths,
        bound: model_scan.as_ref().map(|scan| &scan.bound),
        discovered: resolved.discovered.as_ref(),
    })?;

    // Ingest: the model, then every report as a root.
    let mut model = ingest::semantic_model(&paired.model).map_err(|error| {
        ScanError::new(format!("cannot ingest {}: {error}", paired.model.display()))
    })?;
    let setting = match &request.stats_from {
        // Like `--model`, a relative flag path is taken as written.
        Some(written) => Some(config::StatsSetting::parse(Path::new(""), written)),
        None => config.as_ref().and_then(|config| config.stats_from.clone()),
    };
    let stats_source = attach_storage(
        &mut model.value,
        &paired.model,
        setting,
        request.stats_from.is_some(),
        request.verbose,
        observer,
    )?;
    observer.stage(Stage::Reports);
    let mut skips: Vec<SkipNoticeOut> = model.skips.iter().map(skip_notice_out).collect();
    let mut reports = Vec::new();
    for path in &report_paths {
        let ingested = ingest::report(path).map_err(|error| {
            ScanError::new(format!("cannot ingest report {}: {error}", path.display()))
        })?;
        skips.extend(ingested.skips.iter().map(skip_notice_out));
        if ingested.value.stub {
            observer.note(Note::info(format!(
                "{} is a ripbi stub (no bindings)",
                report_name(path)
            )))?;
        }
        reports.push(ingested.value);
    }
    // A `stub-report` stub binds nothing (issue #130): alone, it would turn
    // every object into a finding. It opens the project in Desktop; it is not
    // a consumer, so a model with only stubs is a model with no reports.
    if uncovered.is_none() && reports.iter().all(|report| report.stub) && probe.is_some() {
        uncovered = Some(format!(
            "{} had only ripbi stub reports there",
            paired.model.display()
        ));
    } else if uncovered.is_none() && reports.iter().all(|report| report.stub) {
        if request.allow_no_reports {
            return Ok(Some(Scan::Skipped {
                model: paired.model,
                reason: SkipReason::OnlyStubs,
            }));
        }
        return Err(ScanError::new(format!(
            "nothing to scan against: {} has only ripbi stub reports, which bind nothing",
            paired.model.display()
        ))
        .with_hint("pass the reports that consume the model with --report <path>"));
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
                    kind: discover::unresolved_kind(path),
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
    let query_log = load_query_log(
        query_log_path(request.queries_from.as_deref(), config.as_ref()),
        query_log_item(request.queries_item.as_deref(), config.as_ref()),
        &paired.model,
        observer,
        &mut skips,
    )?;
    skips.extend(fetch_completeness(&cwd.join(&paired.model)));
    dedupe_opaque_skips(&mut skips);

    // Analysis: entirely core's job.
    observer.stage(Stage::Graph);
    let report_refs: Vec<&ReportModel> = reports.iter().collect();
    let graph = DependencyGraph::build_with_queries(&model.value, &report_refs, &query_log);
    let unused = graph.unused_objects();
    let verdicts = graph.auto_date_time_tables(&model.value);
    let objects = graph.object_ids().count();
    let roots = graph.roots().len();
    // Storage statistics (issue #122): display-only, from a `.abf`/PBIX model's
    // own catalog or attached from a storage source (issue #129).
    let storage = model.value.storage_by_object();
    let reachable = objects - unused.len();

    // The auto date/time machinery's members are never standalone findings
    // (issue #47): GUID-named columns reading as orphans is exactly the noise
    // the section exists to prevent. The section's one row per machinery
    // table — verdict, the date column it serves, and the dead table's own
    // chain — is the deliberate surface; a table only ever goes away with its
    // members.
    observer.stage(Stage::Findings);
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
    let selected = selected_kinds(request.broken, &request.types)?;
    let section_visible = selected
        .as_ref()
        .is_none_or(|kinds| kinds.contains("table"));
    let broken_selected = selected
        .as_ref()
        .is_none_or(|kinds| kinds.contains("broken_visual"));
    let artifact_selected = selected
        .as_ref()
        .is_none_or(|kinds| kinds.contains("broken_artifact"));
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
    //
    // `--compare-root` bookkeeping (issue #141) runs alongside every bucket
    // below: `detected` fingerprints everything the scan found, before any
    // filter, so a finding of the other checkout is "fixed" only when it is
    // truly gone here; and a reportable finding that already existed there is
    // counted in `existing` and dropped, exactly like an ignored one.
    let mut detected = Detected::new();
    let mut existing = 0;
    // `--sarif` (and `--sarif-file`) keeps what already existed as
    // suppressed results (issue #142); it and `--azure-devops` need every
    // finding's source file. Both only when rendering.
    let sarif_mode = request.keep_existing && probe.is_none();
    let mut kept_existing = Existing::default();
    let mut locator = if request.locate && probe.is_none() {
        let declared = ingest::source_locations(&paired.model).map_err(|error| {
            ScanError::new(format!(
                "cannot read source positions of {}: {error}",
                paired.model.display()
            ))
        })?;
        Some(Locator::new(&paired.model, declared, &report_paths))
    } else {
        None
    };
    let status = |entry: &Entry| before.map_or(Status::New, |before| before.classify(entry));
    let existed = |entry: &Entry| status(entry) == Status::Existing;
    // Usage findings on objects a skipped baseline had but could not judge:
    // listed apart from the findings, never gating.
    let mut unverified: Vec<Entry> = Vec::new();
    let mut auto_date_time: Vec<AutoDateTimeRow> = Vec::new();
    let mut row_by_table: HashMap<&ObjectId, usize> = HashMap::new();
    // Tables whose row already existed: their own dead-table finding goes
    // with the row, never back into the generic list.
    let mut existing_tables: HashSet<&ObjectId> = HashSet::new();
    for verdict in &verdicts {
        let label = output::verdict_of(verdict.verdict);
        let id = verdict.id.to_string();
        // An in-use table is advice, not a finding: nothing to accept.
        let entry = (label != "in_use").then(|| Entry::auto_date_time(&id, label));
        if let Some(entry) = &entry {
            compare::insert(&mut detected, entry);
        }
        if !section_visible || is_ignored(&verdict.id, patterns) {
            continue;
        }
        if let (Some(locator), Some(entry)) = (&mut locator, &entry) {
            locator.object(entry, &verdict.id, None);
        }
        let row = AutoDateTimeRow {
            verdict: label,
            id,
            source_column: verdict.source_column.as_ref().map(ToString::to_string),
            finding: None,
        };
        match entry.as_ref().map(status) {
            Some(Status::Existing) => {
                existing += 1;
                existing_tables.insert(&verdict.id);
                if sarif_mode {
                    kept_existing.auto_date_time.push(row);
                }
                continue;
            }
            Some(Status::Unverified) => {
                unverified.extend(entry);
                existing_tables.insert(&verdict.id);
                continue;
            }
            _ => {}
        }
        row_by_table.insert(&verdict.id, auto_date_time.len());
        auto_date_time.push(row);
    }

    // The model table travels with a finding only where the caller reads it;
    // cloning it otherwise would allocate for nothing.
    let want_table = request.with_tables;
    let mut filtered_out = 0;
    let mut machinery_members = 0;
    // The identities behind `findings`, for the storage total's dedupe.
    let mut reported: Vec<&ObjectId> = Vec::new();
    // Dead auto date/time tables, filed under their verdict rows: removable
    // like any finding, so `--emit tx-rm` lists them too.
    let mut dead_machinery: Vec<&ObjectId> = Vec::new();
    for finding in &unused {
        if is_machinery_member(&finding.id, &machinery) {
            machinery_members += 1;
            continue;
        }
        if existing_tables.contains(&finding.id) {
            continue;
        }
        let kind = output::kind_of(&finding.id);
        let display = finding.id.to_string();
        let section_row = row_by_table.get(&finding.id).copied();
        // A dead auto date/time table's own finding is fingerprinted by its row.
        let entry = section_row.is_none().then(|| Entry::unused(kind, &display));
        if let Some(entry) = &entry {
            compare::insert(&mut detected, entry);
        }
        if is_ignored(&finding.id, patterns) {
            ignored += 1;
            continue;
        }
        if let Some(kinds) = &selected
            && !kinds.contains(kind)
        {
            filtered_out += 1;
            continue;
        }
        let status = entry.as_ref().map_or(Status::New, status);
        if status == Status::Unverified {
            unverified.extend(entry);
            continue;
        }
        let is_existing = status == Status::Existing;
        if is_existing {
            existing += 1;
            if !sarif_mode {
                continue;
            }
        }
        if let (Some(locator), Some(entry)) = (&mut locator, &entry) {
            locator.object(entry, &finding.id, None);
        }
        let id = &finding.id;
        let finding = Finding {
            kind,
            id: display,
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
            named_in_power_query: output::power_query_labels(&finding.named_by_m),
            storage: storage.get(&finding.id).copied(),
        };
        match section_row {
            _ if is_existing => kept_existing.findings.push(finding),
            Some(position) => {
                dead_machinery.push(id);
                auto_date_time[position].finding = Some(finding);
            }
            None => {
                reported.push(id);
                findings.push(finding);
            }
        }
    }
    let unused_storage = unused_storage(&reported, &storage);
    if request.sort_by_size {
        // Stable: equal sizes, and every finding without one (last), keep
        // identity order.
        findings.sort_by(|a, b| {
            let bytes = |finding: &Finding| finding.storage.and_then(|stats| stats.bytes);
            bytes(b).cmp(&bytes(a))
        });
        if storage.is_empty() {
            observer.note(Note::info(format!(
                "{} has no storage statistics (pass --stats-from with an .abf or \
                 .pbix export); --sort size keeps name order",
                report_name(&paired.model)
            )))?;
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
        let target = binding.target.to_string();
        let provenance = binding.edge.to_string();
        let entry = Entry::broken_visual(&target, reason_code(&binding.reason), &provenance);
        compare::insert(&mut detected, &entry);
        if is_ignored_display(&target, patterns) {
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
        let is_existing = existed(&entry);
        if is_existing {
            existing += 1;
            if !sarif_mode {
                continue;
            }
        }
        if let Some(locator) = &mut locator {
            locator.binding(&entry, binding.edge.report.as_ref());
        }
        let reason = reason_code(&binding.reason);
        let (bound_artifact, bound_artifact_report) = match &binding.reason {
            BrokenReason::BoundArtifactBroken {
                artifact,
                report_index,
            } => (
                Some(artifact.to_string()),
                report_index.map(|index| report_paths[index].display().to_string()),
            ),
            _ => (None, None),
        };
        let out = BrokenOut {
            target,
            reason,
            bound_artifact,
            bound_artifact_report,
            provenance,
            location: binding.edge.located().to_string(),
        };
        if is_existing {
            kept_existing.broken.push(out);
        } else {
            broken.push(out);
        }
    }

    let mut broken_artifacts = Vec::new();
    let mut broken_artifacts_hidden = 0;
    let mut broken_artifacts_suppressed = 0;
    for artifact in graph.broken_artifacts() {
        let id = artifact.id.to_string();
        // The report *name*, never its path: two checkouts live in different
        // folders.
        let entry = Entry::broken_artifact(
            &id,
            artifact
                .report_index
                .map(|index| report_name(&report_paths[index])),
        );
        compare::insert(&mut detected, &entry);
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
        let is_existing = existed(&entry);
        if is_existing {
            existing += 1;
            if !sarif_mode {
                continue;
            }
        }
        if let Some(locator) = &mut locator {
            locator.object(&entry, &artifact.id, artifact.report_index);
        }
        let out = BrokenArtifactOut {
            id,
            kind: output::kind_of(&artifact.id),
            report: artifact
                .report_index
                .map(|index| report_paths[index].display().to_string()),
            unresolved_references: artifact.unresolved_references.clone(),
        };
        if is_existing {
            kept_existing.broken_artifacts.push(out);
        } else {
            broken_artifacts.push(out);
        }
    }

    if let Some(probe) = probe {
        *probe = match uncovered {
            Some(detail) => Baseline::skipped(detected, detail),
            None => Baseline::scanned(detected),
        };
        return Ok(None);
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
        kept: graph.kept().len(),
        queried: graph.queried().len(),
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
        model_bytes: model.value.storage_bytes,
        unused_storage,
        stats_source: stats_source.map(|path| path.display().to_string()),
        elapsed: None,
        compare: before
            .zip(request.compare.as_ref())
            .map(|(before, compare)| CompareOut {
                root: compare.root.display().to_string(),
                label: compare
                    .label
                    .clone()
                    .unwrap_or_else(|| compare.root.display().to_string()),
                coverage: before.coverage,
                coverage_detail: before.detail.clone(),
                existing,
                unverified,
                fixed: before.fixed(&detected),
            }),
    };
    let removable: Vec<ObjectId> = reported
        .into_iter()
        .chain(dead_machinery)
        .cloned()
        .collect();
    drop(graph);
    Ok(Some(Scan::Done(Box::new(ScanOutcome {
        output,
        existing: kept_existing,
        locator,
        removable,
        model: model.value,
        config,
        selected,
        workbooks,
    }))))
}

/// A bound-report walk's result, kept for the announcements and the
/// zero-connected-report diagnostics. Always present in model mode; present
/// in PATH mode when a model-naming PATH turns a plain `--report` folder
/// into a search root (issue #67).
pub struct ModelScan {
    /// How every discovered report item binds to the model.
    pub bound: discover::BoundReports,
    /// The folders actually walked, for the refusal message.
    pub search_roots: Vec<PathBuf>,
}

/// What a command names as its scan inputs, for [`resolve_target`]: the
/// struct a new command fills in to inherit the whole pairing ladder.
pub struct TargetInput<'a> {
    /// `--model`: model mode; disables the PATH ladder and discovery.
    pub model: Option<&'a Path>,
    /// The positional PATH argument, when the command has one.
    pub path: Option<&'a Path>,
    /// The `ripbi.toml` `target`, already resolved against the config dir.
    pub config_target: Option<&'a Path>,
    /// The `ripbi.toml` `[apps]` entries pairing Fabric apps with models.
    pub app_mappings: &'a [crate::config::AppMapping],
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
///
/// # Errors
/// Any input that names no model, several, or an unusable report.
pub fn resolve_target(
    input: TargetInput<'_>,
    observer: &mut dyn Observer,
) -> Result<Resolved, ScanError> {
    let resolved = |(paired, walk)| Resolved {
        paired,
        walk,
        discovered: None,
    };
    if let Some(model_path) = input.model {
        let (paired, walk) = resolve_model_mode(model_path, input.extras, input.app_mappings)?;
        return Ok(resolved((paired, Some(walk))));
    }
    if let Some(explicit) = input.path.or(input.config_target) {
        let model_named = names_semantic_model(explicit);
        let paired = resolve_explicit(explicit)?;
        return Ok(resolved(attach_extras(
            paired,
            input.extras,
            model_named,
            input.app_mappings,
        )?));
    }
    if !input.extras.is_empty() {
        let paired = derive_model_from_reports(input.extras)?;
        return Ok(resolved((paired, None)));
    }
    let (paired, discovered) = discover_target(input.no_input, input.quiet, input.cwd, observer)?;
    let (paired, walk) = attach_extras(paired, input.extras, false, input.app_mappings)?;
    Ok(Resolved {
        paired,
        walk,
        discovered: Some(discovered),
    })
}

/// A resolved scan target: the pairing, the bound-report walk when one ran,
/// and how discovery found the target when nothing named it.
pub struct Resolved {
    pub paired: discover::Paired,
    pub walk: Option<ModelScan>,
    pub discovered: Option<Discovered>,
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
        if is_binary_workbook(extra) {
            return Err(binary_workbook_error(extra));
        }
        if extra.is_file() && ingest::is_workbook(extra) {
            return Err(ScanError::new(format!(
                "--report {} is a workbook, which cannot name its model",
                extra.display()
            ))
            .with_hint(
                "its connections name a dataset in the service, not a local model; pass --model",
            ));
        }
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
    app_mappings: &[crate::config::AppMapping],
) -> Result<(discover::Paired, ModelScan), ScanError> {
    let target = discover::resolve_model(model_path)?;
    let mut direct = Vec::new();
    let lower = file_name_lower(model_path);
    if model_path.is_file()
        && (lower.ends_with(".pbit") || lower.ends_with(".pbix"))
        && ingest::archive_has_report(model_path)
    {
        direct.push(model_path.to_path_buf());
    }
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
    let bound =
        discover::discover_bound_reports_with(&target, &search_roots, &excluded, app_mappings);
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
    app_mappings: &[crate::config::AppMapping],
) -> Result<(discover::Paired, Option<ModelScan>), ScanError> {
    let mut report_paths = paired.reports.clone();
    let mut direct = Vec::new();
    let mut search_roots = Vec::new();
    for extra in extras {
        if is_binary_workbook(extra) {
            return Err(binary_workbook_error(extra));
        }
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
    let bound =
        discover::discover_bound_reports_with(&target, &search_roots, &excluded, app_mappings);
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

/// Sorts one `--report` value in model mode into a direct report item or a
/// search folder; a `.pbip` expands to its project's reports as direct items.
/// A `.Report`-named folder without a report anchor is malformed — fail
/// before any walk rather than at ingestion.
pub fn partition_report_value(
    path: &Path,
    direct: &mut Vec<PathBuf>,
    search_roots: &mut Vec<PathBuf>,
) -> Result<(), ScanError> {
    if is_binary_workbook(path) {
        return Err(binary_workbook_error(path));
    }
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
/// not a `.pbip` file — the project's reports become direct anchors. Its
/// model is never paired: the model names itself (via `--model` or the
/// anchors), so a thin report project with no model beside it still counts.
fn pbip_reports(path: &Path) -> Result<Option<Vec<PathBuf>>, ScanError> {
    if !path.is_file() || !file_name_lower(path).ends_with(".pbip") {
        return Ok(None);
    }
    discover::report_items_without_model(path).map(Some)
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
    let elsewhere = scan.bound.ignored_elsewhere.len() + scan.bound.apps_elsewhere.len();
    if elsewhere == 0 && scan.bound.unresolved.is_empty() {
        format!("no report items found under {roots}")
    } else {
        format!(
            "{elsewhere} bound to other models, {} unresolved dataset references under {roots}",
            scan.bound.unresolved.len()
        )
    }
}

/// The display name of a report path: its final folder component.
pub fn report_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The display form of a name wall: the first three, then the tail as a
/// count — the capping the by-name collapse prints (issue #65), shared with
/// the ignored list so every name list reads the same way.
pub fn capped_names(names: &[String]) -> String {
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
    if path.is_file() {
        return discover::is_model_file(path);
    }
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
/// Several candidates go to the observer's picker unless `no_input` or
/// `quiet` rules a prompt out.
fn discover_target(
    no_input: bool,
    quiet: bool,
    cwd: &Path,
    observer: &mut dyn Observer,
) -> Result<(discover::Paired, Discovered), ScanError> {
    let candidates = discover::discover(cwd);
    match candidates.as_slice() {
        [] => Err(
            ScanError::new(format!("no Power BI projects found in {}", cwd.display()))
                .with_hint("run inside a .pbip project folder, or pass a PATH"),
        ),
        [only] => {
            let paired = discover::pair(only)?;
            Ok((paired, Discovered::Only(only.label.clone())))
        }
        many => {
            if no_input || quiet {
                return Err(ambiguous_error(cwd, many));
            }
            let Some(candidate) = observer.pick(cwd, many)?.and_then(|index| many.get(index))
            else {
                return Err(ambiguous_error(cwd, many));
            };
            let paired = discover::pair(candidate)?;
            Ok((paired, Discovered::Selected(candidate.label.clone())))
        }
    }
}

/// A model fetched by `ripbi fetch` whose manifest (`.ripbi/source.json`
/// beside it) lists downstream reports that could not be downloaded. The
/// scan cannot see their bindings, so objects only they use come back as
/// unused: an `incomplete_report_set` skip notice, which `--strict` fails on.
fn fetch_completeness(model_path: &Path) -> Option<SkipNoticeOut> {
    const SHOWN: usize = 5;
    let root = model_path.parent()?;
    let manifest = Manifest::load(root)?;
    let folder = model_path.file_name()?.to_str()?;
    if !manifest.model.path.eq_ignore_ascii_case(folder) {
        return None;
    }
    let missing: Vec<String> = manifest
        .reports
        .iter()
        .filter(|report| report.status != ReportStatus::Downloaded)
        .map(|report| format!("'{}' ({})", report.name, report.workspace_name))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut named = missing
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if missing.len() > SHOWN {
        named.push_str(&format!(" and {} more", missing.len() - SHOWN));
    }
    Some(SkipNoticeOut {
        path: root.join(MANIFEST_PATH).display().to_string(),
        location: None,
        kind: "incomplete_report_set",
        detail: format!(
            "{} of {} downstream reports could not be fetched, so objects only they use show as unused: {named}",
            missing.len(),
            manifest.reports.len()
        ),
    })
}

fn ambiguous_error(dir: &Path, candidates: &[Candidate]) -> ScanError {
    let listing: String = candidates
        .iter()
        .map(|candidate| format!("\n  - {}", candidate.label))
        .collect();
    ScanError::new(format!("multiple projects in {}:{listing}", dir.display()))
        .with_hint("pass a PATH to scan one project; --no-input keeps this non-interactive")
}

/// Whether any scanned report is an Excel workbook.
pub fn has_workbooks(reports: &[PathBuf]) -> bool {
    reports
        .iter()
        .any(|path| path.is_file() && ingest::is_workbook(path))
}

/// Whether `path` names a binary Excel workbook (`.xls`, `.xlsb`, `.xlt`),
/// which ripbi cannot read: only Open XML workbooks are report inputs.
fn is_binary_workbook(path: &Path) -> bool {
    let name = file_name_lower(path);
    [".xls", ".xlsb", ".xlt"]
        .iter()
        .any(|extension| name.ends_with(extension))
}

/// The refusal for a binary workbook passed to `--report` (exit `2`).
fn binary_workbook_error(path: &Path) -> ScanError {
    ScanError::new(format!(
        "--report {} is a binary workbook, which ripbi cannot read",
        path.display()
    ))
    .with_hint("save it as .xlsx (or .xlsm) in Excel and pass that instead")
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
/// whatever it is named), or a Fabric app folder (`fabric.yaml`), which core
/// reads as a code-first consumer.
fn is_report_item(path: &Path) -> bool {
    if ingest::is_fabric_app(path) || path.is_file() && ingest::is_workbook(path) {
        return true;
    }
    if path.is_file() {
        return (file_name_lower(path).ends_with(".pbix")
            || file_name_lower(path).ends_with(".pbit"))
            && ingest::archive_has_report(path);
    }
    path.is_dir()
        && (path.join("report.json").is_file()
            || path.join("definition").join("report.json").is_file())
}

/// Drops paths already seen (compared canonically), keeping first occurrence.
pub fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
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

/// The workspace-monitoring query log to read: `--queries-from` (taken as
/// written, like `--model`), else the config's `queries_from`.
pub fn query_log_path(flag: Option<&Path>, config: Option<&config::Config>) -> Option<PathBuf> {
    flag.map(Path::to_path_buf)
        .or_else(|| config.and_then(|config| config.queries_from.clone()))
}

/// Which model's rows to read from a query log: `--queries-item`, else the
/// config's `queries_item`. `None` falls back to the model's own name.
pub fn query_log_item<'a>(
    flag: Option<&'a str>,
    config: Option<&'a config::Config>,
) -> Option<&'a str> {
    flag.or_else(|| config.and_then(|config| config.queries_item.as_deref()))
}

/// Reads the query log `scan` and `deps` add as roots, when one is set. An
/// empty [`QueryLog`] when none is: the graph is then built exactly as
/// without one. A log that cannot be read fails the run — it was asked for
/// by name, like an explicit `--stats-from`. Rows the parser skipped join
/// `skips`; the log's size and window are notes.
///
/// One export can cover a whole workspace. A log that names several models
/// is narrowed to this one — `item` (`--queries-item`), else the model's
/// folder or file name — because another model's `'Date'[Year]` query says
/// nothing about this one's. When no model in such a log matches, or an
/// explicit `item` matches none, none of its queries count. The log is
/// supplementary root evidence: a model nobody queried is a real answer, and
/// zero queries is exactly the no-log baseline. Every such mismatch — and a
/// single-model log under another name, whose queries do count — is a
/// `stale_state` skip notice, so `--strict` fails on it.
pub fn load_query_log(
    path: Option<PathBuf>,
    item: Option<&str>,
    model_path: &Path,
    observer: &mut dyn Observer,
    skips: &mut Vec<SkipNoticeOut>,
) -> Result<QueryLog, ScanError> {
    let Some(path) = path else {
        return Ok(QueryLog::default());
    };
    let ingested = usage::read_query_log(&path).map_err(|error| {
        ScanError::new(format!("cannot read queries from {}: {error}", path.display())).with_hint(
            "export SemanticModelLogs QueryEnd rows (with EventText) as CSV or JSON; see docs/deps.md",
        )
    })?;
    skips.extend(ingested.skips.iter().map(skip_notice_out));
    let mut log = ingested.value;

    let model = model_name(model_path);
    let key = item.or(model.as_deref());
    let mut notes = Vec::new();
    // A log that does not identify this model is a skip notice, not a note:
    // the run goes on, but `--strict` — a CI gate — fails on it, so a typo'd
    // `--queries-item` or a renamed model never passes silently.
    let mut unmatched = None;
    if !log.item_names.is_empty() {
        match key.filter(|key| log.names_item(key)) {
            // Several models: keep this one's rows.
            Some(key) if log.item_names.len() > 1 => {
                let total = log.queries.len();
                let dropped = log.retain_item(key);
                notes.push(format!(
                    "the query log covers {} models; ignored {dropped} of {total} queries \
                     logged against the others",
                    log.item_names.len(),
                ));
            }
            Some(_) => {}
            // Several models, none of them this one (or an explicit item the
            // log lacks): only rows that name no model are left to count.
            None if item.is_some() || log.item_names.len() > 1 => {
                log.retain_item("");
                let names = capped_names(&log.item_names);
                unmatched = Some(match key {
                    Some(key) => format!(
                        "the query log has no queries for {key} (it names {names}); none count \
                         — pass --queries-item NAME if the service calls it something else"
                    ),
                    None => format!(
                        "the query log covers {} models ({names}); none count \
                         — pass --queries-item NAME to pick this one",
                        log.item_names.len()
                    ),
                });
            }
            // One model under another name: most likely this one, renamed
            // locally. Keep its queries, but say so.
            None => {
                if let Some(model) = &model {
                    unmatched = Some(format!(
                        "the query log names {}, not {model}; its queries count, but may belong \
                         to another model — pass --queries-item NAME to confirm",
                        capped_names(&log.item_names)
                    ));
                }
            }
        }
    }
    if let Some(detail) = unmatched {
        skips.push(SkipNoticeOut {
            path: path.display().to_string(),
            location: None,
            kind: "stale_state",
            detail,
        });
    }
    let window = match (log.first_seen.as_deref(), log.last_seen.as_deref()) {
        (Some(first), Some(last)) if day(first) == day(last) => format!(" ({})", day(first)),
        (Some(first), Some(last)) => format!(" ({} → {})", day(first), day(last)),
        _ => String::new(),
    };
    observer.note(Note::info(format!(
        "{} logged queries{window} from {} count as consumers",
        log.queries.len(),
        path.display()
    )))?;
    for note in notes {
        observer.note(Note::warn(note))?;
    }
    Ok(log)
}

/// The date part of an exported timestamp (`2026-09-30 11:11:05.65` or ISO 8601).
fn day(stamp: &str) -> &str {
    stamp
        .split([' ', 'T'])
        .next()
        .filter(|date| !date.is_empty())
        .unwrap_or(stamp)
}

/// The model's name as the service would log it: the `.SemanticModel`
/// folder's stem, or a PBIX/PBIT/ABF file's. `None` when the path names no
/// such item (a bare `model.bim`), so no mismatch is ever guessed.
fn model_name(model_path: &Path) -> Option<String> {
    model_path.ancestors().find_map(|path| {
        let name = path.file_name()?.to_str()?;
        [".SemanticModel", ".Dataset", ".pbix", ".pbit", ".abf"]
            .iter()
            .find_map(|suffix| {
                name.len()
                    .checked_sub(suffix.len())
                    .filter(|&cut| {
                        name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(suffix)
                    })
                    .map(|cut| name[..cut].to_string())
            })
    })
}

/// Attaches storage sizes from outside the model (issue #129): the explicit
/// `--stats-from`/`[scan].stats_from` source, else a PBIP's `.pbi/cache.abf`.
/// Returns the source used, if any.
///
/// A model with its own catalog (PBIX, `.abf`) keeps it: `--stats-from` on one is
/// a usage error, and the config setting is ignored. An explicit source that
/// cannot be read fails the scan; an auto-detected cache only earns a note, so
/// a stale or foreign `.pbi/` folder never blocks the analysis. Coverage and
/// staleness notes are informational — `--strict` does not gate on them.
fn attach_storage(
    model: &mut ripbi_core::TabularDatabase,
    model_path: &Path,
    setting: Option<config::StatsSetting>,
    from_flag: bool,
    verbose: bool,
    observer: &mut dyn Observer,
) -> Result<Option<PathBuf>, ScanError> {
    if model.storage_bytes.is_some() {
        if from_flag && matches!(setting, Some(config::StatsSetting::Path(_))) {
            return Err(ScanError::new(format!(
                "--stats-from cannot be used with {}: it carries its own storage statistics",
                model_path.display()
            ))
            .with_hint("--stats-from adds sizes to PBIP, TMDL, model.bim, and PBIT models"));
        }
        return Ok(None);
    }
    let (source, explicit) = match setting {
        Some(config::StatsSetting::Off) => return Ok(None),
        Some(config::StatsSetting::Path(path)) => (path, true),
        None => match ingest::pbip_storage_cache(model_path) {
            Some(cache) => (cache, false),
            None => return Ok(None),
        },
    };
    let stats = match ingest::storage_source(&source) {
        Ok(stats) => stats.value,
        Err(error) if explicit => {
            return Err(ScanError::new(format!(
                "cannot read stats from {}: {error}",
                source.display()
            ))
            .with_hint(
                "pass an .abf backup, a .pbix saved with its data, or a .vpax, or --stats-from none",
            ));
        }
        Err(error) => {
            observer.note(Note::warn(format!(
                "cannot read stats from {}: {error}; continuing without sizes",
                source_label(&source, model_path, verbose)
            )))?;
            return Ok(None);
        }
    };
    let coverage = model.attach_storage(&stats);
    {
        let label = source_label(&source, model_path, verbose);
        // Full coverage is the expected case: it earns a line only on request.
        // Partial coverage leaves some sizes blank, which changes the totals.
        if coverage.matched < coverage.total || verbose {
            let line = format!(
                "stats from {label} cover {} of {} tables and columns",
                coverage.matched, coverage.total
            );
            observer.note(if coverage.matched < coverage.total {
                Note::warn(line)
            } else {
                Note::info(line)
            })?;
        }
        if is_older_than_model(&source, model_path) {
            observer.note(Note::warn(format!(
                "stats may be stale: {label} is older than the model files"
            )))?;
        }
        if holds_no_data(model) {
            observer.note(Note::warn(format!(
                "{label} has no rows in any table loaded by Power Query; was the \
                 model refreshed before it was saved or exported? Sizes are the \
                 engine's empty minimums"
            )))?;
        }
    }
    Ok(Some(source))
}

/// A stats source as notes name it: relative to the model folder when it
/// lives inside it (`.pbi/cache.abf`), else as given; the full path under
/// `--verbose`.
fn source_label(source: &Path, model_path: &Path, verbose: bool) -> String {
    if verbose {
        return source.display().to_string();
    }
    source
        .strip_prefix(model_path)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| source.display().to_string())
}

/// Whether the tables that load data (Power Query or a source query) got
/// statistics but no rows: a model saved or exported before its first
/// refresh, such as a PBIP opened in Desktop and exported to `.vpax` straight
/// away. Calculated tables are left out; the engine fills them without one.
fn holds_no_data(model: &ripbi_core::TabularDatabase) -> bool {
    let mut loaded = model
        .tables
        .iter()
        .filter(|table| {
            table.partitions.iter().any(|partition| {
                matches!(
                    partition.source,
                    PartitionSource::M { .. } | PartitionSource::Query { .. }
                )
            })
        })
        .filter_map(|table| table.storage)
        .peekable();
    loaded.peek().is_some() && loaded.all(|stats| stats.rows.is_none_or(|rows| rows == 0))
}

/// Whether `source` was last written before the newest file of the model.
/// Unreadable timestamps never warn.
fn is_older_than_model(source: &Path, model: &Path) -> bool {
    fn newest(path: &Path) -> Option<std::time::SystemTime> {
        let meta = std::fs::metadata(path).ok()?;
        if meta.is_file() {
            return meta.modified().ok();
        }
        std::fs::read_dir(path)
            .ok()?
            .flatten()
            // `.pbi/` holds the cache itself and Desktop's settings, not the model.
            .filter(|entry| entry.file_name() != ".pbi")
            .filter_map(|entry| newest(&entry.path()))
            .max()
    }
    let Some(written) = std::fs::metadata(source)
        .and_then(|meta| meta.modified())
        .ok()
    else {
        return false;
    };
    newest(model).is_some_and(|model| written < model)
}

fn unused_storage(
    reported: &[&ObjectId],
    storage: &HashMap<ObjectId, StorageStats>,
) -> Option<UnusedStorage> {
    let tables: HashSet<&NameKey> = reported
        .iter()
        .filter_map(|id| match id {
            ObjectId::Table { table } => Some(table),
            _ => None,
        })
        .collect();
    let mut total: Option<UnusedStorage> = None;
    for id in reported {
        let Some(stats) = storage.get(*id) else {
            continue;
        };
        let Some(bytes) = stats.bytes else {
            continue;
        };
        let total = total.get_or_insert(UnusedStorage {
            bytes: 0,
            objects: 0,
            lower_bound: false,
            in_memory: false,
        });
        total.objects += 1;
        total.lower_bound |= stats.basis == SizeBasis::LowerBound;
        total.in_memory |= stats.basis == SizeBasis::Engine;
        let covered = !matches!(id, ObjectId::Table { .. })
            && id
                .owning_table()
                .is_some_and(|table| tables.contains(table));
        if !covered {
            total.bytes = total.bytes.saturating_add(bytes);
        }
    }
    total
}

/// A broken binding's reason, as the snake_case code `--plain`/`--json` emit.
fn reason_code(reason: &BrokenReason) -> &'static str {
    match reason {
        BrokenReason::BoundArtifactBroken { .. } => "bound_artifact_broken",
        BrokenReason::TableNotFound => "table_not_found",
        BrokenReason::FieldNotFound => "field_not_found",
        BrokenReason::MeasureNotFound => "measure_not_found",
        BrokenReason::HierarchyNotFound => "hierarchy_not_found",
        BrokenReason::LevelNotFound => "level_not_found",
    }
}

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
        ObjectId::Bookmark { bookmark, .. } => vec![bookmark.label()],
    }
}

/// The selected finding kinds: `--broken` unioned with every `--type` value,
/// in the same machine vocabulary `deps --type` speaks. An
/// unknown kind is a usage error, never silence — and `broken_visual` is
/// deliberately outside the vocabulary, because selecting it is what makes
/// breakage gate the exit code, and that is `--broken`'s job alone (issue
/// #60: advisory until asked).
pub fn selected_kinds(
    broken: bool,
    types: &[String],
) -> Result<Option<HashSet<&'static str>>, ScanError> {
    if !broken && types.is_empty() {
        return Ok(None);
    }
    let mut picks: HashSet<&'static str> = if broken {
        HashSet::from(["broken_visual", "broken_artifact"])
    } else {
        HashSet::new()
    };
    for kind in types {
        let Some(static_kind) = output::KINDS.iter().find(|known| known == &kind) else {
            return Err(
                ScanError::new(format!("--type {kind} is not an object type")).with_hint(format!(
                    "one of: {}; broken findings are selected by --broken",
                    output::KINDS.join(", ")
                )),
            );
        };
        picks.insert(static_kind);
    }
    Ok(Some(picks))
}

/// Maps a core skip notice to its output DTO — the shared shape of every
/// command's `skips` array and stderr block.
pub fn skip_notice_out(notice: &SkipNotice) -> SkipNoticeOut {
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

pub fn skip_kind_out(kind: SkipKind) -> &'static str {
    match kind {
        SkipKind::UnknownObject => "unknown_object",
        SkipKind::UnknownProperty => "unknown_property",
        SkipKind::MalformedValue => "malformed_value",
        SkipKind::UnresolvedAlias => "unresolved_alias",
        SkipKind::StaleState => "stale_state",
        SkipKind::OpaqueSource => "opaque_source",
        SkipKind::NotAnalyzed => "not_analyzed",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ripbi_core::{NameKey, ObjectId, SizeBasis, StorageStats};

    use super::{
        SkipNoticeOut, UnusedStorage, dedupe_opaque_skips, partition_report_value, unused_storage,
    };

    /// A thin report project (`byConnection`, no model beside it) passed to
    /// `--report` expands to its report, not a "no semantic model" error.
    #[test]
    fn a_thin_pbip_report_expands_without_a_model() {
        let root = std::env::temp_dir().join(format!("ripbi-thin-pbip-{}", std::process::id()));
        let report = root.join("Thin.Report");
        std::fs::create_dir_all(report.join("definition")).unwrap();
        std::fs::write(report.join("definition").join("report.json"), "{}").unwrap();
        std::fs::write(root.join("Thin.pbip"), "{}").unwrap();

        let (mut direct, mut search_roots) = (Vec::new(), Vec::new());
        let result =
            partition_report_value(&root.join("Thin.pbip"), &mut direct, &mut search_roots);
        std::fs::remove_dir_all(&root).unwrap();
        result.expect("a thin .pbip is a report anchor");
        assert_eq!(direct, vec![report]);
        assert!(search_roots.is_empty());
    }

    fn size(bytes: u64, basis: SizeBasis) -> StorageStats {
        StorageStats {
            bytes: Some(bytes),
            basis,
            rows: None,
            cardinality: None,
        }
    }

    /// Issue #122: a reported table's size already covers its reported
    /// columns and relationships; other tables' objects still count.
    #[test]
    fn unused_storage_counts_each_file_once() {
        let table = |name: &str| ObjectId::Table {
            table: NameKey::new(name),
        };
        let column = |table: &str, name: &str| ObjectId::Column {
            table: NameKey::new(table),
            column: NameKey::new(name),
        };
        let relationship = ObjectId::Relationship {
            from_table: NameKey::new("Old"),
            from_column: NameKey::new("Key"),
            to_table: NameKey::new("Dim"),
            to_column: NameKey::new("Key"),
        };
        let measure = ObjectId::Measure {
            table: NameKey::new("Sales"),
            measure: NameKey::new("Total"),
        };
        let storage = HashMap::from([
            (table("Old"), size(1000, SizeBasis::Files)),
            (column("old", "A"), size(400, SizeBasis::Files)),
            (relationship.clone(), size(50, SizeBasis::Files)),
            (column("Sales", "B"), size(30, SizeBasis::LowerBound)),
        ]);
        let reported = [
            &column("old", "A"),
            &table("Old"),
            &relationship,
            &column("Sales", "B"),
            &measure,
        ];
        assert_eq!(
            unused_storage(&reported, &storage),
            Some(UnusedStorage {
                bytes: 1030,
                objects: 4,
                lower_bound: true,
                in_memory: false,
            })
        );
        assert_eq!(unused_storage(&[&measure], &storage), None);
    }

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
