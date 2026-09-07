//! Rendering: everything a scan prints to stdout, in the three output modes
//! from `docs/cli-ux-guidelines.md` — human text, `--plain`, and `--json`.
//! The full contract (shapes, examples, schema) lives in `docs/output.md`
//! beside this crate.

use std::io;

use ripbi_core::ObjectId;
use serde::Serialize;

use crate::style::Palette;

/// Everything a scan wants to say on stdout, as presentation-ready data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOutput {
    /// The scanned model, as given.
    pub target: String,
    /// The reports scanned against, as given.
    pub reports: Vec<String>,
    /// Every object in the dependency graph.
    pub objects: usize,
    /// Objects reachability reached.
    pub reachable: usize,
    /// Report binding roots (RLS roles also seed reachability but are not roots here).
    pub roots: usize,
    /// Unused objects before any `[scan].ignore` filtering.
    pub unused_raw: usize,
    /// Objects suppressed by `[scan].ignore` patterns.
    pub ignored: usize,
    /// The unused objects that survive ignore filtering, sorted by identity.
    pub findings: Vec<Finding>,
    /// Every skip notice ingestion recorded.
    pub skips: Vec<SkipNoticeOut>,
}

/// One unused finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The object kind — a group key and the `--plain`/JSON `type` field.
    pub kind: &'static str,
    /// The human-readable id, e.g. `'Sales'[Draft Amount]`.
    pub id: String,
    /// Every object still referencing this one.
    pub used_by: Vec<UsedByOut>,
}

/// One referencing object behind a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedByOut {
    /// The referencing object's display id.
    pub id: String,
    /// How the reference is made, as a phrase.
    pub provenance: String,
    /// True when the referencing object is itself unused.
    pub also_unused: bool,
}

/// One parser skip notice, shaped for output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipNoticeOut {
    /// The file the skip was found in.
    pub path: String,
    /// Line number or JSON pointer, when known.
    pub location: Option<String>,
    /// The skip kind, snake_case.
    pub kind: &'static str,
    /// What was skipped and why.
    pub detail: String,
}

/// Group order and labels: the fixed section order of human output.
const GROUPS: &[(&str, &str)] = &[
    ("measure", "Measures"),
    ("column", "Columns"),
    ("hierarchy", "Hierarchies"),
    ("table", "Tables"),
    ("partition", "Partitions"),
    ("relationship", "Relationships"),
    ("calculation_item", "Calculation items"),
    ("expression", "Expressions"),
    ("function", "Functions"),
    ("report_measure", "Report measures"),
];

/// The object kind of an id, as used in `--plain`, JSON, and grouping.
#[must_use]
pub fn kind_of(id: &ObjectId) -> &'static str {
    match id {
        ObjectId::Table { .. } => "table",
        ObjectId::Column { .. } => "column",
        ObjectId::Measure { .. } => "measure",
        ObjectId::Hierarchy { .. } => "hierarchy",
        ObjectId::Partition { .. } => "partition",
        ObjectId::Relationship { .. } => "relationship",
        ObjectId::Role { .. } => "role",
        ObjectId::CalculationItem { .. } => "calculation_item",
        ObjectId::Expression { .. } => "expression",
        ObjectId::Function { .. } => "function",
        ObjectId::ReportMeasure { .. } => "report_measure",
    }
}

/// Writes the human-readable report: a summary line, then findings grouped by
/// object type with `←` chain annotations.
///
/// # Errors
/// Propagates stream write failures.
pub fn human(out: &mut dyn io::Write, palette: &Palette, report: &ScanOutput) -> io::Result<()> {
    let unused_text = if report.findings.is_empty() {
        palette.green("0 unused")
    } else {
        palette.yellow(&format!("{} unused", report.findings.len()))
    };
    writeln!(
        out,
        "{} objects, {} reachable from {} roots, {unused_text}",
        palette.bold(&report.objects.to_string()),
        palette.bold(&report.reachable.to_string()),
        palette.bold(&report.roots.to_string()),
    )?;
    if report.ignored > 0 {
        writeln!(
            out,
            "({} objects suppressed by [scan].ignore)",
            report.ignored
        )?;
    }
    writeln!(out)?;

    if report.findings.is_empty() {
        writeln!(out, "No unused objects.")?;
        return Ok(());
    }
    for (kind, label) in GROUPS {
        let group: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|finding| finding.kind == *kind)
            .collect();
        if group.is_empty() {
            continue;
        }
        writeln!(
            out,
            "{}",
            palette.bold(&format!("{label} ({})", group.len()))
        )?;
        for finding in group {
            writeln!(out, "  {}", finding.id)?;
            write_annotations(out, palette, finding)?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn write_annotations(
    out: &mut dyn io::Write,
    palette: &Palette,
    finding: &Finding,
) -> io::Result<()> {
    if finding.used_by.is_empty() {
        writeln!(out, "{}", palette.dim("    ← nothing references it"))?;
        return Ok(());
    }
    let only = finding.used_by.len() == 1;
    for used in &finding.used_by {
        let prefix = if only { "only " } else { "" };
        let also = if used.also_unused {
            " (also unused)"
        } else {
            ""
        };
        writeln!(
            out,
            "{}",
            palette.dim(&format!(
                "    ← {prefix}used by {} — {}{also}",
                used.id, used.provenance
            ))
        )?;
    }
    Ok(())
}

/// Writes `--plain` output: one `<type>\t<id>` record per finding.
///
/// # Errors
/// Propagates stream write failures.
pub fn plain(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    for finding in &report.findings {
        writeln!(out, "{}\t{}", finding.kind, finding.id)?;
    }
    Ok(())
}

/// Writes `--json` output: the v1 schema from `docs/output.md`.
///
/// # Errors
/// Propagates stream write failures.
pub fn json(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    let payload = JsonReport {
        schema_version: 1,
        target: report.target.clone(),
        reports: report.reports.clone(),
        summary: JsonSummary {
            objects: report.objects,
            reachable: report.reachable,
            roots: report.roots,
            unused: report.findings.len(),
            ignored: report.ignored,
        },
        unused: report
            .findings
            .iter()
            .map(|finding| JsonFinding {
                kind: finding.kind,
                id: finding.id.clone(),
                used_by: finding
                    .used_by
                    .iter()
                    .map(|used| JsonUsedBy {
                        id: used.id.clone(),
                        provenance: used.provenance.clone(),
                        also_unused: used.also_unused,
                    })
                    .collect(),
            })
            .collect(),
        skips: JsonSkips {
            count: report.skips.len(),
            notices: report
                .skips
                .iter()
                .map(|skip| JsonSkip {
                    path: skip.path.clone(),
                    location: skip.location.clone(),
                    kind: skip.kind,
                    detail: skip.detail.clone(),
                })
                .collect(),
        },
    };
    serde_json::to_writer_pretty(&mut *out, &payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writeln!(out)
}

#[derive(Serialize)]
struct JsonReport {
    schema_version: u8,
    target: String,
    reports: Vec<String>,
    summary: JsonSummary,
    unused: Vec<JsonFinding>,
    skips: JsonSkips,
}

#[derive(Serialize)]
struct JsonSummary {
    objects: usize,
    reachable: usize,
    roots: usize,
    unused: usize,
    ignored: usize,
}

#[derive(Serialize)]
struct JsonFinding {
    #[serde(rename = "type")]
    kind: &'static str,
    id: String,
    used_by: Vec<JsonUsedBy>,
}

#[derive(Serialize)]
struct JsonUsedBy {
    id: String,
    provenance: String,
    also_unused: bool,
}

#[derive(Serialize)]
struct JsonSkips {
    count: usize,
    notices: Vec<JsonSkip>,
}

#[derive(Serialize)]
struct JsonSkip {
    path: String,
    location: Option<String>,
    kind: &'static str,
    detail: String,
}
