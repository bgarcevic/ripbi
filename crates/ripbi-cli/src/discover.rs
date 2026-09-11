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
//! - A `.pbip` file pairs with its stem-named items in the same folder.
//! - A `.SemanticModel` folder pairs with stem-named `.Report` siblings, or
//!   — in a folder dedicated to one project — with all `.Report` siblings.
//! - A `.Report` folder pairs with its stem-named model, then the sole model
//!   sibling, then the model its `definition.pbir` points at.
//! - A `--model` target pairs with every report item found under its search
//!   folders whose `definition.pbir` resolves to it, whose folder stem names
//!   it (`X.Report` beside `X.SemanticModel`), or whose `byConnection` names
//!   its dataset.
//! - `.pbix`/`.pbit`/`model.bim` are recognized but not yet ingestable.

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
    /// Every report folder to ingest as reachability roots, in discovery order.
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
    /// True for archives (`.pbix` and friends): recognized, not ingestable.
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
    /// Recognized but not yet ingestable: a `.pbix`, `.pbit`, or `model.bim`.
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
    if lower == "model.bim" || lower.ends_with(".pbix") || lower.ends_with(".pbit") {
        return Ok(Resolution::Archive(path.to_path_buf()));
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
        0 => Ok(Resolution::Unrecognized(path.to_path_buf())),
        1 => {
            let stem = stems.values().next().expect("exactly one stem").clone();
            pair_project(path, &stem).map(Resolution::Paired)
        }
        _ => Ok(Resolution::Ambiguous {
            dir: path.to_path_buf(),
            candidates: project_candidates(path, &stems),
        }),
    }
}

/// Pairs a candidate from folder discovery. Archives never pair.
pub fn pair(candidate: &Candidate) -> Result<Paired, ScanError> {
    let (Some(root), Some(stem)) = (&candidate.project_root, &candidate.stem) else {
        return Err(archive_error(&candidate.path));
    };
    pair_project(root, stem)
}

/// Pairs a project stem with its model and report items in `root`.
pub fn pair_project(root: &Path, stem: &str) -> Result<Paired, ScanError> {
    let model = stem_item(root, stem, "SemanticModel").or_else(|| {
        sole_child_matching(root, |name| name.to_lowercase().ends_with(".semanticmodel"))
    });
    let Some(model) = model else {
        return Err(ScanError::new(format!(
            "no semantic model for project '{stem}' in {}",
            root.display()
        ))
        .with_hint("a .pbip project pairs with a '<stem>.SemanticModel' folder beside it"));
    };
    let reports = project_reports(root, stem);
    Ok(Paired { model, reports })
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
/// sibling, then the `definition.pbir` `byPath` reference.
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
        .or_else(|| model_from_dataset_reference(report_dir));
    let Some(model) = model else {
        return Err(ScanError::new(format!(
            "cannot locate the semantic model for report {}",
            report_dir.display()
        ))
        .with_hint("pass the model folder as PATH and the report with --report"));
    };
    Ok(Paired {
        model,
        reports: vec![report_dir.to_path_buf()],
    })
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
}

/// Resolves a `--model` PATH into the model item it names.
///
/// The three accepted shapes are core's: a `.SemanticModel` folder, its
/// `definition/` subfolder, or any folder directly containing `model.tmdl`.
///
/// # Errors
/// When `path` is none of those. The message starts with core's
/// "not a semantic model" and the hint lists the accepted shapes.
pub fn resolve_model(path: &Path) -> Result<ModelTarget, ScanError> {
    let definition = ingest::locate_definition(path).map_err(|error| {
        ScanError::new(error.to_string()).with_hint(
            "pass a .SemanticModel folder, its definition/ folder, or any folder containing model.tmdl",
        )
    })?;
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
/// unreadable directories are skipped silently, mirroring [`discover`].
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
        walk_report_items(root, &mut visited, &mut items, &mut malformed);
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
/// skipped outright, so a traversal cycle can never trap the walk. Unreadable
/// directories are skipped silently.
fn walk_report_items(
    dir: &Path,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
    malformed: &mut Vec<PathBuf>,
) {
    if !visited.insert(canonical_key(dir)) {
        return;
    }
    if is_report_item(dir) {
        out.push(dir.to_path_buf());
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<fs::DirEntry> = entries.filter_map(|entry| entry.ok()).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        // `file_type` does not follow symlinks, so a linked directory never
        // reports `is_dir` and is skipped here.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
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
        walk_report_items(&path, visited, out, malformed);
    }
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
fn canonical_key(path: &Path) -> PathBuf {
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

/// Reads a report's model location from its `datasetReference.byPath` by
/// letting core parse the report — the CLI never re-implements the PBIR
/// schema. The parsed value is discarded; the report is ingested again for
/// the scan. The path resolves against the report item root, as Power BI
/// writes it (`Mini.Report/definition.pbir` says `../Mini.SemanticModel`).
fn model_from_dataset_reference(report_dir: &Path) -> Option<PathBuf> {
    let ingested = ripbi_core::ingest::report(report_dir).ok()?;
    match ingested.value.dataset {
        DatasetReference::ByPath { path } => {
            let resolved = resolve_by_path(report_dir, &path);
            resolved.is_dir().then_some(resolved)
        }
        _ => None,
    }
}

/// The error for an archive a caller tried to scan: recognized, not ingestable.
pub fn archive_error(path: &Path) -> ScanError {
    ScanError::new(format!(
        "{} is not supported yet: only TMDL semantic models and PBIR reports can be scanned",
        path.display()
    ))
    .with_hint("track .pbix/.pbit support in the ripbi issue tracker")
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
    fn an_archive_is_recognized_but_never_pairs() {
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
        let error = pair(&candidate).expect_err("archives do not pair");
        assert!(error.message.contains("not supported yet"));
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
            walk_report_items(&temp.0.join("r"), &mut visited, &mut out, &mut malformed);

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
            walk_report_items(&temp.0.join("r"), &mut visited, &mut out, &mut malformed);

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
            walk_report_items(&temp.0.join("real"), &mut visited, &mut out, &mut malformed);

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
