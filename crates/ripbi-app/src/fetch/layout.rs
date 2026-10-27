//! The fetched project on disk:
//!
//! ```text
//! <out>/
//!   ripbi.toml                          # target + explicit report list
//!   .ripbi/source.json                  # provenance + completeness
//!   <Model>.SemanticModel/              # TMDL, definition.pbism, .platform
//!   reports/<Workspace>/<Report>.Report/
//! ```
//!
//! Part paths come from the tenant and are untrusted: each is checked to stay
//! inside its item folder before anything is written. A re-fetch of the same
//! model replaces the folders an earlier fetch wrote; anything else in the
//! directory is left alone.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ripbi_service::api::Definition;
use ripbi_service::downstream::{Access, Downstream, ItemRef, ModelRef, Verification};

use super::Downloads;
use crate::error::ScanError;
pub use crate::manifest::{
    MANIFEST_PATH, Manifest, ManifestItem, ManifestModel, ManifestRelation, ManifestReport,
    ManifestVerification, ReportStatus, SCHEMA_VERSION,
};

/// The folder fetched reports live under.
const REPORTS_DIR: &str = "reports";
/// The first line of a `ripbi.toml` that `fetch` may rewrite.
const CONFIG_MARKER: &str = "# Written by `ripbi fetch`";
/// Report parts that a scan never reads: images, themes, custom visuals.
const SKIPPED_REPORT_PREFIX: &str = "StaticResources/";

/// Where the project goes: `--out`, else `./<model name>`.
#[must_use]
pub fn output_dir(cwd: &Path, out: Option<&Path>, model: &ModelRef) -> PathBuf {
    out.map_or_else(|| cwd.join(file_name(&model.name)), |out| cwd.join(out))
}

/// A checked output directory.
pub struct Plan {
    root: PathBuf,
    previous: Option<Manifest>,
}

/// Refuses a directory that holds anything but an earlier fetch of the same
/// model, unless `force`.
///
/// # Errors
///
/// When `root` is a file, holds another model's fetch, or is a non-empty
/// directory fetch did not write (without `force`).
pub fn prepare(root: &Path, model: &ModelRef, force: bool) -> Result<Plan, ScanError> {
    if root.is_file() {
        return Err(ScanError::new(format!(
            "'{}' is a file, not a directory",
            root.display()
        )));
    }
    let previous = Manifest::load(root);
    let empty = fs::read_dir(root).map_or(true, |mut entries| entries.next().is_none());
    match &previous {
        Some(manifest) if manifest.model.id.eq_ignore_ascii_case(&model.id) => {}
        Some(manifest) if !force => {
            return Err(ScanError::new(format!(
                "'{}' holds a fetch of another model ('{}')",
                root.display(),
                manifest.model.name
            ))
            .with_hint("pick another directory with -o, or pass --force to replace it"));
        }
        None if !empty && !force => {
            return Err(ScanError::new(format!(
                "'{}' is not empty and was not written by ripbi fetch",
                root.display()
            ))
            .with_hint("pick another directory with -o, or pass --force to write into it"));
        }
        _ => {}
    }
    Ok(Plan {
        root: root.to_path_buf(),
        previous,
    })
}

/// What [`write`] put on disk.
#[derive(Debug, Clone)]
pub struct Written {
    /// The manifest as written, paths and statuses final.
    pub manifest: Manifest,
    /// Whether `ripbi.toml` was (re)written.
    pub config_written: bool,
}

/// Builds the manifest from the forecast and the download results; paths
/// are filled in by [`write`].
#[must_use]
pub fn manifest(
    found: &Downstream,
    tenant_id: Option<String>,
    downloads: &Downloads,
    verification: Option<&Verification>,
) -> Manifest {
    let reports = found
        .reports
        .iter()
        .map(|report| {
            let mut entry = ManifestReport {
                id: report.id.clone(),
                name: report.name.clone(),
                workspace_id: report.workspace_id.clone(),
                workspace_name: report.workspace_name.clone(),
                ..ManifestReport::default()
            };
            entry.status = match report.access {
                Access::Viewer => ReportStatus::Viewer,
                Access::NoAccess => ReportStatus::NoAccess,
                Access::Downloadable => match downloads.reports.get(&report.id) {
                    Some(Ok(_)) => ReportStatus::Downloaded,
                    // Item-level permissions exist: a forecast-readable
                    // report can still answer 404.
                    Some(Err(error)) if error.is_not_found() => {
                        entry.http_status = Some(404);
                        ReportStatus::NoAccess
                    }
                    Some(Err(error)) => {
                        entry.http_status = error.status();
                        entry.error = Some(error.to_string());
                        ReportStatus::Failed
                    }
                    None => ReportStatus::Failed,
                },
            };
            entry
        })
        .collect::<Vec<_>>();
    Manifest {
        schema_version: SCHEMA_VERSION,
        generator: concat!("ripbi ", env!("CARGO_PKG_VERSION")).to_string(),
        fetched_at: rfc3339(SystemTime::now()),
        tenant_id,
        model: ManifestModel {
            id: found.model.id.clone(),
            name: found.model.name.clone(),
            workspace_id: found.model.workspace_id.clone(),
            workspace_name: found.model.workspace_name.clone(),
            path: String::new(),
        },
        complete: reports
            .iter()
            .all(|report| report.status == ReportStatus::Downloaded),
        reports,
        app_copies: found.app_copies.iter().map(manifest_item).collect(),
        other_items: found.other_items.iter().map(manifest_item).collect(),
        relations: found
            .relations
            .iter()
            .map(|relation| ManifestRelation {
                item_id: relation.item_id.clone(),
                depends_on: relation.dependent_on_item_id.clone(),
                kind: relation.relation_type.clone(),
            })
            .collect(),
        verification: verification.map(|verification| ManifestVerification {
            workspaces_checked: verification.workspaces_checked,
            only_in_listing: verification
                .only_in_listing
                .iter()
                .map(manifest_item)
                .collect(),
            only_in_downstream: verification
                .only_in_downstream
                .iter()
                .map(manifest_item)
                .collect(),
            failures: verification.failures.clone(),
        }),
        notes: found.notes.clone(),
    }
}

fn manifest_item(item: &ItemRef) -> ManifestItem {
    ManifestItem {
        id: item.id.clone(),
        name: item.name.clone(),
        kind: item.kind.clone(),
        workspace_id: item.workspace_id.clone(),
        workspace_name: item.workspace_name.clone(),
    }
}

/// Writes the model, the downloaded reports, `ripbi.toml`, and the manifest.
///
/// # Errors
///
/// On I/O failure, or when the model's definition names an unsafe path. A
/// report with an unsafe path is recorded as failed instead.
pub fn write(
    plan: &Plan,
    found: &Downstream,
    downloads: &Downloads,
    mut manifest: Manifest,
) -> Result<Written, ScanError> {
    let root = &plan.root;
    fs::create_dir_all(root)?;
    let model_dir = format!("{}.SemanticModel", file_name(&found.model.name));

    // Replace what an earlier fetch (or `--force`) owns: its model folder and
    // the reports tree. Nothing else in the directory is touched.
    let mut stale: Vec<String> = vec![model_dir.clone(), REPORTS_DIR.to_string()];
    if let Some(previous) = &plan.previous {
        stale.push(previous.model.path.clone());
    }
    for relative in stale {
        if let Some(path) = safe_join(root, &relative).filter(|path| path != root)
            && path.is_dir()
        {
            fs::remove_dir_all(&path)?;
        }
    }

    write_parts(&root.join(&model_dir), &downloads.model, None).map_err(|error| {
        ScanError::new(format!(
            "cannot write the model '{}': {}",
            found.model.name, error.message
        ))
    })?;
    manifest.model.path = model_dir.clone();

    let mut taken = HashSet::new();
    let mut report_paths = Vec::new();
    for entry in &mut manifest.reports {
        let Some(Ok(definition)) = downloads.reports.get(&entry.id) else {
            continue;
        };
        let relative = unique_report_dir(entry, &mut taken);
        let dir = root.join(&relative);
        match write_parts(&dir, definition, Some(SKIPPED_REPORT_PREFIX)) {
            Ok(()) => {
                entry.path = Some(relative.clone());
                report_paths.push(relative);
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&dir);
                entry.status = ReportStatus::Failed;
                entry.error = Some(error.message);
            }
        }
    }
    manifest.complete = manifest.unreadable_count() == 0;

    let config_written = write_config(root, &model_dir, &report_paths)?;
    let manifest_path = root.join(MANIFEST_PATH);
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|error| ScanError::new(format!("cannot serialize {MANIFEST_PATH}: {error}")))?;
    fs::write(&manifest_path, format!("{json}\n"))?;
    Ok(Written {
        manifest,
        config_written,
    })
}

/// `reports/<Workspace>/<Report>.Report`, suffixed with the id's first
/// block when two reports share a workspace and a name.
fn unique_report_dir(entry: &ManifestReport, taken: &mut HashSet<String>) -> String {
    let base = format!(
        "{REPORTS_DIR}/{}/{}",
        file_name(&entry.workspace_name),
        file_name(&entry.name)
    );
    let mut relative = format!("{base}.Report");
    if !taken.insert(relative.to_lowercase()) {
        let short = entry.id.split('-').next().unwrap_or(&entry.id);
        relative = format!("{base} ({short}).Report");
        taken.insert(relative.to_lowercase());
    }
    relative
}

/// Writes every part under `dir`, skipping parts under `skip`.
fn write_parts(dir: &Path, definition: &Definition, skip: Option<&str>) -> Result<(), ScanError> {
    for part in &definition.parts {
        if skip.is_some_and(|prefix| part.path.starts_with(prefix)) {
            continue;
        }
        let target = safe_join(dir, &part.path).ok_or_else(|| {
            ScanError::new(format!(
                "the definition names an unsafe path '{}'",
                part.path
            ))
        })?;
        let bytes = part
            .decode()
            .map_err(|error| ScanError::new(error.to_string()))?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, bytes)?;
    }
    Ok(())
}

/// `base` joined with a `/`-separated relative path, or `None` when the path
/// is absolute, empty, or could leave `base` (`..`, drive prefixes,
/// backslashes, `:` alternate streams).
fn safe_join(base: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    let mut any = false;
    for segment in relative.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".."
            || segment.contains(['\\', ':', '\0'])
            || !matches!(
                Path::new(segment).components().next(),
                Some(Component::Normal(_))
            )
        {
            return None;
        }
        path.push(segment);
        any = true;
    }
    any.then_some(path)
}

/// Writes `ripbi.toml` unless one exists that fetch did not write.
fn write_config(root: &Path, model_dir: &str, reports: &[String]) -> Result<bool, ScanError> {
    let path = root.join("ripbi.toml");
    if let Ok(existing) = fs::read_to_string(&path)
        && !existing.starts_with(CONFIG_MARKER)
    {
        return Ok(false);
    }
    let quote = |text: &str| serde_json::to_string(text).unwrap_or_else(|_| format!("\"{text}\""));
    let mut text = format!(
        "{CONFIG_MARKER}; re-running it rewrites this file.\n\
         # Provenance and the reports that could not be read: {MANIFEST_PATH}\n\
         target = {}\n\
         reports = [\n",
        quote(model_dir)
    );
    for report in reports {
        text.push_str(&format!("    {},\n", quote(report)));
    }
    text.push_str("]\n");
    fs::write(&path, text)?;
    Ok(true)
}

/// A name safe as one path segment on every OS: reserved characters become
/// `_`, trailing dots and spaces go, Windows device names get a `_` suffix.
#[must_use]
pub fn file_name(name: &str) -> String {
    let mut cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    while cleaned.ends_with(['.', ' ']) {
        cleaned.pop();
    }
    let cleaned = cleaned.trim_start().to_string();
    if cleaned.is_empty() {
        return "_".to_string();
    }
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    let mut cleaned = if reserved {
        format!("{cleaned}_")
    } else {
        cleaned
    };
    if cleaned.chars().count() > 120 {
        cleaned = cleaned
            .chars()
            .take(120)
            .collect::<String>()
            .trim_end()
            .to_string();
    }
    cleaned
}

/// RFC 3339 UTC, second precision.
fn rfc3339(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn file_names_survive_every_os() {
        assert_eq!(file_name("Sales / Margin: Q1?"), "Sales _ Margin_ Q1_");
        assert_eq!(file_name("Report. "), "Report");
        assert_eq!(file_name("con"), "con_");
        assert_eq!(file_name("COM1.txt"), "COM1.txt_");
        assert_eq!(file_name("Console"), "Console");
        assert_eq!(file_name("  "), "_");
        assert_eq!(file_name("Æblegrød"), "Æblegrød");
    }

    #[test]
    fn unsafe_part_paths_are_refused() {
        let base = Path::new("out");
        assert_eq!(
            safe_join(base, "definition/tables/Yes%2FNo.tmdl"),
            Some(base.join("definition").join("tables").join("Yes%2FNo.tmdl"))
        );
        for bad in [
            "../evil",
            "a/../../b",
            "C:/x",
            "a\\..\\b",
            "/",
            "",
            "x:stream",
        ] {
            assert_eq!(safe_join(base, bad), None, "{bad}");
        }
        // A leading slash is tolerated as a separator, never as a root.
        assert_eq!(
            safe_join(base, "/definition.pbir"),
            Some(base.join("definition.pbir"))
        );
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(1_792_491_045)),
            "2026-10-20T10:10:45Z"
        );
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
    }

    #[test]
    fn duplicate_report_names_get_the_id_prefix() {
        let entry = |id: &str| ManifestReport {
            id: id.to_string(),
            name: "Prices".to_string(),
            workspace_name: "Marketing".to_string(),
            ..ManifestReport::default()
        };
        let mut taken = HashSet::new();
        assert_eq!(
            unique_report_dir(&entry("aaaa-1"), &mut taken),
            "reports/Marketing/Prices.Report"
        );
        assert_eq!(
            unique_report_dir(&entry("bbbb-2"), &mut taken),
            "reports/Marketing/Prices (bbbb).Report"
        );
    }
}
