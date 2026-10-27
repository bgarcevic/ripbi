//! Where findings live on disk: the model declaration (TMDL file and line) or
//! report file behind each finding, keyed by its comparison fingerprint. The
//! SARIF and Azure DevOps renderers point annotations at these sites; a
//! desktop UI can open them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ripbi_core::{NameKey, ObjectId, SourceLocation};

use crate::compare::{Entry, Key};
use crate::output::{AutoDateTimeRow, BrokenArtifactOut, BrokenOut, Finding};
use crate::scan::report_name;

/// Findings that already existed in the `--compare-root` checkout: dropped by
/// every other mode, kept by SARIF as suppressed results.
#[derive(Debug, Default)]
pub struct Existing {
    pub findings: Vec<Finding>,
    pub broken: Vec<BrokenOut>,
    pub broken_artifacts: Vec<BrokenArtifactOut>,
    pub auto_date_time: Vec<AutoDateTimeRow>,
}

/// Where a finding points: a file, relative to the scan's working directory
/// when it lies beneath it, and the declaration line when the source records
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub path: PathBuf,
    pub line: Option<usize>,
}

/// Resolves findings to source sites, keyed by fingerprint so the renderer can
/// look them up from the presentation data alone.
#[derive(Debug)]
pub struct Locator {
    model: PathBuf,
    declared: HashMap<ObjectId, SourceLocation>,
    reports: Vec<PathBuf>,
    sites: HashMap<Key, Site>,
}

impl Locator {
    /// `declared` is `ripbi_core::ingest::source_locations` for the model —
    /// empty for single-file formats, which point at the file itself.
    #[must_use]
    pub fn new(model: &Path, declared: Vec<SourceLocation>, reports: &[PathBuf]) -> Self {
        Self {
            model: model.to_path_buf(),
            declared: declared
                .into_iter()
                .map(|location| (location.id.clone(), location))
                .collect(),
            reports: reports.to_vec(),
            sites: HashMap::new(),
        }
    }

    /// Records where the model object or report item `id` of `entry` lives;
    /// `report_index` names the report of a report-level object when known.
    pub fn object(&mut self, entry: &Entry, id: &ObjectId, report_index: Option<usize>) {
        let site = match id {
            ObjectId::Bookmark { report_index, .. } => self.report_site(Some(*report_index)),
            ObjectId::ReportMeasure { .. } => self.report_site(report_index),
            _ => self.model_site(id),
        };
        self.sites.insert(entry.key(), site);
    }

    /// Records where a broken binding lives: its report, found by name.
    pub fn binding(&mut self, entry: &Entry, report: Option<&NameKey>) {
        let index = report.and_then(|name| {
            self.reports
                .iter()
                .position(|path| NameKey::new(report_name(path)) == *name)
        });
        let site = self.report_site(index);
        self.sites.insert(entry.key(), site);
    }

    fn model_site(&self, id: &ObjectId) -> Site {
        match self.declared.get(id) {
            Some(location) => Site {
                path: location.path.clone(),
                line: Some(location.line),
            },
            None => self.model_file(),
        }
    }

    /// The model as a file: the model file itself, or a TMDL folder's
    /// `model.tmdl`, so a result with no recorded declaration still lands on
    /// a file.
    fn model_file(&self) -> Site {
        let model_tmdl = [
            self.model.join("definition").join("model.tmdl"),
            self.model.join("model.tmdl"),
        ]
        .into_iter()
        .find(|path| path.is_file());
        Site {
            path: model_tmdl.unwrap_or_else(|| self.model.clone()),
            line: None,
        }
    }

    /// A report as a file: the archive itself, or a PBIR folder's
    /// `report.json`. An unknown report falls back to the first one.
    fn report_site(&self, index: Option<usize>) -> Site {
        let Some(report) = self.reports.get(index.unwrap_or(0)) else {
            return self.model_file();
        };
        let path = [
            report.join("definition").join("report.json"),
            report.join("report.json"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| report.clone());
        Site { path, line: None }
    }

    #[must_use]
    pub fn site(&self, entry: &Entry) -> Option<&Site> {
        self.sites.get(&entry.key())
    }
}
