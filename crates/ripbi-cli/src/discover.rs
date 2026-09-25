//! Path discovery: turning a PATH argument (or the working directory) into the
//! (model, reports) pairing one scan runs against.
//!
//! This is orchestration only — deciding *which* folders to feed core — never
//! analysis. Every parse still happens in `ripbi_core::ingest`; the one
//! exception is a report passed directly with no sibling model, where the
//! report is ingested once to read its `datasetReference.byPath`.
//!
//! Pairing rules, mirroring the PBIP layout convention that a project's items
//! share its stem (`X.pbip`, `X.SemanticModel`, `X.Report`):
//!
//! - A `.pbip` file pairs with its stem-named items in the same folder; a
//!   project with no model folder of its own pairs with the model its
//!   stem-named `.Report`'s `definition.pbir` names.
//! - A `.SemanticModel` folder pairs with stem-named `.Report` siblings, or
//!   — in a folder dedicated to one project — with all `.Report` siblings.
//! - A `.Report` folder pairs with its stem-named model, then the sole model
//!   sibling, then the model its `definition.pbir` names — by written
//!   `byPath`, else by the `byConnection` dataset name when it matches
//!   exactly one sibling model's stem or `.platform` display name.
//! - A `--model` target — a `.SemanticModel` folder, its `definition/`, a
//!   bare `model.tmdl` folder, a `.pbip` naming the project, `model.bim`, or
//!   a PBIT — pairs with
//!   every report item found under its search folders whose `definition.pbir`
//!   resolves to it, whose folder stem names it (`X.Report` beside
//!   `X.SemanticModel`), or whose `byConnection` names its dataset.
//! - A PBIT supplies its own model and report; a PBIX report pairs with one
//!   unambiguous sibling model, while a bare `model.bim` follows model-only rules.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use ripbi_core::DatasetReference;
use ripbi_core::ingest::{self, SkipNotice};

use crate::error::ScanError;

/// A model and the reports one scan runs against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
    /// The semantic-model folder to ingest.
    pub model: PathBuf,
    /// Every report item to ingest as reachability roots, in discovery order.
    pub reports: Vec<PathBuf>,
}

/// One pickable entry in a folder being searched for projects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What the picker and ambiguity errors display.
    pub label: String,
    /// The path shown next to the label (the `.pbip` file or model folder).
    pub path: PathBuf,
    /// The folder the project's items live in, when this is a project.
    pub project_root: Option<PathBuf>,
    /// The project stem, when this is a project.
    pub stem: Option<String>,
    /// True for archive entries (`.pbix` and `.pbit`) in the picker.
    pub is_archive: bool,
}

/// What resolving an explicit PATH produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The path unambiguously maps to a scan target.
    Paired(Paired),
    /// The path is a folder holding several projects; the user must pick.
    Ambiguous {
        dir: PathBuf,
        candidates: Vec<Candidate>,
    },
    /// An archive without the entries needed for a scan.
    Archive(PathBuf),
    /// Nothing ripbi recognizes.
    Unrecognized(PathBuf),
}

/// Resolves an existing PATH argument into a scan target.
///
/// # Errors
/// When the path is recognizable as a project but pairs with no model —
/// a `.pbip` with no semantic model beside it, say.
pub fn resolve_path(path: &Path) -> Result<Resolution, ScanError> {
    if path.is_file() {
        return resolve_file(path);
    }
    if path.is_dir() {
        return resolve_dir(path);
    }
    Ok(Resolution::Unrecognized(path.to_path_buf()))
}

fn resolve_file(path: &Path) -> Result<Resolution, ScanError> {
    let name = file_name(path);
    let lower = name.to_lowercase();
    if lower.ends_with(".pbit") {
        if ingest::archive_has_model(path) && ingest::archive_has_report(path) {
            return Ok(Resolution::Paired(Paired {
                model: path.to_path_buf(),
                reports: vec![path.to_path_buf()],
            }));
        }
        return Ok(Resolution::Archive(path.to_path_buf()));
    }
    if lower.ends_with(".bim") {
        return Ok(Resolution::Paired(Paired {
            model: path.to_path_buf(),
            reports: Vec::new(),
        }));
    }
    if lower.ends_with(".pbix") {
        if !ingest::archive_has_report(path) {
            return Ok(Resolution::Archive(path.to_path_buf()));
        }
        let parent = parent_of(path);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let model = sibling_model(&parent, &stem);
        return model
            .map(|model| {
                Resolution::Paired(Paired {
                    model,
                    reports: vec![path.to_path_buf()],
                })
            })
            .ok_or_else(|| {
                ScanError::no_model(format!(
                    "cannot locate the semantic model for report {}",
                    path.display()
                ))
                .with_hint("pass --model <PBIP or model.bim> with --report <PBIX>")
            });
    }
    if let Some(stem) = strip_suffix(&name, ".pbip") {
        let root = parent_of(path);
        return pair_project(&root, &stem).map(Resolution::Paired);
    }
    Ok(Resolution::Unrecognized(path.to_path_buf()))
}

fn resolve_dir(path: &Path) -> Result<Resolution, ScanError> {
    let name = file_name(path);
    let lower = name.to_lowercase();
    if lower.ends_with(".semanticmodel") || path.join("model.tmdl").is_file() {
        return pair_model(path).map(Resolution::Paired);
    }
    if lower.ends_with(".report") || path.join("report.json").is_file() {
        return pair_report(path).map(Resolution::Paired);
    }
    let stems = project_stems(path);
    match stems.len() {
        0 => {
            let candidates = discover(path);
            match candidates.len() {
                0 => Ok(Resolution::Unrecognized(path.to_path_buf())),
                1 => match resolve_path(&candidates[0].path)? {
                    Resolution::Paired(paired) => Ok(Resolution::Paired(paired)),
                    other => Ok(other),
                },
                _ => Ok(Resolution::Ambiguous {
                    dir: path.to_path_buf(),
                    candidates,
                }),
            }
        }
        1 => {
            let stem = stems.values().next().expect("exactly one stem").clone();
            pair_project(path, &stem).map(Resolution::Paired)
        }
        _ => Ok(Resolution::Ambiguous {
            dir: path.to_path_buf(),
            candidates: discover(path),
        }),
    }
}

/// Pairs a candidate from folder discovery.
pub fn pair(candidate: &Candidate) -> Result<Paired, ScanError> {
    if candidate.path.is_file() && candidate.is_archive {
        return match resolve_path(&candidate.path)? {
            Resolution::Paired(paired) => Ok(paired),
            _ => Err(archive_error(&candidate.path)),
        };
    }
    let (Some(root), Some(stem)) = (&candidate.project_root, &candidate.stem) else {
        return Err(archive_error(&candidate.path));
    };
    pair_project(root, stem)
}

/// Pairs a project stem with its model and report items in `root`.
pub fn pair_project(root: &Path, stem: &str) -> Result<Paired, ScanError> {
    let model =
        model_for_project_stem(root, stem).or_else(|| model_from_project_reports(root, stem));
    let Some(model) = model else {
        return Err(ScanError::no_model(format!(
            "no semantic model for project '{stem}' in {}",
            root.display()
        ))
        .with_hint(
            "a .pbip project pairs with a '<stem>.SemanticModel' folder beside it, \
             or with the model its '<stem>.Report' pairs with — or list the report \
             alone: ripbi report --allow-no-model <report>",
        ));
    };
    let reports = project_reports(root, stem);
    Ok(Paired { model, reports })
}

/// The semantic-model item of project `stem` in `root`: the stem-named
/// folder, else the sole `.SemanticModel` sibling.
fn model_for_project_stem(root: &Path, stem: &str) -> Option<PathBuf> {
    stem_item(root, stem, "SemanticModel").or_else(|| {
        sole_child_matching(root, |name| name.to_lowercase().ends_with(".semanticmodel"))
    })
}

/// The model a project's own report pairs with, for a project that carries
/// no model folder of its own — a report-only `.pbip` names its dataset in
/// the report's `definition.pbir`, by path or (unambiguously) by name.
fn model_from_project_reports(root: &Path, stem: &str) -> Option<PathBuf> {
    project_reports(root, stem).into_iter().find_map(|report| {
        let parent = parent_of(&report);
        model_from_dataset_reference(&report, &parent)
    })
}

/// Pairs a semantic-model folder with its report siblings.
pub fn pair_model(model: &Path) -> Result<Paired, ScanError> {
    let reports = sibling_reports(model);
    Ok(Paired {
        model: model.to_path_buf(),
        reports,
    })
}

/// Pairs a report folder with its model: stem-named sibling, sole model
/// sibling, then the model its `definition.pbir` names — by written `byPath`,
/// else by a `byConnection` dataset name matching exactly one sibling model.
pub fn pair_report(report_dir: &Path) -> Result<Paired, ScanError> {
    let parent = parent_of(report_dir);
    let stem =
        strip_suffix(&file_name(report_dir), ".Report").unwrap_or_else(|| file_name(report_dir));
    let model = stem_item(&parent, &stem, "SemanticModel")
        .or_else(|| {
            sole_child_matching(&parent, |name| {
                name.to_lowercase().ends_with(".semanticmodel")
            })
        })
        .or_else(|| model_from_dataset_reference(report_dir, &parent));
    let Some(model) = model else {
        return Err(ScanError::no_model(format!(
            "cannot locate the semantic model for report {}",
            report_dir.display()
        ))
        .with_hint(
            "point PATH or --model at the model and pass the report with --report; \
             a byConnection report pairs by name only when exactly one sibling model's \
             stem or display name matches its dataset — or list the report alone: \
             ripbi report --allow-no-model <report>",
        ));
    };
    Ok(Paired {
        model,
        reports: vec![report_dir.to_path_buf()],
    })
}

/// The report items a path names, without pairing a model — the listing the
/// `report` command's `--allow-no-model` produces: a `.Report` item is
/// itself, a `.pbip` file or single-project folder expands to its project's
/// reports.
///
/// # Errors
/// When the path names no report items. Model folders, plain folders, and
/// missing paths are usage errors even under `--allow-no-model`:
/// the flag rescues a missing model, never a mistyped path.
pub fn report_items_without_model(path: &Path) -> Result<Vec<PathBuf>, ScanError> {
    if path.is_file() && ingest::archive_has_report(path) {
        return Ok(vec![path.to_path_buf()]);
    }
    if path.is_dir() {
        let lower = file_name(path).to_lowercase();
        if lower.ends_with(".report") || path.join("report.json").is_file() {
            return Ok(vec![path.to_path_buf()]);
        }
        let stems = project_stems(path);
        return match stems.len() {
            1 => Ok(project_reports(
                path,
                stems.values().next().expect("exactly one stem"),
            )),
            0 => Err(ScanError::new(format!(
                "{} does not name a report item or project",
                path.display()
            ))
            .with_hint(
                "--allow-no-model rescues a missing model, not a mistyped path — \
                 pass a .Report folder or a .pbip project",
            )),
            _ => Err(ScanError::new(format!(
                "several projects in {} — pass one report item or .pbip",
                path.display()
            ))
            .with_hint("--allow-no-model lists one project's reports")),
        };
    }
    if path.is_file()
        && let Some(stem) = strip_suffix(&file_name(path), ".pbip")
    {
        return Ok(project_reports(&parent_of(path), &stem));
    }
    if !path.exists() {
        return Err(ScanError::new(format!("no such path: {}", path.display()))
            .with_hint("pass a .Report folder or a .pbip project"));
    }
    Err(ScanError::new(format!(
        "{} does not name a report item or project",
        path.display()
    ))
    .with_hint(
        "--allow-no-model rescues a missing model, not a mistyped path — \
         pass a .Report folder or a .pbip project",
    ))
}

/// A resolved semantic-model item to scan named reports against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTarget {
    /// The semantic-model item root — the folder Power BI names
    /// `X.SemanticModel`, where `.platform` and `definition/` live.
    pub item_root: PathBuf,
    /// The `definition/` folder, as core's locator resolved it.
    pub definition: PathBuf,
    /// `X` when `item_root` is named `X.SemanticModel`; `None` otherwise.
    pub stem: Option<String>,
    /// `.platform`'s `metadata.displayName`, when the item carries one.
    pub display_name: Option<String>,
}

/// Report items discovered under the search folders, classified by how they
/// bind to one model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoundReports {
    /// Connected report item roots, sorted by canonical path.
    pub reports: Vec<PathBuf>,
    /// The connected reports matched by `byConnection`'s `initial catalog`
    /// rather than by path or stem, each with the catalog that matched.
    pub name_matched: Vec<(PathBuf, String)>,
    /// Report items bound to a different existing model — informational, and
    /// deliberately never fatal, so a healthy multi-model folder can be
    /// scanned with `--strict`.
    pub ignored_elsewhere: Vec<PathBuf>,
    /// Report items no tier matched: the item root and a one-sentence reason.
    pub unresolved: Vec<(PathBuf, String)>,
    /// Anchor-less `.Report` folders the walk pruned: malformed items whose
    /// bindings cannot be read, sorted by canonical path like the other
    /// buckets. Explicit `--report` items never land here — an anchor-less
    /// one fails before the walk, a valid one is excluded from it.
    pub malformed: Vec<PathBuf>,
    /// `definition.pbir` read or parse drift from the unresolved items.
    pub parse_skips: Vec<SkipNotice>,
    /// A filesystem error during the search. An incomplete walk cannot
    /// establish the full set of report roots.
    pub walk_error: Option<ScanError>,
}

/// The shapes `--model` accepts, listed whenever resolution fails.
const MODEL_SHAPES_HINT: &str = "pass a .pbip, .pbit, model.bim, a .SemanticModel folder, its definition/ folder, or any folder containing model.tmdl";

/// Resolves a `--model` PATH into the model item it names.
///
/// The accepted shapes are core's — a `.SemanticModel` folder, its
/// `definition/` subfolder, or any folder directly containing `model.tmdl` —
/// plus a `.pbip` file naming the project whose model to use, a TMSL `.bim`
/// file, or a PBIT with `DataModelSchema`.
///
/// # Errors
/// When `path` is none of those, or the `.pbip` names no model. The message
/// starts with core's "not a semantic model" (or the missing-project pairing
/// error) and the hint lists the accepted shapes.
pub fn resolve_model(path: &Path) -> Result<ModelTarget, ScanError> {
    if path.is_file() {
        let lower = file_name(path).to_lowercase();
        if lower.ends_with(".bim") || lower.ends_with(".pbit") {
            return model_target(path);
        }
        return match strip_suffix(&file_name(path), ".pbip") {
            Some(stem) => {
                let root = parent_of(path);
                let paired = pair_project(&root, &stem)?;
                model_target(&paired.model)
            }
            // Not a `.pbip`: let core's message say why a file is no model.
            None => model_target(path),
        };
    }
    model_target(path)
}

/// Resolves an already-located model item root into a [`ModelTarget`].
fn model_target(item_root: &Path) -> Result<ModelTarget, ScanError> {
    if item_root.is_file() {
        let lower = file_name(item_root).to_lowercase();
        if lower.ends_with(".bim") || lower.ends_with(".pbit") {
            let name = ingest::semantic_model(item_root)
                .map_err(|error| ScanError::new(error.to_string()))?
                .value
                .name;
            return Ok(ModelTarget {
                item_root: item_root.to_path_buf(),
                definition: item_root.to_path_buf(),
                stem: item_root
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned()),
                display_name: name,
            });
        }
    }
    let definition = ingest::locate_definition(item_root)
        .map_err(|error| ScanError::new(error.to_string()).with_hint(MODEL_SHAPES_HINT))?;
    let item_root = parent_of(&definition);
    let stem = strip_suffix(&file_name(&item_root), ".SemanticModel");
    let display_name = ingest::platform_display_name(&item_root);
    Ok(ModelTarget {
        item_root,
        definition,
        stem,
        display_name,
    })
}

/// Walks `roots` for report items and classifies each one's binding to
/// `model` — by written path, then folder stem, then dataset name; the first
/// tier to speak wins.
///
/// `exclude` holds canonical paths of explicitly passed report items: they are
/// never re-walked, so an explicit `--report` produces no walk notice. A
/// directory holding a report item is not searched further, an anchor-less
/// `.Report` directory is recorded as malformed rather than vanishing, and
/// unreadable directories fail the walk.
#[must_use]
pub fn discover_bound_reports(
    model: &ModelTarget,
    roots: &[PathBuf],
    exclude: &HashSet<PathBuf>,
) -> BoundReports {
    let mut items = Vec::new();
    let mut malformed = Vec::new();
    let mut visited = exclude.clone();
    for root in roots {
        if let Err(error) = walk_report_items(root, &mut visited, &mut items, &mut malformed) {
            return BoundReports {
                walk_error: Some(error),
                ..BoundReports::default()
            };
        }
    }
    items.sort_by_cached_key(|item| canonical_key(item));
    malformed.sort_by_cached_key(|path| canonical_key(path));

    let model_root = canonical_key(&model.item_root);
    let model_definition = canonical_key(&model.definition);

    let mut bound = BoundReports::default();
    for item in items {
        let (dataset, skips) = ingest::dataset_reference(&item);
        match classify_binding(model, &item, &dataset, &model_root, &model_definition) {
            Binding::Connected { catalog } => {
                if let Some(catalog) = catalog {
                    bound.name_matched.push((item.clone(), catalog));
                }
                bound.reports.push(item);
            }
            Binding::Elsewhere => bound.ignored_elsewhere.push(item),
            Binding::Unresolved(detail) => {
                bound.unresolved.push((item, detail));
                bound.parse_skips.extend(skips);
            }
        }
    }
    bound.malformed = malformed;
    bound
}

/// What one report item's dataset reference says about its binding to the
/// model under scan.
enum Binding {
    /// Connected; `Some(catalog)` when the hit was `byConnection`'s name.
    Connected { catalog: Option<String> },
    /// Bound to a different existing model — not this scan's business.
    Elsewhere,
    /// No tier matched; the string is the one-sentence reason.
    Unresolved(String),
}

/// The three pairing tiers, in order, each final once it speaks.
fn classify_binding(
    model: &ModelTarget,
    item: &Path,
    dataset: &DatasetReference,
    model_root: &Path,
    model_definition: &Path,
) -> Binding {
    // Tier 1: the written `byPath`. A hit on another existing folder is final
    // — the report says where it lives, and it does not live here. A dangling
    // path falls through: it may be stale while the name still holds.
    if let DatasetReference::ByPath { path } = dataset {
        let resolved = resolve_by_path(item, path);
        if resolved.is_dir()
            && let Ok(canonical) = resolved.canonicalize()
        {
            return if canonical == model_root || canonical == model_definition {
                Binding::Connected { catalog: None }
            } else {
                Binding::Elsewhere
            };
        }
    }

    // Tier 2: the PBIP stem convention, `X.Report` beside `X.SemanticModel`.
    if let Some(stem) = &model.stem
        && strip_suffix(&file_name(item), ".Report")
            .is_some_and(|report_stem| report_stem.eq_ignore_ascii_case(stem))
    {
        return Binding::Connected { catalog: None };
    }

    // Tier 3: the dataset name a live connection names.
    match dataset {
        DatasetReference::ByConnection { connection_string } => {
            let Some(catalog) = initial_catalog(connection_string) else {
                return Binding::Unresolved(
                    "byConnection carries no 'initial catalog' to match the model against"
                        .to_string(),
                );
            };
            if name_matches(model, &catalog) {
                return Binding::Connected {
                    catalog: Some(catalog),
                };
            }
            if model.stem.is_some() || model.display_name.is_some() {
                return Binding::Elsewhere;
            }
            Binding::Unresolved(format!(
                "byConnection names dataset '{catalog}', which the unnamed model cannot be matched against"
            ))
        }
        DatasetReference::ByPath { path } => Binding::Unresolved(format!(
            "datasetReference byPath '{path}' does not resolve to a folder and no name matches"
        )),
        DatasetReference::Unresolved => {
            Binding::Unresolved("definition.pbir carries no usable datasetReference".to_string())
        }
    }
}

/// Whether the dataset `catalog` names this model, by item stem or display name.
fn name_matches(model: &ModelTarget, catalog: &str) -> bool {
    model
        .stem
        .as_deref()
        .is_some_and(|stem| stem.eq_ignore_ascii_case(catalog))
        || model
            .display_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(catalog))
}

/// Collects report item roots under `dir`, recursively.
///
/// Entries are visited in name order (deterministic output); a directory
/// holding `report.json` or `definition/report.json` is itself an item and is
/// not searched further. Hidden and `.SemanticModel` directories are pruned
/// from descent silently; a `.Report` directory without an anchor cannot be a
/// report item and is pushed to `malformed` instead. Directory symlinks are
/// skipped outright, so a traversal cycle can never trap the walk.
fn walk_report_items(
    dir: &Path,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
    malformed: &mut Vec<PathBuf>,
) -> Result<(), ScanError> {
    if !visited.insert(canonical_key(dir)) {
        return Ok(());
    }
    if is_report_item(dir) {
        out.push(dir.to_path_buf());
        return Ok(());
    }
    let entries = fs::read_dir(dir).map_err(|error| {
        ScanError::new(format!(
            "cannot search {} for reports: {error}",
            dir.display()
        ))
    })?;
    let mut entries: Vec<fs::DirEntry> =
        entries.collect::<std::io::Result<_>>().map_err(|error| {
            ScanError::new(format!("cannot read entries in {}: {error}", dir.display()))
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        // `file_type` does not follow symlinks, so a linked directory never
        // reports `is_dir` and is skipped here.
        let file_type = entry.file_type().map_err(|error| {
            ScanError::new(format!(
                "cannot inspect {}: {error}",
                entry.path().display()
            ))
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        if is_report_item(&path) {
            if visited.insert(canonical_key(&path)) {
                out.push(path);
            }
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.starts_with('.') || name.ends_with(".semanticmodel") {
            continue;
        }
        if name.ends_with(".report") {
            malformed.push(path);
            continue;
        }
        walk_report_items(&path, visited, out, malformed)?;
    }
    Ok(())
}

/// True when `path` is a folder that makes a report item: one directly
/// holding `report.json` or `definition/report.json`.
fn is_report_item(path: &Path) -> bool {
    path.is_dir()
        && (path.join("report.json").is_file()
            || path.join("definition").join("report.json").is_file())
}

/// Resolves a report's `byPath` against the report item root.
///
/// Power BI writes forward slashes relative to the `.Report` folder;
/// hand-edited files may use backslashes. `.` and `..` are folded textually so
/// the result reads like the written layout instead of carrying the ladder
/// along; callers still canonicalize before comparing directories.
fn resolve_by_path(item_root: &Path, written: &str) -> PathBuf {
    let mut resolved = item_root.to_path_buf();
    for part in written.replace('\\', "/").split('/') {
        match part {
            "" | "." => {}
            ".." => {
                resolved.pop();
            }
            part => resolved.push(part),
        }
    }
    resolved
}

/// Extracts `Initial Catalog` from a `;`-separated connection string.
///
/// Keys compare case-insensitively; the value is trimmed of whitespace and
/// surrounding quotes.
fn initial_catalog(connection_string: &str) -> Option<String> {
    connection_string.split(';').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("initial catalog") {
            return None;
        }
        let catalog = value.trim().trim_matches(|c| c == '"' || c == '\'');
        (!catalog.is_empty()).then(|| catalog.to_string())
    })
}

/// The canonical spelling of `path`, falling back to the path itself when it
/// cannot be canonicalized (a missing path still gets a stable identity).
pub(crate) fn canonical_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The projects and archives directly inside `dir`, sorted by label.
///
/// A bare `.Report` folder whose stem has no model is not a candidate: it
/// cannot be scanned on its own. Discovery never descends into subfolders.
pub fn discover(dir: &Path) -> Vec<Candidate> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let stems = project_stems(dir);
    let mut candidates = project_candidates(dir, &stems);
    for name in &names {
        let path = dir.join(name);
        let lower = name.to_lowercase();
        if (path.is_file()
            && (lower.ends_with(".pbix") || lower.ends_with(".pbit") || lower == "model.bim"))
            && !candidates.iter().any(|candidate| candidate.path == path)
        {
            candidates.push(Candidate {
                label: name.clone(),
                path,
                project_root: None,
                stem: None,
                is_archive: true,
            });
        }
    }
    candidates.sort_by(|a, b| a.label.cmp(&b.label).then(a.path.cmp(&b.path)));
    candidates
}

/// The project stems directly inside `dir`: from `.pbip` files and
/// `.SemanticModel` folders, keyed case-insensitively.
fn project_stems(dir: &Path) -> std::collections::BTreeMap<String, String> {
    let mut stems = std::collections::BTreeMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return stems;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.path().is_dir();
        let stem = if is_dir {
            strip_suffix(&name, ".SemanticModel")
        } else {
            strip_suffix(&name, ".pbip")
        };
        if let Some(stem) = stem {
            stems.entry(stem.to_lowercase()).or_insert(stem);
        }
    }
    stems
}

fn project_candidates(
    dir: &Path,
    stems: &std::collections::BTreeMap<String, String>,
) -> Vec<Candidate> {
    stems
        .values()
        .map(|stem| {
            let pbip = dir.join(format!("{stem}.pbip"));
            let path = if pbip.is_file() {
                pbip
            } else {
                dir.join(format!("{stem}.SemanticModel"))
            };
            Candidate {
                label: stem.clone(),
                path,
                project_root: Some(dir.to_path_buf()),
                stem: Some(stem.clone()),
                is_archive: false,
            }
        })
        .collect()
}

/// Project reports: every `.Report` sibling in a folder dedicated to exactly
/// one project (one model may serve several reports), or the stem-named
/// `.Report` folder in a shared folder.
fn project_reports(root: &Path, stem: &str) -> Vec<PathBuf> {
    let dedicated =
        sole_child_matching(root, |name| name.to_lowercase().ends_with(".semanticmodel")).is_some();
    if dedicated {
        return children_matching(root, |name| name.to_lowercase().ends_with(".report"));
    }
    stem_item(root, stem, "Report")
        .map(|report| vec![report])
        .unwrap_or_default()
}

/// Report siblings of a model: stem-named first, else the sole `.Report`.
fn sibling_reports(model: &Path) -> Vec<PathBuf> {
    let parent = parent_of(model);
    let stem =
        strip_suffix(&file_name(model), ".SemanticModel").unwrap_or_else(|| file_name(model));
    if let Some(report) = stem_item(&parent, &stem, "Report") {
        return vec![report];
    }
    let mut reports = children_matching(&parent, |name| name.to_lowercase().ends_with(".report"));
    if reports.len() > 1 {
        reports.clear();
    }
    reports
}

/// `<stem>.<suffix>` beside `root`, when it exists as a directory.
fn stem_item(root: &Path, stem: &str, suffix: &str) -> Option<PathBuf> {
    let path = root.join(format!("{stem}.{suffix}"));
    path.is_dir().then_some(path)
}

/// The single child matching `keep`, or `None` when none or several match.
fn sole_child_matching(dir: &Path, keep: impl Fn(&str) -> bool) -> Option<PathBuf> {
    let mut matches = children_matching(dir, keep);
    (matches.len() == 1).then(|| matches.remove(0))
}

/// The direct children of `dir` whose names match `keep`, sorted by name.
fn children_matching(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && keep(&file_name(path)))
        .collect();
    paths.sort();
    paths
}

/// Reads a report's model location from its `definition.pbir` by letting core
/// parse it — the CLI never re-implements the PBIR schema. A written `byPath`
/// resolves against the report item root, as Power BI writes it
/// (`Mini.Report/definition.pbir` says `../Mini.SemanticModel`); a live
/// `byConnection` pairs by its `Initial Catalog` dataset name when exactly one
/// sibling model carries that stem or `.platform` display name. The reference
/// is re-read during the scan, so parse notices are left to that ingest.
fn model_from_dataset_reference(report_dir: &Path, parent: &Path) -> Option<PathBuf> {
    let (dataset, _) = ingest::dataset_reference(report_dir);
    match dataset {
        DatasetReference::ByPath { path } => {
            let resolved = resolve_by_path(report_dir, &path);
            resolved.is_dir().then_some(resolved)
        }
        DatasetReference::ByConnection { connection_string } => {
            sole_model_named(parent, &initial_catalog(&connection_string)?)
        }
        DatasetReference::Unresolved => None,
    }
}

/// The one model in `dir` whose stem or `.platform` display name equals
/// `catalog`, or `None` when none or several match — a name is only a
/// pairing when it is unambiguous.
fn sole_model_named(dir: &Path, catalog: &str) -> Option<PathBuf> {
    let mut matches: Vec<PathBuf> =
        children_matching(dir, |name| name.to_lowercase().ends_with(".semanticmodel"))
            .into_iter()
            .filter(|model| model_names_dataset(model, catalog))
            .collect();
    (matches.len() == 1).then(|| matches.remove(0))
}

/// Whether the model item `model` is named `catalog`, by folder stem or
/// `.platform` display name, case-insensitively.
fn model_names_dataset(model: &Path, catalog: &str) -> bool {
    strip_suffix(&file_name(model), ".SemanticModel")
        .is_some_and(|stem| stem.eq_ignore_ascii_case(catalog))
        || ingest::platform_display_name(model)
            .is_some_and(|name| name.eq_ignore_ascii_case(catalog))
}

/// The error for an archive a caller tried to scan: recognized, not ingestable.
pub fn archive_error(path: &Path) -> ScanError {
    ScanError::new(format!(
        "{} is not a supported PBIT or PBIX input",
        path.display()
    ))
    .with_hint("PBIT needs DataModelSchema and a report; PBIX needs a report and a separate model")
}

/// A stem-matched model wins; otherwise every supported sibling model is
/// considered together and only a sole candidate can be inferred.
fn sibling_model(parent: &Path, stem: &str) -> Option<PathBuf> {
    if let Some(item) = stem_item(parent, stem, "SemanticModel") {
        return Some(item);
    }
    for suffix in ["bim", "pbit"] {
        let path = parent.join(format!("{stem}.{suffix}"));
        if path.is_file() && (suffix == "bim" || ingest::archive_has_model(&path)) {
            return Some(path);
        }
    }
    let mut candidates = children_matching(parent, |name| {
        name.to_lowercase().ends_with(".semanticmodel")
    });
    if let Ok(entries) = fs::read_dir(parent) {
        for path in entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
        {
            if !path.is_file() {
                continue;
            }
            let lower = file_name(&path).to_lowercase();
            if lower.ends_with(".bim")
                || lower.ends_with(".pbit") && ingest::archive_has_model(&path)
            {
                candidates.push(path);
            }
        }
    }
    (candidates.len() == 1).then(|| candidates.remove(0))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn parent_of(path: &Path) -> PathBuf {
    path.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Strips a literal (case-insensitive) suffix, returning the stem. ASCII
/// suffixes only, so byte-length arithmetic is safe.
fn strip_suffix(name: &str, suffix: &str) -> Option<String> {
    let suffix = suffix.to_lowercase();
    name.to_lowercase()
        .ends_with(&suffix)
        .then(|| name[..name.len() - suffix.len()].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn paired_of(resolution: Resolution) -> Paired {
        match resolution {
            Resolution::Paired(paired) => paired,
            other => panic!("expected Paired, got {other:?}"),
        }
    }

    #[test]
    fn a_model_pairs_with_its_stem_named_report_sibling() {
        let temp = TempDir::new("model-stem");
        temp.mkdir("Sales.SemanticModel");
        temp.mkdir("Sales.Report");
        temp.mkdir("Other.Report");

        let paired = paired_of(resolve_path(&temp.0.join("Sales.SemanticModel")).expect("pair"));

        assert_eq!(paired.model, temp.0.join("Sales.SemanticModel"));
        assert_eq!(paired.reports, vec![temp.0.join("Sales.Report")]);
    }

    #[test]
    fn a_report_pairs_with_its_model_and_keeps_itself_as_the_report() {
        let temp = TempDir::new("report-stem");
        temp.mkdir("Sales.SemanticModel");
        temp.mkdir("Sales.Report");

        let paired = paired_of(resolve_path(&temp.0.join("Sales.Report")).expect("pair"));

        assert_eq!(paired.model, temp.0.join("Sales.SemanticModel"));
        assert_eq!(paired.reports, vec![temp.0.join("Sales.Report")]);
    }

    #[test]
    fn a_pbip_pairs_with_its_stem_named_items_in_a_flat_folder() {
        let temp = TempDir::new("pbip-flat");
        temp.mkdir("A.SemanticModel");
        temp.mkdir("A.Report");
        temp.write("A.pbip", "{}");
        temp.mkdir("B.SemanticModel");
        temp.mkdir("B.Report");

        let paired = paired_of(resolve_path(&temp.0.join("A.pbip")).expect("pair"));

        assert_eq!(paired.model, temp.0.join("A.SemanticModel"));
        assert_eq!(paired.reports, vec![temp.0.join("A.Report")]);
    }

    #[test]
    fn a_dedicated_project_folder_pairs_every_report_sibling() {
        let temp = TempDir::new("dedicated");
        temp.mkdir("P/P.SemanticModel");
        temp.mkdir("P/P.Report");
        temp.mkdir("P/Extra.Report");
        temp.write("P/P.pbip", "{}");

        let paired = paired_of(resolve_path(&temp.0.join("P/P.pbip")).expect("pair"));

        assert_eq!(paired.model, temp.0.join("P/P.SemanticModel"));
        assert_eq!(
            paired.reports,
            vec![temp.0.join("P/Extra.Report"), temp.0.join("P/P.Report")]
        );
    }

    #[test]
    fn a_folder_with_several_projects_is_ambiguous() {
        let temp = TempDir::new("ambiguous");
        temp.mkdir("A.SemanticModel");
        temp.write("A.pbip", "{}");
        temp.mkdir("B.SemanticModel");
        temp.write("B.pbip", "{}");

        let resolution = resolve_path(&temp.0).expect("resolution");

        let Resolution::Ambiguous { candidates, .. } = resolution else {
            panic!("expected Ambiguous, got {resolution:?}");
        };
        let labels: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.label.clone())
            .collect();
        assert_eq!(labels, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn a_folder_with_one_project_resolves_to_it() {
        let temp = TempDir::new("single-project");
        temp.mkdir("Only.SemanticModel");
        temp.mkdir("Only.Report");

        let paired = paired_of(resolve_path(&temp.0).expect("resolution"));

        assert_eq!(paired.model, temp.0.join("Only.SemanticModel"));
        assert_eq!(paired.reports, vec![temp.0.join("Only.Report")]);
    }

    #[test]
    fn discovery_lists_projects_then_archives_sorted() {
        let temp = TempDir::new("discover");
        temp.mkdir("B.SemanticModel");
        temp.write("B.pbip", "{}");
        temp.write("A.pbix", "PK");
        temp.mkdir("Orphan.Report");

        let candidates = discover(&temp.0);

        let labels: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.label.clone())
            .collect();
        assert_eq!(labels, vec!["A.pbix".to_string(), "B".to_string()]);
        assert!(candidates[0].is_archive);
        assert!(!candidates[1].is_archive);
        assert_eq!(
            candidates[1].path,
            temp.0.join("B.pbip"),
            "the .pbip file represents the project"
        );
    }

    #[test]
    fn a_malformed_archive_is_recognized_but_cannot_pair() {
        let temp = TempDir::new("archive");
        temp.write("Model.pbix", "PK");

        let resolution = resolve_path(&temp.0.join("Model.pbix")).expect("resolution");

        assert_eq!(resolution, Resolution::Archive(temp.0.join("Model.pbix")));
        let candidate = Candidate {
            label: "Model.pbix".to_string(),
            path: temp.0.join("Model.pbix"),
            project_root: None,
            stem: None,
            is_archive: true,
        };
        let error = pair(&candidate).expect_err("malformed archive cannot pair");
        assert!(error.message.contains("not a supported PBIT or PBIX input"));
    }

    #[test]
    fn a_pbip_without_a_model_is_a_pairing_error() {
        let temp = TempDir::new("pbip-lonely");
        temp.write("P.pbip", "{}");

        let error = resolve_path(&temp.0.join("P.pbip")).expect_err("no model");

        assert!(error.message.contains("no semantic model for project 'P'"));
    }

    #[test]
    fn a_report_with_no_sibling_model_resolves_via_by_path() {
        let temp = TempDir::new("by-path");
        // A second model folder rules out the sole-sibling fallback, forcing
        // the `definition.pbir` reference to decide.
        temp.mkdir("Deep/Decoy.SemanticModel");
        temp.mkdir("Deep/M.SemanticModel");
        temp.mkdir("Deep/R.Report/definition");
        temp.write(
            "Deep/R.Report/definition/report.json",
            "{\"$schema\": \"https://example.invalid/report\"}",
        );
        temp.write(
            "Deep/R.Report/definition.pbir",
            "{\"datasetReference\": {\"byPath\": {\"path\": \"../M.SemanticModel\"}}}",
        );

        let paired = paired_of(resolve_path(&temp.0.join("Deep/R.Report")).expect("pair"));

        assert_eq!(paired.model, temp.0.join("Deep/M.SemanticModel"));
        assert_eq!(paired.reports, vec![temp.0.join("Deep/R.Report")]);
    }

    #[test]
    fn a_model_with_no_reports_pairs_empty_and_the_scan_refuses_later() {
        let temp = TempDir::new("model-lonely");
        temp.mkdir("Lone.SemanticModel");

        let paired = paired_of(resolve_path(&temp.0.join("Lone.SemanticModel")).expect("pair"));

        assert!(paired.reports.is_empty());
    }

    mod dataset_name_pairing {
        use super::*;

        /// The `definition.pbir` of a report bound by connection alone.
        fn by_connection_pbir(catalog: &str) -> String {
            format!(
                "{{\"datasetReference\": {{\"byConnection\": {{\"connectionString\": \
                 \"Data Source=powerbi://x;Initial Catalog={catalog}\"}}}}}}"
            )
        }

        #[test]
        fn a_by_connection_report_pairs_with_the_one_sibling_model_its_dataset_names() {
            let temp = TempDir::new("by-name");
            // Two model siblings rule out the sole-sibling fallback: only the
            // dataset name can decide.
            temp.mkdir("M.SemanticModel");
            temp.mkdir("Decoy.SemanticModel");
            temp.mkdir("R.Report");
            temp.write("R.Report/definition.pbir", &by_connection_pbir("M"));

            let paired = paired_of(resolve_path(&temp.0.join("R.Report")).expect("pair"));

            assert_eq!(paired.model, temp.0.join("M.SemanticModel"));
            assert_eq!(paired.reports, vec![temp.0.join("R.Report")]);
        }

        #[test]
        fn a_by_connection_report_matches_a_platform_display_name() {
            let temp = TempDir::new("by-name-display");
            temp.write(
                "N.SemanticModel/.platform",
                "{\"metadata\": {\"displayName\": \"Sales Model\"}}",
            );
            temp.mkdir("Decoy.SemanticModel");
            temp.mkdir("R.Report");
            temp.write(
                "R.Report/definition.pbir",
                &by_connection_pbir("Sales Model"),
            );

            let paired = paired_of(resolve_path(&temp.0.join("R.Report")).expect("pair"));

            assert_eq!(paired.model, temp.0.join("N.SemanticModel"));
        }

        #[test]
        fn a_stem_sibling_wins_over_a_dataset_name() {
            let temp = TempDir::new("by-name-stem-wins");
            temp.mkdir("X.SemanticModel");
            temp.mkdir("M.SemanticModel");
            temp.mkdir("X.Report");
            temp.write("X.Report/definition.pbir", &by_connection_pbir("M"));

            let paired = paired_of(resolve_path(&temp.0.join("X.Report")).expect("pair"));

            assert_eq!(paired.model, temp.0.join("X.SemanticModel"));
        }

        #[test]
        fn a_dataset_name_matching_two_models_is_no_pairing() {
            let temp = TempDir::new("by-name-ambiguous");
            temp.write(
                "A.SemanticModel/.platform",
                "{\"metadata\": {\"displayName\": \"Sales\"}}",
            );
            temp.write(
                "B.SemanticModel/.platform",
                "{\"metadata\": {\"displayName\": \"Sales\"}}",
            );
            temp.mkdir("R.Report");
            temp.write("R.Report/definition.pbir", &by_connection_pbir("Sales"));

            let error = resolve_path(&temp.0.join("R.Report")).expect_err("ambiguous name");

            assert!(
                error
                    .message
                    .contains("cannot locate the semantic model for report"),
                "message: {}",
                error.message
            );
        }

        #[test]
        fn a_dataset_name_no_sibling_carries_is_no_pairing() {
            let temp = TempDir::new("by-name-nomatch");
            temp.mkdir("A.SemanticModel");
            temp.mkdir("B.SemanticModel");
            temp.mkdir("R.Report");
            temp.write("R.Report/definition.pbir", &by_connection_pbir("Z"));

            let error = resolve_path(&temp.0.join("R.Report")).expect_err("no name match");

            assert!(
                error
                    .message
                    .contains("cannot locate the semantic model for report"),
                "message: {}",
                error.message
            );
            assert!(
                error
                    .hint
                    .as_deref()
                    .is_some_and(|hint| hint.contains("--model")),
                "the hint names the escape hatch: {:?}",
                error.hint
            );
        }

        #[test]
        fn a_model_less_pbip_pairs_through_its_report_s_dataset_name() {
            let temp = TempDir::new("by-name-pbip");
            // Two model siblings rule out the sole-sibling fallback, and the
            // project carries no model folder: only the report's dataset name
            // can decide.
            temp.mkdir("M.SemanticModel");
            temp.mkdir("Decoy.SemanticModel");
            temp.mkdir("P.Report");
            temp.write("P.pbip", "{}");
            temp.write("P.Report/definition.pbir", &by_connection_pbir("M"));

            let paired = paired_of(resolve_path(&temp.0.join("P.pbip")).expect("pair"));

            assert_eq!(paired.model, temp.0.join("M.SemanticModel"));
            assert_eq!(paired.reports, vec![temp.0.join("P.Report")]);
        }
    }

    #[test]
    fn an_unrecognized_path_stays_unrecognized() {
        let temp = TempDir::new("unknown");
        temp.mkdir("plain");

        let resolution = resolve_path(&temp.0.join("plain")).expect("resolution");

        assert_eq!(resolution, Resolution::Unrecognized(temp.0.join("plain")));
    }

    mod model_resolution {
        use super::*;

        #[test]
        fn resolve_model_accepts_the_item_definition_and_bare_shapes() {
            let temp = TempDir::new("resolve-model");
            temp.write("X.SemanticModel/definition/model.tmdl", "model Model\n");
            temp.write(
                "X.SemanticModel/.platform",
                "{\"metadata\": {\"displayName\": \"Sales\"}}",
            );
            temp.write("bare/model.tmdl", "model Model\n");

            let item = resolve_model(&temp.0.join("X.SemanticModel")).expect("item shape");
            assert_eq!(item.item_root, temp.0.join("X.SemanticModel"));
            assert_eq!(item.definition, temp.0.join("X.SemanticModel/definition"));
            assert_eq!(item.stem.as_deref(), Some("X"));
            assert_eq!(item.display_name.as_deref(), Some("Sales"));

            let definition = resolve_model(&temp.0.join("X.SemanticModel/definition"))
                .expect("definition shape");
            assert_eq!(definition.item_root, temp.0.join("X.SemanticModel"));
            assert_eq!(
                definition.definition,
                temp.0.join("X.SemanticModel/definition")
            );

            let bare = resolve_model(&temp.0.join("bare")).expect("bare shape");
            assert_eq!(bare.definition, temp.0.join("bare"));
            assert_eq!(bare.stem, None);
            assert_eq!(bare.display_name, None);
        }

        #[test]
        fn resolve_model_rejects_a_folder_that_is_not_a_model() {
            let temp = TempDir::new("resolve-model-garbage");
            temp.mkdir("plain");

            let error = resolve_model(&temp.0.join("plain")).expect_err("not a model");

            assert!(
                error.message.contains("not a semantic model"),
                "message: {}",
                error.message
            );
            assert!(error.hint.is_some(), "the accepted shapes are listed");
        }

        #[test]
        fn resolve_model_accepts_a_pbip_naming_its_project_model() {
            let temp = TempDir::new("resolve-model-pbip");
            temp.write("X.SemanticModel/definition/model.tmdl", "model Model\n");
            temp.write("X.pbip", "{}");

            let item = resolve_model(&temp.0.join("X.pbip")).expect("pbip shape");

            assert_eq!(item.item_root, temp.0.join("X.SemanticModel"));
            assert_eq!(item.definition, temp.0.join("X.SemanticModel/definition"));
            assert_eq!(item.stem.as_deref(), Some("X"));
        }

        #[test]
        fn resolve_model_falls_through_a_pbip_to_the_sole_model_sibling() {
            let temp = TempDir::new("resolve-model-pbip-sole");
            temp.write("P.SemanticModel/definition/model.tmdl", "model Model\n");
            temp.write("Project.pbip", "{}");

            let item = resolve_model(&temp.0.join("Project.pbip")).expect("sole sibling");

            assert_eq!(item.item_root, temp.0.join("P.SemanticModel"));
        }

        #[test]
        fn resolve_model_rejects_a_pbip_without_a_model() {
            let temp = TempDir::new("resolve-model-pbip-lonely");
            temp.write("P.pbip", "{}");

            let error = resolve_model(&temp.0.join("P.pbip")).expect_err("no model");

            assert!(
                error.message.contains("no semantic model for project 'P'"),
                "message: {}",
                error.message
            );
        }

        #[test]
        fn resolve_model_pairs_a_report_only_pbip_through_its_report() {
            let temp = TempDir::new("resolve-model-pbip-thin");
            temp.write(
                "M.SemanticModel/definition/model.tmdl",
                "model Model
",
            );
            temp.mkdir("Decoy.SemanticModel");
            temp.mkdir("P.Report");
            temp.write("P.pbip", "{}");
            temp.write(
                "P.Report/definition.pbir",
                "{\"datasetReference\": {\"byConnection\": {\"connectionString\":                  \"Data Source=powerbi://x;Initial Catalog=M\"}}}",
            );

            let item = resolve_model(&temp.0.join("P.pbip")).expect("thin pbip");

            assert_eq!(item.item_root, temp.0.join("M.SemanticModel"));
            assert_eq!(item.stem.as_deref(), Some("M"));
        }
    }

    mod catalog {
        use super::*;

        #[test]
        fn initial_catalog_reads_keys_case_insensitively_and_strips_quotes() {
            assert_eq!(
                initial_catalog("Data Source=powerbi://x;Initial Catalog=Sales;Other=1"),
                Some("Sales".to_string())
            );
            assert_eq!(
                initial_catalog("initial catalog=\"Sales Model\";x=y"),
                Some("Sales Model".to_string())
            );
            assert_eq!(
                initial_catalog("INITIAL CATALOG='Quoted Name'"),
                Some("Quoted Name".to_string())
            );
            assert_eq!(initial_catalog("integrated security=SSPI"), None);
            assert_eq!(initial_catalog("Initial Catalog=;"), None);
            assert_eq!(initial_catalog(""), None);
        }
    }

    mod by_path_resolution {
        use super::*;

        #[test]
        fn resolve_by_path_accepts_both_separators_and_deeper_ladders() {
            let temp = TempDir::new("by-path-resolve");
            temp.write("M.SemanticModel/definition/model.tmdl", "model Model\n");
            temp.mkdir("reports/A.Report");
            temp.mkdir("reports/sub/B.Report");

            let forward =
                resolve_by_path(&temp.0.join("reports/A.Report"), "../../M.SemanticModel");
            assert!(forward.is_dir(), "forward slashes: {}", forward.display());

            let backslash =
                resolve_by_path(&temp.0.join("reports/A.Report"), "..\\..\\M.SemanticModel");
            assert!(backslash.is_dir(), "backslashes: {}", backslash.display());

            let deeper = resolve_by_path(
                &temp.0.join("reports/sub/B.Report"),
                "../../../M.SemanticModel",
            );
            assert!(deeper.is_dir(), "deeper: {}", deeper.display());

            let dangling =
                resolve_by_path(&temp.0.join("reports/A.Report"), "../../Gone.SemanticModel");
            assert!(!dangling.is_dir(), "dangling must not resolve");
        }
    }

    mod walking {
        use super::*;

        #[test]
        fn unreadable_search_root_fails_instead_of_omitting_reports() {
            let temp = TempDir::new("walk-error");
            temp.write("not-a-directory", "x");
            let error = walk_report_items(
                &temp.0.join("not-a-directory"),
                &mut HashSet::new(),
                &mut Vec::new(),
                &mut Vec::new(),
            )
            .expect_err("search must fail");
            assert!(error.message.contains("cannot search"));
        }

        #[test]
        fn the_walker_prunes_by_convention_and_keeps_nested_items() {
            let temp = TempDir::new("walk-prune");
            temp.write("r/Inner.Report/report.json", "{}");
            temp.write("r/Sub/Deep.Report/definition/report.json", "{}");
            temp.write("r/.hidden/Buried.Report/report.json", "{}");
            temp.write("r/Model.SEMANTICMODEL/Embedded.Report/report.json", "{}");
            temp.write("r/not-a-report.txt", "x");

            let mut visited = HashSet::new();
            let mut out = Vec::new();
            let mut malformed = Vec::new();
            walk_report_items(&temp.0.join("r"), &mut visited, &mut out, &mut malformed)
                .expect("walk succeeds");

            assert_eq!(
                out,
                vec![
                    temp.0.join("r/Inner.Report"),
                    temp.0.join("r/Sub/Deep.Report"),
                ],
                "name order, convention pruned, nested item kept"
            );
            assert!(
                malformed.is_empty(),
                "hidden and .SemanticModel prunes stay silent: {malformed:?}"
            );
        }

        #[test]
        fn the_walker_records_anchorless_report_folders() {
            let temp = TempDir::new("walk-malformed");
            temp.mkdir("r/Empty.REPORT");
            temp.write("r/Empty.REPORT/placeholder.txt", "x");
            temp.write("r/.hidden/Buried.Report/report.json", "{}");
            temp.write("r/Model.SEMANTICMODEL/Embedded.Report/report.json", "{}");

            let mut visited = HashSet::new();
            let mut out = Vec::new();
            let mut malformed = Vec::new();
            walk_report_items(&temp.0.join("r"), &mut visited, &mut out, &mut malformed)
                .expect("walk succeeds");

            assert!(out.is_empty(), "no report item lives under r");
            assert_eq!(
                malformed,
                vec![temp.0.join("r/Empty.REPORT")],
                "only the anchor-less .Report folder is recorded"
            );
        }

        #[test]
        fn the_walker_skips_symlinked_directories() {
            let temp = TempDir::new("walk-symlink");
            temp.mkdir("real");
            temp.write("elsewhere/Deep.Report/report.json", "{}");
            let link = temp.0.join("real/link");
            if symlink_dir(&temp.0.join("elsewhere"), &link).is_err() {
                // Platforms without symlink permission (Windows without
                // developer mode) skip the assertion, not the test run.
                return;
            }

            let mut visited = HashSet::new();
            let mut out = Vec::new();
            let mut malformed = Vec::new();
            walk_report_items(&temp.0.join("real"), &mut visited, &mut out, &mut malformed)
                .expect("walk succeeds");

            assert!(out.is_empty(), "a linked directory is never entered");
        }

        #[cfg(unix)]
        fn symlink_dir(from: &Path, to: &Path) -> std::io::Result<()> {
            std::os::unix::fs::symlink(from, to)
        }

        #[cfg(windows)]
        fn symlink_dir(from: &Path, to: &Path) -> std::io::Result<()> {
            std::os::windows::fs::symlink_dir(from, to)
        }
    }

    mod binding_tiers {
        use super::*;

        fn model_target(root: &Path) -> ModelTarget {
            ModelTarget {
                item_root: root.join("X.SemanticModel"),
                definition: root.join("X.SemanticModel").join("definition"),
                stem: Some("X".to_string()),
                display_name: Some("Sales Model".to_string()),
            }
        }

        /// Writes a report item: the `definition/report.json` anchor plus the
        /// given `definition.pbir` text (`None` leaves the reference out).
        fn report(temp: &TempDir, relative: &str, pbir: Option<&str>) -> PathBuf {
            let root = temp.mkdir(relative);
            temp.write(&format!("{relative}/definition/report.json"), "{}");
            if let Some(text) = pbir {
                temp.write(&format!("{relative}/definition.pbir"), text);
            }
            root
        }

        fn by_path(written: &str) -> String {
            format!("{{\"datasetReference\": {{\"byPath\": {{\"path\": \"{written}\"}}}}}}")
        }

        fn by_connection(connection: &str) -> String {
            format!(
                "{{\"datasetReference\": {{\"byConnection\": {{\"connectionString\": \"{connection}\"}}}}}}"
            )
        }

        /// The model every tier test resolves against; the folders must exist
        /// so canonical comparison sees the same spelling on every platform.
        fn target(temp: &TempDir) -> ModelTarget {
            temp.write("X.SemanticModel/definition/model.tmdl", "model Model\n");
            model_target(&temp.0)
        }

        #[test]
        fn by_path_connects_to_the_item_root_and_the_definition_folder() {
            let temp = TempDir::new("tier-bypath");
            let model = target(&temp);
            let item_root = report(&temp, "r/A.Report", Some(&by_path("../../X.SemanticModel")));
            let definition = report(
                &temp,
                "r/B.Report",
                Some(&by_path("../../X.SemanticModel/definition")),
            );

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &HashSet::new());

            assert_eq!(bound.reports, vec![item_root, definition]);
            assert!(bound.name_matched.is_empty());
            assert!(bound.ignored_elsewhere.is_empty());
            assert!(bound.unresolved.is_empty());
        }

        #[test]
        fn a_by_path_to_another_model_is_bound_elsewhere_without_falling_through() {
            let temp = TempDir::new("tier-elsewhere");
            let model = target(&temp);
            temp.write("Y.SemanticModel/definition/model.tmdl", "model Model\n");
            // The folder stem says X, but the written path names Y: byPath speaks first.
            let elsewhere = report(&temp, "r/X.Report", Some(&by_path("../../Y.SemanticModel")));

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &HashSet::new());

            assert!(bound.reports.is_empty());
            assert_eq!(bound.ignored_elsewhere, vec![elsewhere]);
            assert!(bound.unresolved.is_empty());
        }

        #[test]
        fn a_dangling_by_path_falls_through_to_the_stem() {
            let temp = TempDir::new("tier-dangling");
            let model = target(&temp);
            let stem = report(
                &temp,
                "r/X.Report",
                Some(&by_path("../../Gone.SemanticModel")),
            );

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &HashSet::new());

            assert_eq!(bound.reports, vec![stem]);
            assert!(bound.unresolved.is_empty());
        }

        #[test]
        fn the_stem_tier_compares_without_case() {
            let temp = TempDir::new("tier-stem-case");
            let model = target(&temp);
            let stem = report(&temp, "r/x.REPORT", None);

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &HashSet::new());

            assert_eq!(bound.reports, vec![stem]);
        }

        #[test]
        fn by_connection_matches_the_platform_display_name() {
            let temp = TempDir::new("tier-connection");
            let model = target(&temp);
            let thin = report(
                &temp,
                "r/Thin.Report",
                Some(&by_connection(
                    "Data Source=powerbi://x;Initial Catalog=Sales Model",
                )),
            );

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &HashSet::new());

            assert_eq!(bound.reports, vec![thin.clone()]);
            assert_eq!(bound.name_matched, vec![(thin, "Sales Model".to_string())]);
        }

        #[test]
        fn by_connection_to_another_dataset_is_bound_elsewhere() {
            let temp = TempDir::new("tier-connection-other");
            let model = target(&temp);
            let elsewhere = report(
                &temp,
                "r/Thin.Report",
                Some(&by_connection(
                    "Initial Catalog=Other;Data Source=powerbi://x",
                )),
            );

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &HashSet::new());

            assert!(bound.reports.is_empty());
            assert_eq!(bound.ignored_elsewhere, vec![elsewhere]);
            assert!(bound.name_matched.is_empty());
        }

        #[test]
        fn by_connection_without_a_catalog_is_unresolved() {
            let temp = TempDir::new("tier-connection-nocatalog");
            let model = target(&temp);
            let item = report(
                &temp,
                "r/Thin.Report",
                Some(&by_connection("Data Source=powerbi://x")),
            );

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &HashSet::new());

            assert!(bound.reports.is_empty());
            assert_eq!(bound.unresolved.len(), 1);
            assert_eq!(bound.unresolved[0].0, item);
            assert!(bound.unresolved[0].1.contains("initial catalog"));
        }

        #[test]
        fn a_malformed_definition_pbir_is_unresolved_and_reported() {
            let temp = TempDir::new("tier-malformed");
            let model = target(&temp);
            let item = report(&temp, "r/Thin.Report", Some("{\"datasetReference\": {}}"));

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &HashSet::new());

            assert_eq!(bound.unresolved.len(), 1);
            assert_eq!(bound.unresolved[0].0, item);
            assert_eq!(
                bound.parse_skips.len(),
                1,
                "core's malformed-value notice travels"
            );
            assert_eq!(
                bound.parse_skips[0].kind,
                ripbi_core::SkipKind::MalformedValue
            );
        }

        #[test]
        fn a_missing_definition_pbir_is_unresolved_without_a_notice() {
            let temp = TempDir::new("tier-no-pbir");
            let model = target(&temp);
            let item = report(&temp, "r/Thin.Report", None);

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &HashSet::new());

            assert_eq!(bound.unresolved.len(), 1);
            assert_eq!(bound.unresolved[0].0, item);
            assert!(bound.parse_skips.is_empty());
        }

        #[test]
        fn excluded_report_items_are_never_walked_again() {
            let temp = TempDir::new("tier-exclude");
            let model = target(&temp);
            let explicit = report(
                &temp,
                "r/Explicit.Report",
                Some(&by_path("../../Gone.SemanticModel")),
            );
            let mut exclude = HashSet::new();
            exclude.insert(canonical_key(&explicit));

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &exclude);

            assert!(bound.reports.is_empty());
            assert!(bound.unresolved.is_empty(), "explicit is explicit");
            assert!(bound.ignored_elsewhere.is_empty());
        }

        #[test]
        fn connected_reports_come_out_in_canonical_order() {
            let temp = TempDir::new("tier-sorted");
            let model = target(&temp);
            let zebra = report(
                &temp,
                "r/Zebra.Report",
                Some(&by_path("../../X.SemanticModel")),
            );
            let alpha = report(
                &temp,
                "r/Alpha.Report",
                Some(&by_path("../../X.SemanticModel")),
            );

            let bound = discover_bound_reports(&model, &[temp.0.join("r")], &HashSet::new());

            assert_eq!(bound.reports, vec![alpha, zebra]);
        }

        #[test]
        fn overlapping_search_roots_do_not_duplicate_reports() {
            let temp = TempDir::new("tier-overlap");
            let model = target(&temp);
            let item = report(
                &temp,
                "r/sub/A.Report",
                Some(&by_path("../../../X.SemanticModel")),
            );

            let bound = discover_bound_reports(
                &model,
                &[temp.0.join("r"), temp.0.join("r/sub")],
                &HashSet::new(),
            );

            assert_eq!(bound.reports, vec![item]);
        }
    }
}
