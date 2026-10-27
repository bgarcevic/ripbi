//! The scan's JSON contract: schema version 1, documented in
//! `crates/ripbi-cli/docs/scan/json.md`. `ripbi scan --json` writes it, and
//! a desktop UI's export writes the same bytes.

use std::io;

use ripbi_core::SizeBasis;
use serde::Serialize;

use crate::compare::{Coverage, Entry};
use crate::output::{AutoDateTimeRow, Finding, ScanOutput};

/// Writes the v1 schema (`docs/scan/json.md`), pretty-printed, newline-terminated.
///
/// # Errors
/// Propagates stream write failures.
pub fn write(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    let payload = JsonReport {
        schema_version: 1,
        target: report.target.clone(),
        reports: report.reports.clone(),
        summary: JsonSummary {
            findings: report.reported(),
            objects: report.objects,
            reachable: report.reachable,
            roots: report.roots,
            unused: report.findings.len(),
            unused_total: report.unused_raw,
            ignored: report.ignored,
            kept: report.kept,
            queried: report.queried,
            broken: report.broken.len(),
            broken_total: report.broken_raw,
            broken_artifacts: report.broken_artifacts.len(),
            broken_artifacts_total: report.broken_artifacts_raw,
            unused_bytes: report.unused_storage.map(|storage| storage.bytes),
            unused_bytes_lower_bound: report
                .unused_storage
                .and_then(|storage| storage.lower_bound.then_some(true)),
            model_bytes: report.model_bytes,
            stats_source: report.stats_source.clone(),
            auto_date_time: JsonAutoDateTimeCounts {
                hidden_tables: report.auto_date_time.len(),
                date_columns: report.date_columns(),
                member_findings: report.machinery_members,
                in_use: count_verdict(&report.auto_date_time, "in_use"),
                unused_by_reports: count_verdict(&report.auto_date_time, "unused_by_reports"),
                dead: count_verdict(&report.auto_date_time, "dead"),
            },
        },
        unused: report.findings.iter().map(JsonFinding::from).collect(),
        broken: report
            .broken
            .iter()
            .map(|binding| JsonBroken {
                target: binding.target.clone(),
                reason: binding.reason,
                bound_artifact: binding.bound_artifact.clone(),
                bound_artifact_report: binding.bound_artifact_report.clone(),
                provenance: binding.provenance.clone(),
                location: binding.location.clone(),
            })
            .collect(),
        broken_artifacts: report
            .broken_artifacts
            .iter()
            .map(|artifact| JsonBrokenArtifact {
                id: artifact.id.clone(),
                kind: artifact.kind,
                report: artifact.report.clone(),
                unresolved_references: artifact.unresolved_references.clone(),
            })
            .collect(),
        auto_date_time: report
            .auto_date_time
            .iter()
            .map(|row| JsonAutoDateTimeRow {
                verdict: row.verdict,
                id: row.id.clone(),
                source_column: row.source_column.clone(),
                finding: row.finding.as_ref().map(JsonFinding::from),
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
        compare: report.compare.as_ref().map(|compare| JsonCompare {
            root: compare.root.clone(),
            baseline: compare.coverage,
            baseline_detail: compare.coverage_detail.clone(),
            existing: compare.existing,
            unverified: compare.unverified.clone(),
            fixed: compare.fixed.clone(),
            fixed_uncertain: report.fixed_uncertain(),
        }),
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
    /// type selection. Empty — but present — when the scan found none.
    broken: Vec<JsonBroken>,
    broken_artifacts: Vec<JsonBrokenArtifact>,
    auto_date_time: Vec<JsonAutoDateTimeRow>,
    skips: JsonSkips,
    /// The `--compare-root` comparison (issue #141); absent without the flag,
    /// so other scans' output is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    compare: Option<JsonCompare>,
}

#[derive(Serialize)]
struct JsonCompare {
    root: String,
    /// `scanned`, `skipped` (the model had no connected reports there), or
    /// `unavailable` (the scan could not run there).
    baseline: Coverage,
    /// Why the baseline is `skipped` or `unavailable`; absent when scanned.
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_detail: Option<String>,
    /// Findings that already existed there, left out of every array above.
    existing: usize,
    /// Under a `skipped` baseline: unused findings on objects that existed
    /// there, whose usage there is unknown. Left out of every array above and
    /// of the exit code.
    unverified: Vec<Entry>,
    /// Findings of the other checkout this scan no longer detects.
    fixed: Vec<Entry>,
    /// `true` when `fixed` is non-empty and this scan's ingest recorded an
    /// `unknown_object` skip, so a "fixed" finding may be parse damage.
    fixed_uncertain: bool,
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
    bound_artifact_report: Option<String>,
    /// Where the binding lives, as a phrase — the same provenance the unused
    /// findings' `used_by` entries carry. Stable across retitles.
    provenance: String,
    /// The same site with the visual's type and title and the page's display
    /// name — what Power BI Desktop shows. Display only.
    location: String,
}

#[derive(Serialize)]
struct JsonBrokenArtifact {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    report: Option<String>,
    unresolved_references: Vec<String>,
}

#[derive(Serialize)]
struct JsonSummary {
    /// Every reported finding: `unused`, the auto date/time rows that are not
    /// `in_use`, `broken`, and `broken_artifacts` — the unsuppressed SARIF
    /// result count. Under `--compare-root`, the new findings.
    findings: usize,
    objects: usize,
    reachable: usize,
    roots: usize,
    /// Unused findings after `[scan].ignore` and type selection — the length
    /// of `unused`.
    unused: usize,
    /// Every unused object in the model: before `[scan].ignore`, the type
    /// flags, and the auto date/time section move. `reachable` is always
    /// `objects − unused_total`.
    unused_total: usize,
    ignored: usize,
    /// Objects a `ripbi_keep` model annotation keeps (issue #151) — roots,
    /// so never in `unused`; a kept table counts once, not per member.
    kept: usize,
    /// Objects a `--queries-from` query log references directly — roots, so
    /// never in `unused`. 0 without a log.
    queried: usize,
    /// Broken visual bindings (issue #60) after `[scan].ignore` and the type
    /// flags — the length of `broken`.
    broken: usize,
    /// Every broken binding detected, before any suppression, `[scan].ignore`,
    /// or type flag. Larger than `broken` when the model ingest's skips
    /// suppressed findings (the issue #60 precision bar) or a type filter hid
    /// them.
    broken_total: usize,
    broken_artifacts: usize,
    broken_artifacts_total: usize,
    /// Bytes on disk across `unused`, each file counted once (issue #122).
    /// Present only for `.abf`/PBIX models.
    #[serde(skip_serializing_if = "Option::is_none")]
    unused_bytes: Option<u64>,
    /// `true` when `unused_bytes` is a lower bound; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    unused_bytes_lower_bound: Option<bool>,
    /// Bytes of every data file in the model. `.abf`/PBIX only.
    #[serde(skip_serializing_if = "Option::is_none")]
    model_bytes: Option<u64>,
    /// The `.abf`/PBIX the sizes were attached from (issue #129); absent when
    /// the model carries its own catalog or no source was attached.
    #[serde(skip_serializing_if = "Option::is_none")]
    stats_source: Option<String>,
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
    /// Bytes on disk (issue #122). This and the three fields below appear
    /// only for tables, columns, and relationships of `.abf`/PBIX models.
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<u64>,
    /// `"files"` (exact) or `"lower_bound"` (files missing from the backup log).
    #[serde(skip_serializing_if = "Option::is_none")]
    size_basis: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cardinality: Option<u64>,
}

impl From<&Finding> for JsonFinding {
    fn from(finding: &Finding) -> Self {
        let storage = finding.storage.unwrap_or_default();
        Self {
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
            bytes: storage.bytes,
            size_basis: storage.bytes.map(|_| match storage.basis {
                SizeBasis::Files => "files",
                SizeBasis::LowerBound => "lower_bound",
                SizeBasis::Engine => "engine",
            }),
            rows: storage.rows,
            cardinality: storage.cardinality,
        }
    }
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
