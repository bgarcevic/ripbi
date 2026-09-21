//! Rendering: everything a scan prints to stdout, in the three output modes
//! from `docs/cli-ux-guidelines.md` — human text, `--plain`, and `--json`.
//! The full contract (shapes, examples, schema) lives in `docs/output.md`
//! beside this crate.

use std::collections::{HashMap, HashSet};
use std::io;

use ripbi_core::{NameKey, ObjectId};
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
    /// Unused members of auto date/time tables — columns, hierarchies,
    /// partitions — that are not reported individually: the section's per-table
    /// verdict covers them, and removing the table removes its members (issue
    /// #47). Counted here so the summary line's arithmetic stays explicable.
    pub machinery_members: usize,
    /// Every broken visual binding (issue #60) that survives `[scan].ignore`
    /// and the type flags, sorted by where the binding lives. Suppressed
    /// entirely — moved into [`ScanOutput::broken_suppressed`] — when the
    /// model ingest recorded skips: a false "broken" claim is itself a
    /// breakage claim, so drift-parsed models don't get one.
    pub broken: Vec<BrokenOut>,
    /// Broken bindings detected but hidden by the type flags (`--measures`
    /// without `--broken`). Counted so the summary's arithmetic stays
    /// explicable.
    pub broken_hidden: usize,
    /// Broken bindings suppressed because the model ingest recorded
    /// `unknown_object` skips — the one drift kind that can hide the name a
    /// binding wrote, and so the precision bar of issue #60. `--strict`
    /// surfaces the skips behind this.
    pub broken_suppressed: usize,
    /// Broken bindings detected before any suppression, filtering, or
    /// `[scan].ignore` — the JSON summary's `broken_total`.
    pub broken_raw: usize,
    /// The unused objects that survive ignore filtering, sorted by identity.
    /// A dead auto date/time table's own finding lives in
    /// [`ScanOutput::auto_date_time`] instead, under its verdict, and the
    /// machinery's other members are not findings at all — they are counted
    /// in [`ScanOutput::machinery_members`] (issue #47).
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
    /// The object's model table — a relationship's "from" side, `None` for
    /// table-less kinds. `--json` renders it quoted; `--summary`'s worst-tables
    /// breakdown groups by it. Populated only for those two modes (issue #38).
    pub table: Option<NameKey>,
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

/// One broken visual binding (issue #60): a written field reference that no
/// longer resolves in the model, or that lands on an artifact whose own
/// expression no longer resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenOut {
    /// The written field reference, e.g. `'Sales'[Color]`.
    pub target: String,
    /// Why it does not resolve — a snake_case code, the `--plain`/JSON form.
    pub reason: &'static str,
    /// The broken artifact the binding lands on, for the
    /// `bound_artifact_broken` reason; `None` otherwise.
    pub bound_artifact: Option<String>,
    /// Where the binding lives, as a phrase — the same provenance rendering
    /// the unused findings' `used_by` lines carry.
    pub provenance: String,
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
    ("broken_visual", "Broken visual bindings"),
];

/// The machine kind vocabulary every type-selecting flag speaks: the keys
/// [`kind_of`] returns and `--plain`/`--json` emit, and what `scan --type`
/// and `deps --type`/`--consumer` validate against. Deliberately not the
/// [`GROUPS`] keys: `broken_visual` is a finding kind selected by scan's
/// `--broken`, not an object type.
pub(crate) const KINDS: &[&str] = &[
    "table",
    "column",
    "measure",
    "hierarchy",
    "partition",
    "relationship",
    "role",
    "calculation_item",
    "expression",
    "function",
    "report_measure",
];

/// How many tables `--summary`'s worst-tables breakdown shows (issue #38). Fixed
/// on purpose — the section answers "where do I start", which a top-10 list does
/// without another flag.
const WORST_TABLES_LIMIT: usize = 10;

/// The worst-tables breakdown of `--summary` (issue #38).
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorstTables {
    /// `(table display name, surviving finding count)`, count descending then
    /// table name, capped at [`WORST_TABLES_LIMIT`] rows.
    rows: Vec<(String, usize)>,
    /// Tables with findings beyond the shown rows.
    more: usize,
}

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
            ObjectId::Partition { table, .. } => format!("{} partition", table.quoted()),
            ObjectId::Expression { name } => format!("{} expression", name.quoted()),
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
/// always carries the underlying field). `broken_only` — a lone `--broken` —
/// swaps the empty-findings placeholder for `clean_line`'s broken phrasing.
///
/// # Errors
/// Propagates stream write failures.
pub fn human(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
    show_power_query: bool,
    broken_only: bool,
) -> io::Result<()> {
    write_summary(out, palette, report)?;

    if !report.findings.is_empty() {
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
    } else if let Some(line) = clean_line(report, broken_only) {
        writeln!(out, "{line}")?;
    }
    write_broken(out, palette, report)?;
    write_auto_date_time(out, palette, report, show_power_query)
}

/// The placeholder line for an empty findings list. Under a lone `--broken`
/// the run's scope is the broken bindings — the empty reachability list is
/// the filter's doing, not a result — so the placeholder speaks for them:
/// `No broken reports.` when none were found, and no line at all when the
/// section below has rows. Every other selection keeps the reachability
/// phrasing.
fn clean_line(report: &ScanOutput, broken_only: bool) -> Option<&'static str> {
    if !broken_only {
        return Some("No unused objects.");
    }
    report.broken.is_empty().then_some("No broken reports.")
}

/// The broken-visual section (issue #60), rendered in both human modes after
/// the reachability findings: like the auto date/time section, a different
/// verdict (written references, not reachability) — and advisory, so it
/// survives even a clean "No unused objects." scan. The `--broken` flag is
/// what turns it into a gate.
fn write_broken(out: &mut dyn io::Write, palette: &Palette, report: &ScanOutput) -> io::Result<()> {
    if report.broken.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "{}",
        palette.red(&format!("Broken visual bindings ({})", report.broken.len()))
    )?;
    for binding in &report.broken {
        writeln!(out, "  {}", binding.target)?;
        writeln!(
            out,
            "{}",
            palette.dim(&format!(
                "    ← {} — {}",
                reason_phrase(binding),
                binding.provenance
            ))
        )?;
    }
    writeln!(out)
}

/// The human phrase for one broken binding's reason: what the engine would
/// render as an error state, said statically.
fn reason_phrase(binding: &BrokenOut) -> String {
    match (binding.reason, binding.bound_artifact.as_deref()) {
        ("bound_artifact_broken", Some(artifact)) => {
            format!("bound artifact {artifact} has unresolvable references")
        }
        (reason, _) => format!(
            "{} not found in the model",
            reason
                .strip_suffix("_not_found")
                .unwrap_or(reason)
                .replace('_', " ")
        ),
    }
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

/// Groups findings by their model table: where does the bloat concentrate.
/// Table-less findings (report measures, shared expressions, functions) belong to
/// no table and are skipped; relationships count under their "from" side. Count
/// descending, then table name folded (case-insensitively, like every other
/// ordering), so the output is deterministic across runs.
fn worst_tables(findings: &[Finding]) -> WorstTables {
    let mut counts: HashMap<&NameKey, usize> = HashMap::new();
    for finding in findings {
        if let Some(table) = &finding.table {
            *counts.entry(table).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&NameKey, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let more = ranked.len().saturating_sub(WORST_TABLES_LIMIT);
    let rows = ranked
        .into_iter()
        .take(WORST_TABLES_LIMIT)
        .map(|(table, count)| (table.quoted().to_string(), count))
        .collect();
    WorstTables { rows, more }
}

/// Writes the `Worst tables:` block of `--summary` (issue #38): a bounded list of
/// the tables carrying the most surviving findings. Nothing prints when every
/// surviving finding is table-less.
fn write_worst_tables(
    out: &mut dyn io::Write,
    palette: &Palette,
    findings: &[Finding],
) -> io::Result<()> {
    let worst = worst_tables(findings);
    if worst.rows.is_empty() {
        return Ok(());
    }
    let name_width = worst
        .rows
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let count_width = worst
        .rows
        .iter()
        .map(|(_, count)| count.to_string().len())
        .max()
        .unwrap_or(0);
    writeln!(out)?;
    writeln!(out, "{}", palette.bold("Worst tables:"))?;
    for (name, count) in &worst.rows {
        writeln!(out, "  {name:<name_width$}  {count:>count_width$}")?;
    }
    if worst.more > 0 {
        writeln!(out, "  ... and {} more tables with findings", worst.more)?;
    }
    Ok(())
}

/// Writes `--summary` output: the summary line and per-type totals, without
/// the findings list — the shape for models with thousands of findings.
/// `broken_only` swaps the empty-findings placeholder the way `human` does.
///
/// # Errors
/// Propagates stream write failures.
pub fn human_summary(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
    broken_only: bool,
) -> io::Result<()> {
    write_summary(out, palette, report)?;

    if !report.findings.is_empty() {
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
        write_worst_tables(out, palette, &report.findings)?;
    } else if let Some(line) = clean_line(report, broken_only) {
        writeln!(out, "{line}")?;
    }
    if !report.broken.is_empty() {
        writeln!(
            out,
            "{}: {}",
            palette.red("Broken visual bindings"),
            report.broken.len()
        )?;
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
    // Issue #47: the aggregation is the machinery and the date columns it
    // serves, with the verdicts as the breakdown. The shared template table
    // pairs with no column, so the scope clause only makes sense when at
    // least one column resolves.
    let hidden_tables = report.auto_date_time.len();
    let date_columns = date_column_count(&report.auto_date_time);
    let scope = if date_columns > 0 {
        format!("{hidden_tables} hidden tables over {date_columns} date columns")
    } else {
        format!("{hidden_tables} hidden tables")
    };
    writeln!(
        out,
        "{}: {} ({})",
        palette.bold("Auto date/time"),
        scope,
        counts.join(", ")
    )
}

/// Distinct user date columns the section's machinery serves — the "over M
/// date columns" of `--summary` (issue #47). Each `LocalDateTable_*` serves
/// one column; the shared `DateTableTemplate_*` serves none.
fn date_column_count(rows: &[AutoDateTimeRow]) -> usize {
    rows.iter()
        .filter_map(|row| row.source_column.as_deref())
        .collect::<HashSet<&str>>()
        .len()
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
            "({} findings suppressed by [scan].ignore)",
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
    if report.broken_hidden > 0 {
        writeln!(
            out,
            "({} broken-visual bindings hidden by type filters)",
            report.broken_hidden
        )?;
    }
    if report.broken_suppressed > 0 {
        writeln!(
            out,
            "({} possible broken-visual bindings suppressed — the model ingest \
             reported skips, listed on stderr; --strict fails on those skips)",
            report.broken_suppressed
        )?;
    }
    if report.machinery_members > 0 {
        writeln!(
            out,
            "({} unused auto date/time members covered by their tables' verdicts)",
            report.machinery_members
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

/// Writes `--plain` output: one `<type>\t<id>` record per finding, one
/// `broken_visual:<reason>\t<target>` record per reported broken binding, then
/// one `auto_date_time:<verdict>\t<id>` record per auto date/time table.
///
/// # Errors
/// Propagates stream write failures.
pub fn plain(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    for finding in &report.findings {
        writeln!(out, "{}\t{}", finding.kind, finding.id)?;
    }
    for binding in &report.broken {
        writeln!(out, "broken_visual:{}\t{}", binding.reason, binding.target)?;
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
            broken: report.broken.len(),
            broken_total: report.broken_raw,
            auto_date_time: JsonAutoDateTimeCounts {
                hidden_tables: report.auto_date_time.len(),
                date_columns: date_column_count(&report.auto_date_time),
                member_findings: report.machinery_members,
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
                table: finding
                    .table
                    .as_ref()
                    .map(|table| table.quoted().to_string()),
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
        broken: report
            .broken
            .iter()
            .map(|binding| JsonBroken {
                target: binding.target.clone(),
                reason: binding.reason,
                bound_artifact: binding.bound_artifact.clone(),
                provenance: binding.provenance.clone(),
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
                    table: finding
                        .table
                        .as_ref()
                        .map(|table| table.quoted().to_string()),
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
    /// Broken visual bindings (issue #60) that survived `[scan].ignore` and
    /// the type flags. Empty — but present — when the scan found none.
    broken: Vec<JsonBroken>,
    auto_date_time: Vec<JsonAutoDateTimeRow>,
    skips: JsonSkips,
}

#[derive(Serialize)]
struct JsonBroken {
    /// The written field reference, e.g. `'Sales'[Color]`.
    target: String,
    /// Why it does not resolve, snake_case: `table_not_found`,
    /// `field_not_found`, `measure_not_found`, `hierarchy_not_found`,
    /// `level_not_found`, or `bound_artifact_broken`.
    reason: &'static str,
    /// The broken artifact the binding lands on — present exactly when
    /// `reason` is `bound_artifact_broken`.
    bound_artifact: Option<String>,
    /// Where the binding lives, as a phrase — the same provenance the unused
    /// findings' `used_by` entries carry.
    provenance: String,
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
    /// Broken visual bindings (issue #60) after `[scan].ignore` and the type
    /// flags — the length of `broken`.
    broken: usize,
    /// Every broken binding detected, before any suppression, `[scan].ignore`,
    /// or type flag. Larger than `broken` when the model ingest's skips
    /// suppressed findings (the issue #60 precision bar) or a type filter hid
    /// them.
    broken_total: usize,
    auto_date_time: JsonAutoDateTimeCounts,
}

#[derive(Serialize)]
struct JsonAutoDateTimeCounts {
    /// Every auto date/time table the section reports, all verdicts together —
    /// the sum of the three verdict counts.
    hidden_tables: usize,
    /// Distinct user date columns the machinery serves (issue #47). The shared
    /// `DateTableTemplate_*` pairs with no column, so this can be smaller
    /// than `hidden_tables`.
    date_columns: usize,
    /// The machinery's unused members (columns, hierarchies, partitions) that
    /// are not in `unused` individually — the per-table rows cover them.
    member_findings: usize,
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
    /// The finding's model table, quoted (`'Sales'`) — a relationship's "from"
    /// side. `null` for table-less kinds (roles, shared expressions, functions,
    /// report measures).
    table: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The vocabulary is exactly what `kind_of` can return: one entry per
    /// `ObjectId` variant, no gaps, no extras. A type-selecting flag that
    /// accepted a key outside this set would silently select nothing.
    #[test]
    fn the_kind_vocabulary_covers_every_object_kind() {
        let every_kind = [
            ObjectId::Table {
                table: NameKey::new("Sales"),
            },
            ObjectId::Column {
                table: NameKey::new("Sales"),
                column: NameKey::new("Amount"),
            },
            ObjectId::Measure {
                table: NameKey::new("Sales"),
                measure: NameKey::new("Total"),
            },
            ObjectId::Hierarchy {
                table: NameKey::new("Date"),
                hierarchy: NameKey::new("Calendar"),
            },
            ObjectId::Partition {
                table: NameKey::new("Sales"),
                partition: NameKey::new("P"),
            },
            ObjectId::Relationship {
                from_table: NameKey::new("Sales"),
                from_column: NameKey::new("Key"),
                to_table: NameKey::new("Dim"),
                to_column: NameKey::new("Key"),
            },
            ObjectId::Role {
                role: NameKey::new("Reader"),
            },
            ObjectId::CalculationItem {
                table: NameKey::new("TI"),
                item: NameKey::new("YTD"),
            },
            ObjectId::Expression {
                name: NameKey::new("Param"),
            },
            ObjectId::Function {
                name: NameKey::new("F"),
            },
            ObjectId::ReportMeasure {
                measure: NameKey::new("Local"),
            },
        ]
        .each_ref()
        .map(kind_of);

        let mut sorted = KINDS.to_vec();
        sorted.sort_unstable();
        let mut expected = every_kind.to_vec();
        expected.sort_unstable();
        assert_eq!(sorted, expected, "KINDS and kind_of must stay in lockstep");
    }

    fn finding(table: Option<&str>) -> Finding {
        Finding {
            kind: "measure",
            id: String::new(),
            table: table.map(NameKey::new),
            used_by: Vec::new(),
            named_in_power_query: Vec::new(),
        }
    }

    #[test]
    fn orders_by_count_then_table_name() {
        let findings = [
            finding(Some("B")),
            finding(Some("A")),
            finding(Some("A")),
            finding(Some("C")),
            finding(Some("C")),
            finding(Some("C")),
        ];

        let worst = worst_tables(&findings);

        assert_eq!(
            worst.rows,
            vec![
                ("'C'".to_string(), 3),
                ("'A'".to_string(), 2),
                ("'B'".to_string(), 1),
            ]
        );
        assert_eq!(worst.more, 0);
    }

    /// The tie-break follows the model's case-insensitive identity ordering, not
    /// the display bytes: `apple` precedes `Zebra` at equal counts.
    #[test]
    fn tie_breaks_on_the_folded_table_name() {
        let findings = [
            finding(Some("Zebra")),
            finding(Some("Zebra")),
            finding(Some("apple")),
            finding(Some("apple")),
        ];

        let worst = worst_tables(&findings);

        assert_eq!(
            worst.rows,
            vec![("'apple'".to_string(), 2), ("'Zebra'".to_string(), 2)]
        );
    }

    #[test]
    fn skips_table_less_findings() {
        let findings = [finding(None), finding(None), finding(Some("Sales"))];

        let worst = worst_tables(&findings);

        assert_eq!(worst.rows, vec![("'Sales'".to_string(), 1)]);
        assert_eq!(worst.more, 0);
    }

    #[test]
    fn is_empty_when_every_finding_is_table_less() {
        let findings = [finding(None), finding(None)];

        let worst = worst_tables(&findings);

        assert!(worst.rows.is_empty());
        assert_eq!(worst.more, 0);
    }

    #[test]
    fn caps_the_rows_and_counts_the_rest() {
        let findings: Vec<Finding> = (0..WORST_TABLES_LIMIT + 3)
            .map(|i| finding(Some(&format!("T{i:02}"))))
            .collect();

        let worst = worst_tables(&findings);

        assert_eq!(worst.rows.len(), WORST_TABLES_LIMIT);
        assert_eq!(worst.more, 3);
    }

    fn scan_output(findings: Vec<Finding>) -> ScanOutput {
        ScanOutput {
            target: String::new(),
            reports: Vec::new(),
            objects: 0,
            reachable: 0,
            roots: 0,
            unused_raw: findings.len(),
            ignored: 0,
            filtered_out: 0,
            machinery_members: 0,
            broken: Vec::new(),
            broken_hidden: 0,
            broken_suppressed: 0,
            broken_raw: 0,
            findings,
            auto_date_time: Vec::new(),
            skips: Vec::new(),
        }
    }

    #[test]
    fn the_summary_prints_a_padded_worst_tables_block() {
        let findings = vec![
            finding(Some("Sales")),
            finding(Some("Sales")),
            finding(Some("Sales")),
            finding(Some("Customer")),
            finding(Some("Customer")),
            finding(None),
        ];

        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &scan_output(findings), false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("\nWorst tables:\n  'Sales'     3\n  'Customer'  2\n"),
            "the block is blank-line separated, count descending, columns aligned:\n{text}"
        );
    }

    #[test]
    fn the_summary_announces_tables_beyond_the_top() {
        let findings: Vec<Finding> = (0..WORST_TABLES_LIMIT + 2)
            .map(|i| finding(Some(&format!("T{i:02}"))))
            .collect();

        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &scan_output(findings), false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("  ... and 2 more tables with findings\n"),
            "the cut is announced:\n{text}"
        );
    }

    #[test]
    fn no_worst_tables_block_when_nothing_has_a_table() {
        let mut out = Vec::new();
        human_summary(
            &mut out,
            &Palette::plain(),
            &scan_output(vec![finding(None)]),
            false,
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains("Worst tables"),
            "table-less findings belong to no table:\n{text}"
        );
    }

    #[test]
    fn json_carries_the_quoted_table_and_nulls_table_less_kinds() {
        let mut tabled = finding(Some("Sales"));
        tabled.id = "'Sales'[Legacy Total]".to_string();

        let mut out = Vec::new();
        json(&mut out, &scan_output(vec![tabled, finding(None)])).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["unused"][0]["table"], "'Sales'");
        assert!(value["unused"][1]["table"].is_null());
    }

    /// Labels are DAX identifiers, so an internal apostrophe doubles — the same
    /// escaping `ObjectId`'s own `Display` uses (issue #63).
    #[test]
    fn power_query_labels_escape_apostrophes() {
        let ids = [
            ObjectId::Partition {
                table: NameKey::new("O'Brien"),
                partition: NameKey::new("O'Brien"),
            },
            ObjectId::Expression {
                name: NameKey::new("O'Brien"),
            },
        ];

        assert_eq!(
            power_query_labels(&ids),
            vec![
                "'O''Brien' partition".to_string(),
                "'O''Brien' expression".to_string(),
            ]
        );
    }

    fn auto_row(verdict: &'static str, id: &str, source_column: Option<&str>) -> AutoDateTimeRow {
        AutoDateTimeRow {
            verdict,
            id: id.to_string(),
            source_column: source_column.map(str::to_string),
            finding: None,
        }
    }

    fn scan_output_with_auto_date_time(
        findings: Vec<Finding>,
        auto_date_time: Vec<AutoDateTimeRow>,
    ) -> ScanOutput {
        let mut output = scan_output(findings);
        output.auto_date_time = auto_date_time;
        output
    }

    /// Issue #47: the summary aggregates the machinery and the date columns it
    /// serves, with the verdicts as the breakdown. Columns count distinctly —
    /// two tables over one column — and the shared template table, which
    /// serves no column, still counts toward the tables.
    #[test]
    fn the_summary_counts_hidden_tables_over_distinct_date_columns() {
        let rows = vec![
            auto_row(
                "in_use",
                "table 'LocalDateTable_a'",
                Some("'Opportunity Calendar'[Date]"),
            ),
            auto_row(
                "dead",
                "table 'LocalDateTable_b'",
                Some("'Opportunity'[Date]"),
            ),
            auto_row(
                "dead",
                "table 'LocalDateTable_c'",
                Some("'Opportunity'[Date]"),
            ),
            auto_row("dead", "table 'DateTableTemplate_d'", None),
        ];

        let mut out = Vec::new();
        human_summary(
            &mut out,
            &Palette::plain(),
            &scan_output_with_auto_date_time(vec![], rows),
            false,
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(
                "Auto date/time: 4 hidden tables over 2 date columns (1 in use, 3 dead)\n"
            ),
            "the aggregation names the machinery, the columns, and the verdicts:\n{text}"
        );
    }

    /// A model whose machinery pairs with no column at all (only the template,
    /// or unresolvable pairings) still aggregates — without a scope clause.
    #[test]
    fn the_summary_drops_the_column_clause_when_no_column_resolves() {
        let rows = vec![
            auto_row("dead", "table 'LocalDateTable_a'", None),
            auto_row("dead", "table 'DateTableTemplate_b'", None),
        ];

        let mut out = Vec::new();
        human_summary(
            &mut out,
            &Palette::plain(),
            &scan_output_with_auto_date_time(vec![], rows),
            false,
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("Auto date/time: 2 hidden tables (2 dead)\n"),
            "no column clause without a resolvable column:\n{text}"
        );
    }

    /// The members the section covers leave a gap in the summary line's
    /// arithmetic (objects ≠ reachable + unused), so the note explains it —
    /// the same convention as the `[scan].ignore` and type-filter notes.
    #[test]
    fn the_summary_explains_the_machinery_members_gap() {
        let mut output = scan_output_with_auto_date_time(
            vec![],
            vec![auto_row(
                "dead",
                "table 'LocalDateTable_a'",
                Some("'Opportunity'[Date]"),
            )],
        );
        output.machinery_members = 8;

        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &output, false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("(8 unused auto date/time members covered by their tables' verdicts)\n"),
            "the covered members are accounted for:\n{text}"
        );
    }

    #[test]
    fn json_counts_the_machinery_and_its_date_columns() {
        let rows = vec![
            auto_row(
                "dead",
                "table 'LocalDateTable_a'",
                Some("'Opportunity'[Date]"),
            ),
            auto_row(
                "dead",
                "table 'LocalDateTable_b'",
                Some("'Opportunity'[Date]"),
            ),
            auto_row("dead", "table 'DateTableTemplate_c'", None),
        ];
        let mut output = scan_output_with_auto_date_time(vec![], rows);
        output.machinery_members = 12;

        let mut out = Vec::new();
        json(&mut out, &output).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let counts = &value["summary"]["auto_date_time"];
        assert_eq!(counts["hidden_tables"], 3);
        assert_eq!(counts["date_columns"], 1);
        assert_eq!(counts["member_findings"], 12);
        assert_eq!(counts["dead"], 3);
    }

    fn broken(target: &str, reason: &'static str, artifact: Option<&str>) -> BrokenOut {
        BrokenOut {
            target: target.to_string(),
            reason,
            bound_artifact: artifact.map(str::to_string),
            provenance: "field well 'Values' — visual 'V1' on page 'P1' in report 'Mini'"
                .to_string(),
        }
    }

    /// Issue #60: the section renders — with the reason phrase and the
    /// binding site — even when the scan is otherwise clean, because it is a
    /// different verdict than reachability's.
    #[test]
    fn the_broken_section_renders_on_an_otherwise_clean_scan() {
        let mut output = scan_output(vec![]);
        output.broken = vec![broken("'Sales'[Color]", "field_not_found", None)];

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("Broken visual bindings (1)\n  'Sales'[Color]\n    ← field not found in the model — field well 'Values' — visual 'V1' on page 'P1' in report 'Mini'\n"),
            "the section names the field, the reason, and the site:\n{text}"
        );
        assert!(text.contains("No unused objects."));
    }

    /// Under a lone `--broken` the placeholder speaks for the scope the flag
    /// asked about: nothing flags reads "No broken reports.", never the
    /// reachability phrasing — the empty findings list is the filter's doing.
    #[test]
    fn a_broken_only_clean_scan_places_the_no_broken_reports_line() {
        let mut out = Vec::new();
        human(
            &mut out,
            &Palette::plain(),
            &scan_output(vec![]),
            false,
            true,
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("No broken reports."),
            "the placeholder names the scope:\n{text}"
        );
        assert!(
            !text.contains("No unused objects."),
            "the run never scoped reachability:\n{text}"
        );
    }

    /// With rows in the section, the placeholder line is dropped entirely —
    /// "No unused objects." stacked above the bindings read like a verdict
    /// the flag never asked for.
    #[test]
    fn a_broken_only_scan_with_rows_prints_no_placeholder() {
        let mut output = scan_output(vec![]);
        output.broken = vec![broken("'Sales'[Color]", "field_not_found", None)];

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, true).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Broken visual bindings (1)"), ":\n{text}");
        assert!(!text.contains("No unused objects."), ":\n{text}");
        assert!(!text.contains("No broken reports."), ":\n{text}");
    }

    /// `--summary` shares the placeholder: one phrasing per scope, both human
    /// modes.
    #[test]
    fn the_summary_shares_the_broken_only_placeholder() {
        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &scan_output(vec![]), true).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("No broken reports."), ":\n{text}");
    }

    /// The propagated reason names the artifact: the visual is what a user
    /// sees, but the measure is what they fix.
    #[test]
    fn a_broken_artifact_reason_names_the_artifact() {
        let mut output = scan_output(vec![]);
        output.broken = vec![broken(
            "'Sales'[Broken]",
            "bound_artifact_broken",
            Some("'Sales'[Broken Total]"),
        )];

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("← bound artifact 'Sales'[Broken Total] has unresolvable references"),
            "the artifact is named:\n{text}"
        );
    }

    #[test]
    fn plain_carries_broken_visual_records_with_their_reason() {
        let mut output = scan_output(vec![]);
        output.broken = vec![
            broken("'Sales'[Color]", "field_not_found", None),
            broken(
                "'Sales'[Broken]",
                "bound_artifact_broken",
                Some("'Sales'[T]"),
            ),
        ];

        let mut out = Vec::new();
        plain(&mut out, &output).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("broken_visual:field_not_found\t'Sales'[Color]\n"),
            "one record per binding, reason in the kind prefix:\n{text}"
        );
        assert!(
            text.contains("broken_visual:bound_artifact_broken\t'Sales'[Broken]\n"),
            "{text}"
        );
    }

    #[test]
    fn json_carries_broken_bindings_and_their_totals() {
        let mut output = scan_output(vec![]);
        output.broken = vec![broken("'Sales'[Color]", "field_not_found", None)];
        output.broken_raw = 3;

        let mut out = Vec::new();
        json(&mut out, &output).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["summary"]["broken"], 1);
        assert_eq!(value["summary"]["broken_total"], 3);
        assert_eq!(value["broken"][0]["target"], "'Sales'[Color]");
        assert_eq!(value["broken"][0]["reason"], "field_not_found");
        assert!(value["broken"][0]["bound_artifact"].is_null());
        assert_eq!(
            value["broken"][0]["provenance"],
            "field well 'Values' — visual 'V1' on page 'P1' in report 'Mini'"
        );
    }

    /// The suppression note explains findings the reader cannot see — the
    /// issue #60 precision bar — and survives a clean scan.
    #[test]
    fn the_summary_explains_suppressed_broken_bindings() {
        let mut output = scan_output(vec![]);
        output.broken_suppressed = 2;

        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &output, false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("(2 possible broken-visual bindings suppressed — the model ingest reported skips, listed on stderr; --strict fails on those skips)\n"),
            "the suppression is accounted for:\n{text}"
        );
    }
}
