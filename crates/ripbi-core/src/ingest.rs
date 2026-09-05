//! Format ingestion: turning Power BI source folders into the crate's ASTs.
//!
//! Entry points return [`Ingested`] — the parsed value plus every
//! [`SkipNotice`] the parser recorded. A notice is a warning carried as data,
//! not control flow: this crate never prints, so the CLI decides how notices
//! are presented. They are collected on every run, not only in debug builds,
//! because a silently skipped object can surface later as a false "unused"
//! finding — the failure mode this tool exists to prevent.
//!
//! Skips come in two tiers. *Deliberately unmodeled* metadata (annotations,
//! lineage tags, display folders, cultures, …) is skipped silently, per the
//! exclusions in `docs/semantic-model.md`. *Unexpected drift* — an unknown
//! object, a property that is neither modeled nor ignored, a value that fails
//! to parse — is recorded as a notice. The full policy, including the curated
//! ignore list, lives in `docs/formats.md`.

mod pbir;
mod tmdl;

use std::fs;
use std::path::{Path, PathBuf};

use crate::model::TabularDatabase;
use crate::report::ReportModel;
use crate::{Error, Result};

/// One thing a parser skipped, and why.
///
/// Notices are warnings as data: this crate records them and moves on, never
/// printing and never failing, so a single unexpected property cannot abort an
/// analysis run. Presentation is the CLI's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipNotice {
    /// The file the skip was found in, as ingest saw it.
    pub path: PathBuf,
    /// Where in the file the skip lives — a TMDL line number (`line 12`) or a
    /// JSON pointer — when the parser can name one.
    pub location: Option<String>,
    /// What kind of skip this is.
    pub kind: SkipKind,
    /// What was skipped and why, in one sentence.
    pub detail: String,
}

/// Why a parser skipped something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipKind {
    /// An object the AST does not model and the ignore list does not cover.
    UnknownObject,
    /// A property the AST does not model and the ignore list does not cover.
    UnknownProperty,
    /// A modeled property whose value could not be parsed.
    MalformedValue,
    /// A query alias that could not be resolved (PBIR `SourceRef.Source`).
    UnresolvedAlias,
}

/// A parsed value plus everything unexpected the parser skipped on the way.
///
/// A named wrapper rather than a tuple, so it can grow fields additively —
/// a parse count, for instance — without breaking every call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested<T> {
    /// The parsed value.
    pub value: T,
    /// Everything the parser skipped, in file order.
    pub skips: Vec<SkipNotice>,
}

/// Parses a TMDL semantic model into a [`TabularDatabase`].
///
/// `path` is a `.SemanticModel` folder (its `definition/` subfolder is located
/// automatically) or a `definition/` folder itself. Unknown-but-harmless TMDL
/// drift is reported in [`Ingested::skips`]; only a file that cannot be parsed
/// into a tree at all fails with [`Error::Tmdl`].
///
/// Table order follows `model.tmdl`'s `ref table` directives; tables present as
/// files but never referenced are appended in file-name order. The `cultures/`
/// folder is deliberately not read.
pub fn semantic_model(path: &Path) -> Result<Ingested<TabularDatabase>> {
    let definition = locate_definition(path)?;
    // `.platform` (which carries the display name) sits beside `definition/`.
    let item_root = definition.parent().unwrap_or(path);
    let name = platform_display_name(item_root);
    let mut skips = Vec::new();
    let value = tmdl::load_database(&definition, name, &mut skips)?;
    Ok(Ingested { value, skips })
}

/// Parses a PBIR report folder into a [`ReportModel`].
///
/// `path` is a `.Report` folder (its `definition/` subfolder is located
/// automatically) or a `definition/` folder itself. A report is parsed
/// standalone: the semantic model it connects to need not sit beside it, so
/// one model can be scanned against several reports. Unexpected drift is
/// reported in [`Ingested::skips`]; only an unreadable or malformed
/// `report.json` — the file that makes the folder a report — fails.
pub fn report(path: &Path) -> Result<Ingested<ReportModel>> {
    let definition = locate_report_definition(path)?;
    // `.platform` (which carries the display name) sits beside `definition/`.
    let item_root = definition.parent().unwrap_or(path);
    let name = platform_display_name(item_root);
    let mut skips = Vec::new();
    let value = pbir::load_report(&definition, name, &mut skips)?;
    Ok(Ingested { value, skips })
}

/// Resolves the `definition/` folder of a semantic-model item.
///
/// Accepts the `.SemanticModel` folder itself, its `definition/` subfolder, or
/// any directory that directly contains a `model.tmdl`.
fn locate_definition(path: &Path) -> Result<PathBuf> {
    if !path.is_dir() {
        return Err(Error::UnsupportedFormat(format!(
            "not a semantic model: {} is not a directory",
            path.display()
        )));
    }
    let nested = path.join("definition");
    if nested.is_dir() {
        return Ok(nested);
    }
    let looks_like_definition = path.join("model.tmdl").is_file()
        || path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("definition"));
    if looks_like_definition {
        return Ok(path.to_path_buf());
    }
    Err(Error::UnsupportedFormat(format!(
        "not a semantic model: no definition/ or model.tmdl under {}",
        path.display()
    )))
}

/// Resolves the `definition/` folder of a PBIR report item.
///
/// Accepts the `.Report` folder itself, its `definition/` subfolder, or any
/// directory that directly contains a `report.json`.
fn locate_report_definition(path: &Path) -> Result<PathBuf> {
    if !path.is_dir() {
        return Err(Error::UnsupportedFormat(format!(
            "not a report: {} is not a directory",
            path.display()
        )));
    }
    let nested = path.join("definition");
    if nested.join("report.json").is_file() {
        return Ok(nested);
    }
    if path.join("report.json").is_file() {
        return Ok(path.to_path_buf());
    }
    Err(Error::UnsupportedFormat(format!(
        "not a report: no definition/report.json or report.json under {}",
        path.display()
    )))
}

/// Reads the item's display name from `.platform`, best-effort.
///
/// TMDL itself records no usable model name (`model.tmdl` names its root
/// object `Model`), so the Fabric item metadata is the only source. Any
/// absence or drift yields `None` — a name is provenance, never liveness.
fn platform_display_name(item_root: &Path) -> Option<String> {
    let text = fs::read_to_string(item_root.join(".platform")).ok()?;
    let platform: serde_json::Value = serde_json::from_str(&text).ok()?;
    platform
        .get("metadata")?
        .get("displayName")?
        .as_str()
        .map(str::to_string)
}
