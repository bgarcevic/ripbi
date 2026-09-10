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
    /// Unused objects hidden by the type flags (`--measures` and friends).
    /// `[scan].ignore` suppressions are counted in [`ScanOutput::ignored`]
    /// instead.
    pub filtered_out: usize,
    /// The unused objects that survive ignore filtering, sorted by identity.
    /// A dead auto date/time table's own row lives in [`ScanOutput::auto_date_time`]
    /// instead, under its verdict.
    pub findings: Vec<Finding>,
    /// One row per auto date/time table (`LocalDateTable_*` /
    /// `DateTableTemplate_*`): the provenance verdict no reachability pass can
    /// produce. Rows suppressed by `[scan].ignore` are absent, and so is the
    /// whole section when a type filter runs without `--tables`.
    pub auto_date_time: Vec<AutoDateTimeRow>,
    /// Every skip notice ingestion recorded.
    pub skips: Vec<SkipNoticeOut>,
}

/// One auto date/time table with its verdict — does a report bind the
/// machinery, or is it alive only through the engine's own relationship?
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoDateTimeRow {
    /// The verdict: `in_use`, `unused_by_reports`, or `dead`.
    pub verdict: &'static str,
    /// The table's display id, e.g. `table 'LocalDateTable_9e0bbdfc-…'`.
    pub id: String,
    /// The user's date column the machinery serves, when one resolves — the
    /// `for 'Date'[OrderDate]` context.
    pub source_column: Option<String>,
    /// The table's own unused finding, present exactly when the verdict is
    /// `dead`: the row moved here from the generic findings so the verdict
    /// and its dead-chain note read together.
    pub finding: Option<Finding>,
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
    /// The Power Query expressions that name this column — the supply chain
    /// that is deliberately not liveness. Unloading the column cannot break
    /// these; removing it from the script entirely means editing each.
    pub named_in_power_query: Vec<String>,
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

/// Group order and labels: the fixed section order of human output. The
/// `scan` type flags (`--measures` and friends) select exactly these kinds —
/// `cli.rs`'s lockstep test pins the two together.
pub(crate) const GROUPS: &[(&str, &str)] = &[
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

/// The verdict label of an auto date/time table, as used in `--plain` and JSON.
#[must_use]
pub fn verdict_of(verdict: ripbi_core::graph::AutoDateTimeStatus) -> &'static str {
    use ripbi_core::graph::AutoDateTimeStatus;
    match verdict {
        AutoDateTimeStatus::InUse => "in_use",
        AutoDateTimeStatus::UnusedByReports => "unused_by_reports",
        AutoDateTimeStatus::Dead => "dead",
    }
}

/// The Power Query expressions that name a finding's column, as display
/// labels — the partition is named by its table (that is what the user
/// edits), not by its GUID. Order-preserving and deduplicated.
#[must_use]
pub fn power_query_labels(ids: &[ObjectId]) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    for id in ids {
        let label = match id {
            ObjectId::Partition { table, .. } => format!("'{}' partition", table.as_str()),
            ObjectId::Expression { name } => format!("'{}' expression", name.as_str()),
            other => other.to_string(),
        };
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

/// Writes the human-readable report: a summary line, then findings grouped by
/// object type with `←` chain annotations. `show_power_query` adds the
/// `⭘ Power Query also names it` annotations (hidden by default; `--json`
/// always carries the underlying field).
///
/// # Errors
/// Propagates stream write failures.
pub fn human(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
    show_power_query: bool,
) -> io::Result<()> {
    write_summary(out, palette, report)?;

    if report.findings.is_empty() {
        writeln!(out, "No unused objects.")?;
    } else {
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
                write_annotations(out, palette, finding, "    ", show_power_query)?;
            }
            writeln!(out)?;
        }
    }
    write_auto_date_time(out, palette, report, show_power_query)
}

/// The auto date/time section, rendered in both human modes after the
/// reachability findings: a different verdict (report bindings, not
/// reachability), so it survives even a clean "No unused objects." scan.
fn write_auto_date_time(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
    show_power_query: bool,
) -> io::Result<()> {
    if report.auto_date_time.is_empty() {
        return Ok(());
    }
    const SECTIONS: &[(&str, &str)] = &[
        ("in_use", "in use — replace with a real date table"),
        (
            "unused_by_reports",
            "unused by reports — disable auto date/time",
        ),
        ("dead", "dead"),
    ];
    let actionable = report
        .auto_date_time
        .iter()
        .any(|row| row.verdict != "in_use");
    let header = format!("Auto date/time ({})", report.auto_date_time.len());
    writeln!(
        out,
        "{}",
        if actionable {
            palette.yellow(&header)
        } else {
            palette.bold(&header)
        }
    )?;
    for (verdict, label) in SECTIONS {
        let group: Vec<&AutoDateTimeRow> = report
            .auto_date_time
            .iter()
            .filter(|row| row.verdict == *verdict)
            .collect();
        if group.is_empty() {
            continue;
        }
        writeln!(out, "  {label}:")?;
        for row in group {
            let source = row
                .source_column
                .as_deref()
                .map(|column| format!(" — for {column}"))
                .unwrap_or_default();
            writeln!(out, "    {}{source}", row.id)?;
            if let Some(finding) = &row.finding {
                write_annotations(out, palette, finding, "      ", show_power_query)?;
            }
        }
    }
    writeln!(out)
}

/// Writes `--summary` output: the summary line and per-type totals, without
/// the findings list — the shape for models with thousands of findings.
///
/// # Errors
/// Propagates stream write failures.
pub fn human_summary(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
) -> io::Result<()> {
    write_summary(out, palette, report)?;

    if report.findings.is_empty() {
        writeln!(out, "No unused objects.")?;
    } else {
        for (kind, label) in GROUPS {
            let count = report
                .findings
                .iter()
                .filter(|finding| finding.kind == *kind)
                .count();
            if count > 0 {
                writeln!(out, "{}: {}", palette.bold(label), count)?;
            }
        }
    }
    if report.auto_date_time.is_empty() {
        return Ok(());
    }
    let mut counts = Vec::new();
    for (verdict, label) in [
        ("in_use", "in use"),
        ("unused_by_reports", "unused by reports"),
        ("dead", "dead"),
    ] {
        let count = report
            .auto_date_time
            .iter()
            .filter(|row| row.verdict == verdict)
            .count();
        if count > 0 {
            counts.push(format!("{count} {label}"));
        }
    }
    writeln!(
        out,
        "{}: {}",
        palette.bold("Auto date/time"),
        counts.join(", ")
    )
}

/// The summary line both human modes open with, plus the `[scan].ignore`
/// note when anything was suppressed.
fn write_summary(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
) -> io::Result<()> {
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
    if report.filtered_out > 0 {
        writeln!(
            out,
            "({} unused hidden by type filters)",
            report.filtered_out
        )?;
    }
    writeln!(out)
}

fn write_annotations(
    out: &mut dyn io::Write,
    palette: &Palette,
    finding: &Finding,
    indent: &str,
    show_power_query: bool,
) -> io::Result<()> {
    if finding.used_by.is_empty() {
        writeln!(
            out,
            "{}",
            palette.dim(&format!("{indent}← nothing references it"))
        )?;
    } else {
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
                    "{indent}← {prefix}used by {} — {}{also}",
                    used.id, used.provenance
                ))
            )?;
        }
    }
    // Cleanup-time supply-chain guidance, not verdict information: hidden
    // unless asked for (--power-query). The JSON field is unconditional.
    if show_power_query && !finding.named_in_power_query.is_empty() {
        let named = finding.named_in_power_query.join(", ");
        writeln!(
            out,
            "{}",
            palette.dim(&format!(
                "{indent}⭘ Power Query also names it ({named}) — safe to stop loading; \
                 removing it from the script means editing those steps too"
            ))
        )?;
    }
    Ok(())
}

/// Writes `--plain` output: one `<type>\t<id>` record per finding, then one
/// `auto_date_time:<verdict>\t<id>` record per auto date/time table.
///
/// # Errors
/// Propagates stream write failures.
pub fn plain(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    for finding in &report.findings {
        writeln!(out, "{}\t{}", finding.kind, finding.id)?;
    }
    for row in &report.auto_date_time {
        writeln!(out, "auto_date_time:{}\t{}", row.verdict, row.id)?;
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
            unused_total: report.unused_raw,
            ignored: report.ignored,
            auto_date_time: JsonAutoDateTimeCounts {
                in_use: count_verdict(&report.auto_date_time, "in_use"),
                unused_by_reports: count_verdict(&report.auto_date_time, "unused_by_reports"),
                dead: count_verdict(&report.auto_date_time, "dead"),
            },
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
                named_in_power_query: finding.named_in_power_query.clone(),
            })
            .collect(),
        auto_date_time: report
            .auto_date_time
            .iter()
            .map(|row| JsonAutoDateTimeRow {
                verdict: row.verdict,
                id: row.id.clone(),
                source_column: row.source_column.clone(),
                finding: row.finding.as_ref().map(|finding| JsonFinding {
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
                    named_in_power_query: finding.named_in_power_query.clone(),
                }),
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
    auto_date_time: Vec<JsonAutoDateTimeRow>,
    skips: JsonSkips,
}

#[derive(Serialize)]
struct JsonSummary {
    objects: usize,
    reachable: usize,
    roots: usize,
    /// Unused findings after `[scan].ignore` and the type flags — the length
    /// of `unused`.
    unused: usize,
    /// Every unused object in the model: before `[scan].ignore`, the type
    /// flags, and the auto date/time section move. `reachable` is always
    /// `objects − unused_total`.
    unused_total: usize,
    ignored: usize,
    auto_date_time: JsonAutoDateTimeCounts,
}

#[derive(Serialize)]
struct JsonAutoDateTimeCounts {
    in_use: usize,
    unused_by_reports: usize,
    dead: usize,
}

#[derive(Serialize)]
struct JsonAutoDateTimeRow {
    verdict: &'static str,
    id: String,
    /// The user's date column the machinery serves, when one resolves.
    source_column: Option<String>,
    /// The dead table's own finding — chain annotations included — moved here
    /// from `unused` so the verdict and the chain read together. Present
    /// exactly when `verdict` is `"dead"`.
    finding: Option<JsonFinding>,
}

fn count_verdict(rows: &[AutoDateTimeRow], verdict: &str) -> usize {
    rows.iter().filter(|row| row.verdict == verdict).count()
}

#[derive(Serialize)]
struct JsonFinding {
    #[serde(rename = "type")]
    kind: &'static str,
    id: String,
    used_by: Vec<JsonUsedBy>,
    /// Power Query expressions naming this column — supply-chain context,
    /// not liveness. Empty for everything but columns.
    named_in_power_query: Vec<String>,
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
