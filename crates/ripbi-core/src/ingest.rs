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

mod abf;
mod archive;
mod legacy;
mod pbir;
mod tmdl;
mod tmsl;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::model::TabularDatabase;
use crate::report::{DatasetReference, ReportModel};
use crate::{Error, Result};

/// Power BI writes some JSON archive members as UTF-16 even without a BOM.
fn decode_json_text(bytes: &[u8], label: &str) -> Result<String> {
    let (bytes, endian) = if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        (rest, Some(false))
    } else if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        (rest, Some(true))
    } else if bytes.starts_with(b"{\0") || bytes.starts_with(b"[\0") {
        (bytes, Some(false))
    } else if bytes.starts_with(b"\0{") || bytes.starts_with(b"\0[") {
        (bytes, Some(true))
    } else {
        (
            bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes),
            None,
        )
    };
    if let Some(big_endian) = endian {
        let (chunks, remainder) = bytes.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(Error::UnsupportedFormat(format!(
                "{label} has an odd UTF-16 byte count"
            )));
        }
        let units = chunks
            .iter()
            .map(|pair| {
                if big_endian {
                    u16::from_be_bytes([pair[0], pair[1]])
                } else {
                    u16::from_le_bytes([pair[0], pair[1]])
                }
            })
            .collect::<Vec<_>>();
        String::from_utf16(&units)
            .map_err(|_| Error::UnsupportedFormat(format!("{label} is not valid UTF-16")))
    } else {
        String::from_utf8(bytes.to_vec())
            .map_err(|_| Error::UnsupportedFormat(format!("{label} is not valid UTF-8")))
    }
}

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
    /// Saved or indexed state that refers to objects which no longer exist —
    /// e.g. a bookmark section whose page was deleted from the report.
    StaleState,
    /// A source expression contains a native query whose SQL is not analyzed.
    OpaqueSource,
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

/// Parses a TMDL folder, TMSL JSON file, ABF backup, or archive model into a
/// [`TabularDatabase`].
///
/// `path` may be a `.SemanticModel` folder (its `definition/` subfolder is
/// located automatically), a `definition/` folder, a `model.bim` file, an
/// `.abf` backup, or a PBIX/PBIT ZIP carrying `DataModelSchema` or a
/// compressed `DataModel`. When an archive carries both, `DataModelSchema`
/// wins. Only the model's metadata is read from a backup, never its data.
/// Unexpected parse drift is reported in [`Ingested::skips`].
///
/// Table order follows `model.tmdl`'s `ref table` directives; tables present as
/// files but never referenced are appended in file-name order. The `cultures/`
/// folder is deliberately not read.
pub fn semantic_model(path: &Path) -> Result<Ingested<TabularDatabase>> {
    if path.is_file() {
        let mut signature = [0; 4];
        let count = fs::File::open(path)?.read(&mut signature)?;
        if count == 4 && signature == *b"PK\x03\x04" {
            return archive::model(path);
        }
        let mut header = [0; abf::SIGNATURE_LEN];
        let count = read_prefix(&mut fs::File::open(path)?, &mut header)?;
        if abf::is_abf(&header[..count]) {
            let mut skips = Vec::new();
            let value = abf::load(fs::File::open(path)?, path, &mut skips)?;
            archive::ensure_m_coverage(&value, path)?;
            return Ok(Ingested { value, skips });
        }
        let bytes = fs::read(path)?;
        let leading = bytes
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace());
        let looks_like_json = leading == Some(b'{')
            || bytes.starts_with(&[0xef, 0xbb, 0xbf])
            || bytes.starts_with(&[0xff, 0xfe])
            || bytes.starts_with(&[0xfe, 0xff])
            || bytes.starts_with(b"\0{");
        if looks_like_json
            || path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("bim"))
        {
            let mut skips = Vec::new();
            let value = tmsl::load(&bytes, path, &mut skips)?;
            archive::ensure_m_coverage(&value, path)?;
            return Ok(Ingested { value, skips });
        }
        return Err(Error::UnsupportedFormat(format!(
            "{} is neither TMSL JSON, an ABF backup, nor a PBIX/PBIT archive",
            path.display()
        )));
    }
    let definition = locate_definition(path)?;
    // `.platform` (which carries the display name) sits beside `definition/`.
    let item_root = definition.parent().unwrap_or(path);
    let name = platform_display_name(item_root);
    let mut skips = Vec::new();
    let value = tmdl::load_database(&definition, name, &mut skips)?;
    Ok(Ingested { value, skips })
}

/// Parses a PBIR folder or an archive report into a [`ReportModel`].
///
/// `path` may be a `.Report` folder (its `definition/` subfolder is located
/// automatically), a `definition/` folder, or a PBIX/PBIT ZIP with
/// `Report/Layout` or `Report/definition/report.json`. A report is parsed
/// standalone: the semantic model it connects to need not sit beside it, so
/// one model can be scanned against several reports. When the report ships a
/// phone layout — a `definition.mobile/` folder beside `definition/` — its
/// pages are parsed into [`ReportModel::mobile_pages`] and bind the model
/// exactly like desktop pages (issue #49). Unexpected drift is reported in
/// [`Ingested::skips`]; an unreadable or malformed `report.json`, or an
/// unreadable report directory, fails. A missing or anchor-less
/// phone layout is the common case and is silent.
pub fn report(path: &Path) -> Result<Ingested<ReportModel>> {
    if path.is_file() {
        return archive::report(path);
    }
    let definition = locate_report_definition(path)?;
    // `.platform` (which carries the display name) sits beside `definition/`.
    let item_root = definition.parent().unwrap_or(path);
    let name = platform_display_name(item_root);
    let mut skips = Vec::new();
    let mut value = pbir::load_report(&definition, name, &mut skips)?;
    if let Some(mobile) = locate_mobile_definition(&definition) {
        value.mobile_pages = pbir::load_mobile_pages(&mobile, &mut skips)?;
    }
    Ok(Ingested { value, skips })
}

/// Whether a ZIP archive embeds a semantic model: a TMSL `DataModelSchema`
/// or an ABF `DataModel` member.
#[must_use]
pub fn archive_has_model(path: &Path) -> bool {
    archive::has_model(path)
}

/// Whether `path` is a file that opens with an ABF backup signature
/// (uncompressed, or single- or multithreaded XPress9).
#[must_use]
pub fn is_abf(path: &Path) -> bool {
    let mut header = [0; abf::SIGNATURE_LEN];
    fs::File::open(path)
        .and_then(|mut file| read_prefix(&mut file, &mut header))
        .is_ok_and(|count| abf::is_abf(&header[..count]))
}

/// Reads up to `buf.len()` leading bytes, returning how many were read.
fn read_prefix(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..])? {
            0 => break,
            count => filled += count,
        }
    }
    Ok(filled)
}

/// Whether a ZIP archive contains either supported report representation.
#[must_use]
pub fn archive_has_report(path: &Path) -> bool {
    archive::has_member(path, "Report/Layout")
        || archive::has_member(path, "Report/definition/report.json")
}

/// Reads a report item's `definition.pbir` dataset reference without parsing
/// the rest of the report.
///
/// `item_root` is the `.Report` folder (the file sits beside `definition/`).
/// Intended for callers that must pair many report items cheaply — a folder
/// walk can read thousands of `definition.pbir` files while only the connected
/// reports are ingested in full. Unexpected drift is returned as
/// [`SkipNotice`]s, exactly as [`report`] would record it.
#[must_use]
pub fn dataset_reference(item_root: &Path) -> (DatasetReference, Vec<SkipNotice>) {
    let mut skips = Vec::new();
    let reference = pbir::dataset_reference(item_root, &mut skips);
    (reference, skips)
}

/// Resolves the `definition/` folder of a semantic-model item.
///
/// Accepts the `.SemanticModel` folder itself, its `definition/` subfolder, or
/// any directory that directly contains a `model.tmdl`.
pub fn locate_definition(path: &Path) -> Result<PathBuf> {
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

/// Resolves the `definition.mobile/` phone layout of a report item, if it
/// ships one.
///
/// The layout is optional and anchor-less — unlike `definition/`, it carries
/// no `report.json` — so presence is decided by its `pages/` folder, and any
/// absence is silent, never drift.
fn locate_mobile_definition(definition: &Path) -> Option<PathBuf> {
    let mobile = definition.parent()?.join("definition.mobile");
    if mobile.join("pages").is_dir() {
        Some(mobile)
    } else {
        None
    }
}

/// Reads the item's display name from `.platform`, best-effort.
///
/// TMDL itself records no usable model name (`model.tmdl` names its root
/// object `Model`), so the Fabric item metadata is the only source. Any
/// absence or drift yields `None` — a name is provenance, never liveness.
#[must_use]
pub fn platform_display_name(item_root: &Path) -> Option<String> {
    let text = fs::read_to_string(item_root.join(".platform")).ok()?;
    let platform: serde_json::Value = serde_json::from_str(&text).ok()?;
    platform
        .get("metadata")?
        .get("displayName")?
        .as_str()
        .map(str::to_string)
}
