//! `.ripbi/source.json`: the manifest `ripbi fetch` writes beside a fetched
//! project — where the files came from and which downstream reports could not
//! be downloaded. `scan` reads it to flag an incomplete report set.
//! Schema documented in `crates/ripbi-cli/docs/fetch.md`.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The manifest, relative to the project root.
pub const MANIFEST_PATH: &str = ".ripbi/source.json";
/// The manifest's schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Where a report ended up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    /// On disk.
    Downloaded,
    /// The caller is a Viewer of its workspace.
    Viewer,
    /// The caller is not a member of its workspace, or the export said `404`.
    NoAccess,
    /// The export failed for another reason.
    #[default]
    Failed,
}

/// `.ripbi/source.json`: where the files came from and what is missing.
/// Schema documented in `crates/ripbi-cli/docs/fetch.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    /// The schema version, `1`.
    pub schema_version: u32,
    /// `ripbi <version>`.
    pub generator: String,
    /// RFC 3339 UTC.
    pub fetched_at: String,
    /// The token's tenant.
    pub tenant_id: Option<String>,
    /// The model.
    pub model: ManifestModel,
    /// True when every downstream report was downloaded.
    pub complete: bool,
    /// Every workspace report bound to the model, downloaded or not.
    pub reports: Vec<ManifestReport>,
    /// Org-app copies, skipped.
    pub app_copies: Vec<ManifestItem>,
    /// Other downstream items, not scanned.
    pub other_items: Vec<ManifestItem>,
    /// The downstream edges.
    pub relations: Vec<ManifestRelation>,
    /// The `--verify` result, when it ran.
    pub verification: Option<ManifestVerification>,
    /// Discovery notes.
    pub notes: Vec<String>,
}

/// The model in the manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestModel {
    /// The model GUID — the `semanticmodelid` every fetched `definition.pbir` names.
    pub id: String,
    /// The model name.
    pub name: String,
    /// The workspace GUID.
    pub workspace_id: String,
    /// The workspace name.
    pub workspace_name: String,
    /// The model folder, relative to the root, `/`-separated.
    pub path: String,
}

/// One report in the manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestReport {
    /// The report GUID.
    pub id: String,
    /// The report name.
    pub name: String,
    /// The workspace GUID.
    pub workspace_id: String,
    /// The workspace name.
    pub workspace_name: String,
    /// Where it ended up.
    pub status: ReportStatus,
    /// The export's HTTP status, when it failed with one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// Why it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The report folder, relative to the root, when downloaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// A downstream item outside the scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestItem {
    /// The item GUID.
    pub id: String,
    /// The item name.
    pub name: String,
    /// The item type as the API spells it.
    #[serde(rename = "type")]
    pub kind: String,
    /// The workspace GUID.
    pub workspace_id: String,
    /// The workspace name.
    pub workspace_name: String,
}

/// A downstream edge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestRelation {
    /// The dependent item.
    pub item_id: String,
    /// The item it depends on.
    pub depends_on: String,
    /// The relation type.
    #[serde(rename = "type")]
    pub kind: String,
}

/// The `--verify` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestVerification {
    /// Workspaces whose listing was read.
    pub workspaces_checked: usize,
    /// Bound reports the downstream answer missed.
    pub only_in_listing: Vec<ManifestItem>,
    /// Downstream reports the listings lack.
    pub only_in_downstream: Vec<ManifestItem>,
    /// Workspaces whose listing failed.
    pub failures: Vec<String>,
}

impl Manifest {
    /// Reads `<root>/.ripbi/source.json`; `None` when absent or unreadable.
    #[must_use]
    pub fn load(root: &Path) -> Option<Self> {
        let text = fs::read_to_string(root.join(MANIFEST_PATH)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Reports a scan of this folder cannot see.
    #[must_use]
    pub fn unreadable_count(&self) -> usize {
        self.reports
            .iter()
            .filter(|report| report.status != ReportStatus::Downloaded)
            .count()
    }
}
