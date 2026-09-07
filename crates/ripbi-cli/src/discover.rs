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
//! - `.pbix`/`.pbit`/`model.bim` are recognized but not yet ingestable.

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_core::DatasetReference;

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
/// the scan.
fn model_from_dataset_reference(report_dir: &Path) -> Option<PathBuf> {
    let ingested = ripbi_core::ingest::report(report_dir).ok()?;
    match ingested.value.dataset {
        DatasetReference::ByPath { path } => {
            let resolved =
                parent_of(report_dir).join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
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
            "{\"datasetReference\": {\"byPath\": {\"path\": \"M.SemanticModel\"}}}",
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
}
