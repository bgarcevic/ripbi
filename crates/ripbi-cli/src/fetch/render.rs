//! `fetch`'s stderr: the pre-download forecast and the closing summary.
//!
//! ```text
//! Sales · 34 downstream reports (+34 app copies skipped)
//!   ✓ 17 downloadable   Marketing (9), HR (7), Finance (1)
//!   ✗ 17 not readable   Operations (17, viewer)
//!   · 2 other downstream items not scanned: 2 Dashboard
//! Scan will be incomplete: 17 reports can't be read.
//! ```

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::time::Duration;

use ripbi_service::downstream::{Access, Downstream, Verification};

use ripbi_app::fetch::{ReportStatus, Written};

use crate::style::Palette;

/// The forecast block.
///
/// # Errors
///
/// On a write failure.
pub fn forecast(
    err: &mut dyn Write,
    palette: &Palette,
    found: &Downstream,
    verification: Option<&Verification>,
) -> io::Result<()> {
    let copies = match found.app_copies.len() {
        0 => String::new(),
        1 => " (+1 app copy skipped)".to_string(),
        n => format!(" (+{n} app copies skipped)"),
    };
    writeln!(
        err,
        "{} {}",
        palette.bold(&found.model.name),
        palette.dim(&format!(
            "· {} · {}{copies}",
            found.model.workspace_name,
            count(
                found.reports.len(),
                "downstream report",
                "downstream reports"
            )
        ))
    )?;

    let readable: Vec<&str> = found
        .reports
        .iter()
        .filter(|report| report.access == Access::Downloadable)
        .map(|report| report.workspace_name.as_str())
        .collect();
    if !readable.is_empty() {
        writeln!(
            err,
            "  {} {:<16} {}",
            palette.ok("✓"),
            format!("{} downloadable", readable.len()),
            per_workspace(readable.iter().map(|name| (*name, ""))),
        )?;
    }
    let unreadable: Vec<(&str, &str)> = found
        .unreadable()
        .map(|report| {
            let why = match report.access {
                Access::Viewer => "viewer",
                _ => "no access",
            };
            (report.workspace_name.as_str(), why)
        })
        .collect();
    if !unreadable.is_empty() {
        writeln!(
            err,
            "  {} {:<16} {}",
            palette.alert("✗"),
            format!("{} not readable", unreadable.len()),
            per_workspace(unreadable.iter().copied()),
        )?;
    }
    if !found.other_items.is_empty() {
        let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
        for item in &found.other_items {
            *kinds.entry(item.kind.as_str()).or_default() += 1;
        }
        let kinds: Vec<String> = kinds
            .iter()
            .map(|(kind, n)| format!("{n} {kind}"))
            .collect();
        writeln!(
            err,
            "{}",
            palette.info_note(&format!(
                "{} not scanned: {}",
                count(
                    found.other_items.len(),
                    "other downstream item",
                    "other downstream items"
                ),
                kinds.join(", ")
            ))
        )?;
    }
    for note in &found.notes {
        writeln!(err, "{}", palette.warn_note(note))?;
    }
    if let Some(verification) = verification {
        verification_lines(err, palette, verification)?;
    }
    if !unreadable.is_empty() {
        writeln!(
            err,
            "{}",
            palette.warn(&format!(
                "Scan will be incomplete: {} can't be read.",
                count(unreadable.len(), "report", "reports")
            ))
        )?;
    }
    Ok(())
}

fn verification_lines(
    err: &mut dyn Write,
    palette: &Palette,
    verification: &Verification,
) -> io::Result<()> {
    let checked = count(verification.workspaces_checked, "workspace", "workspaces");
    if verification.only_in_listing.is_empty() && verification.only_in_downstream.is_empty() {
        writeln!(
            err,
            "{}",
            palette.info_note(&format!(
                "--verify: {checked} agree with the downstream list"
            ))
        )?;
    }
    for item in &verification.only_in_listing {
        writeln!(
            err,
            "{}",
            palette.warn_note(&format!(
                "--verify: '{}' in '{}' is bound to the model but missing downstream; it is not fetched",
                item.name, item.workspace_name
            ))
        )?;
    }
    for item in &verification.only_in_downstream {
        writeln!(
            err,
            "{}",
            palette.warn_note(&format!(
                "--verify: '{}' in '{}' is downstream but missing from its workspace listing",
                item.name, item.workspace_name
            ))
        )?;
    }
    if !verification.failures.is_empty() {
        writeln!(
            err,
            "{}",
            palette.info_note(&format!(
                "--verify: {} could not be listed",
                count(verification.failures.len(), "workspace", "workspaces")
            ))
        )?;
    }
    Ok(())
}

/// `Marketing (9), HR (7)` — busiest first; a reason rides along when given.
fn per_workspace<'a>(entries: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for entry in entries {
        *counts.entry(entry).or_default() += 1;
    }
    let mut counts: Vec<((&str, &str), usize)> = counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
        .iter()
        .map(|((workspace, why), n)| {
            let workspace = if workspace.is_empty() {
                "(unnamed workspace)"
            } else {
                workspace
            };
            if why.is_empty() {
                format!("{workspace} ({n})")
            } else {
                format!("{workspace} ({n}, {why})")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The closing lines after the files are written.
///
/// # Errors
///
/// On a write failure.
pub fn summary(
    err: &mut dyn Write,
    palette: &Palette,
    written: &Written,
    shown_root: &str,
    elapsed: Duration,
    refused: bool,
) -> io::Result<()> {
    let manifest = &written.manifest;
    let downloaded = manifest
        .reports
        .iter()
        .filter(|report| report.status == ReportStatus::Downloaded)
        .count();
    writeln!(
        err,
        "{} Fetched {} and {} into {shown_root} {}",
        palette.ok("✓"),
        palette.bold(&manifest.model.name),
        count(downloaded, "report", "reports"),
        palette.muted(&format!("{:.1}s", elapsed.as_secs_f64()))
    )?;
    // Reports the forecast called readable that still failed.
    for report in manifest
        .reports
        .iter()
        .filter(|report| report.http_status.is_some() || report.status == ReportStatus::Failed)
    {
        let why = report
            .error
            .clone()
            .unwrap_or_else(|| "not readable (HTTP 404: item-level permissions)".to_string());
        writeln!(
            err,
            "{}",
            palette.warn_note(&format!(
                "'{}' in '{}': {why}",
                report.name, report.workspace_name
            ))
        )?;
    }
    let missing = manifest.unreadable_count();
    if missing > 0 {
        writeln!(
            err,
            "{}",
            palette.warn_note(&format!(
                "{} missing — objects only they use will show as unused; listed in {}",
                count(missing, "report is", "reports are"),
                super::MANIFEST_PATH
            ))
        )?;
    }
    if refused {
        writeln!(
            err,
            "{} more reports failed than the forecast predicted; pass --allow-incomplete to accept a partial set",
            palette.alert("error:")
        )?;
    } else {
        writeln!(err, "Next: cd {}, then ripbi scan", quote_arg(shown_root))?;
    }
    Ok(())
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn quote_arg(text: &str) -> String {
    if text.contains(' ') {
        format!("\"{text}\"")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_workspace_orders_by_count_then_name() {
        let entries = [
            ("HR", ""),
            ("Marketing", ""),
            ("Marketing", ""),
            ("Finance", ""),
        ];
        assert_eq!(
            per_workspace(entries.into_iter()),
            "Marketing (2), Finance (1), HR (1)"
        );
        assert_eq!(
            per_workspace([("Operations", "viewer"), ("Operations", "viewer")].into_iter()),
            "Operations (2, viewer)"
        );
    }
}
