//! The result of one scan, as presentation-ready data: what every front end
//! renders — the CLI's human, `--plain`, `--json`, SARIF, and Markdown modes,
//! and the desktop UIs. The JSON contract built from it lives in
//! `crates/ripbi-cli/docs/scan/json.md`.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use ripbi_core::{NameKey, ObjectId, StorageStats};

use crate::compare::{Coverage, Entry};

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
    /// Objects the model declares deliberately kept with a `ripbi_keep`
    /// annotation (issue #151): reachability roots, never findings. Counted
    /// for the JSON summary only; the human output lists them nowhere.
    pub kept: usize,
    /// Objects the `--queries-from` query log references directly: roots,
    /// never findings. Counted for the JSON summary only.
    pub queried: usize,
    /// Unused objects hidden by type selection.
    /// `[scan].ignore` suppressions are counted in [`ScanOutput::ignored`]
    /// instead.
    pub filtered_out: usize,
    /// Unused members of auto date/time tables — columns, hierarchies,
    /// partitions — that are not reported individually: the section's per-table
    /// verdict covers them, and removing the table removes its members (issue
    /// #47). Counted here so the summary line's arithmetic stays explicable.
    pub machinery_members: usize,
    /// Every broken visual binding (issue #60) that survives `[scan].ignore`
    /// and type selection, sorted by where the binding lives. Suppressed
    /// entirely — moved into [`ScanOutput::broken_suppressed`] — when the
    /// model ingest recorded skips: a false "broken" claim is itself a
    /// breakage claim, so drift-parsed models don't get one.
    pub broken: Vec<BrokenOut>,
    /// Broken bindings detected but hidden by `--type` without `--broken`.
    /// Counted so the summary's arithmetic stays
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
    /// Broken DAX owners reported independently of visual bindings.
    pub broken_artifacts: Vec<BrokenArtifactOut>,
    /// Artifact findings hidden by type selection.
    pub broken_artifacts_hidden: usize,
    /// Artifact findings suppressed by an `unknown_object` model skip.
    pub broken_artifacts_suppressed: usize,
    /// Artifact findings before filtering and suppression.
    pub broken_artifacts_raw: usize,
    /// The unused objects that survive ignore filtering, sorted by identity.
    /// A dead auto date/time table's own finding lives in
    /// [`ScanOutput::auto_date_time`] instead, under its verdict, and the
    /// machinery's other members are not findings at all — they are counted
    /// in [`ScanOutput::machinery_members`] (issue #47).
    pub findings: Vec<Finding>,
    /// One row per auto date/time table (`LocalDateTable_*` /
    /// `DateTableTemplate_*`): the provenance verdict no reachability pass can
    /// produce. Rows suppressed by `[scan].ignore` are absent, and so is the
    /// whole section when a type filter runs without `--type table`.
    pub auto_date_time: Vec<AutoDateTimeRow>,
    /// Every skip notice ingestion recorded.
    pub skips: Vec<SkipNoticeOut>,
    /// Bytes of every data file in the model — `.abf`/PBIX only.
    pub model_bytes: Option<u64>,
    /// What the reported findings cost on disk, when any carries a size
    /// (issue #122). `None` for formats without a storage catalog.
    pub unused_storage: Option<UnusedStorage>,
    /// Where the storage statistics came from when the model has no catalog of
    /// its own: `--stats-from`, `[scan].stats_from`, or `.pbi/cache.abf` (issue #129).
    pub stats_source: Option<String>,
    /// The `--compare-root` comparison (issue #141); `None` without the flag.
    pub compare: Option<CompareOut>,
    /// Wall time, shown on the verdict line when the stage ticker ran (an
    /// interactive terminal); `None` keeps piped and captured output stable.
    pub elapsed: Option<Duration>,
}

impl ScanOutput {
    /// Every reported finding: the unused objects, the auto date/time tables
    /// that are not `in_use` (a dead table's own finding counts with its row),
    /// and both breakage lists — one per unsuppressed SARIF result. Under
    /// `--compare-root` these are exactly the new findings.
    #[must_use]
    pub fn reported(&self) -> usize {
        self.findings.len()
            + self
                .auto_date_time
                .iter()
                .filter(|row| row.verdict != "in_use")
                .count()
            + self.broken.len()
            + self.broken_artifacts.len()
    }

    /// Whether a `Fixed since` list may be parse damage: some finding vanished,
    /// and this scan's ingest recorded an `unknown_object` skip — the one kind
    /// that can drop an object (the same bar issue #60 holds breakage claims to).
    #[must_use]
    pub fn fixed_uncertain(&self) -> bool {
        self.compare
            .as_ref()
            .is_some_and(|compare| !compare.fixed.is_empty())
            && self.skips.iter().any(|skip| skip.kind == "unknown_object")
    }

    /// Distinct user date columns the auto date/time machinery serves — the
    /// "over M date columns" of `--summary` (issue #47). Each
    /// `LocalDateTable_*` serves one column; the shared `DateTableTemplate_*`
    /// serves none.
    #[must_use]
    pub fn date_columns(&self) -> usize {
        self.auto_date_time
            .iter()
            .filter_map(|row| row.source_column.as_deref())
            .collect::<HashSet<&str>>()
            .len()
    }
}

/// How a scan compared against the same scan in another checkout (issue #141).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareOut {
    /// The other checkout, as given.
    pub root: String,
    /// What the output calls it: `--compare-label`, else `root`.
    pub label: String,
    /// How far the other checkout's scan can be trusted: `Skipped` when its
    /// model had no connected reports, `Unavailable` when it could not run.
    pub coverage: Coverage,
    /// Why coverage is incomplete; `None` for a scanned baseline.
    pub coverage_detail: Option<String>,
    /// Reportable findings that already existed there: left out of every
    /// list and of the exit code.
    pub existing: usize,
    /// Under a skipped baseline: unused findings on objects that existed
    /// there, whose usage there was never checked. Not findings: listed
    /// apart, never gating.
    pub unverified: Vec<Entry>,
    /// Findings of the other checkout this scan no longer detects at all.
    pub fixed: Vec<Entry>,
}

/// The on-disk cost of the reported findings (issue #122).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnusedStorage {
    /// Bytes across the findings, each file counted once: a column or
    /// relationship whose table is itself a finding is covered by the table.
    pub bytes: u64,
    /// Findings that carry a size.
    pub objects: usize,
    /// Some finding's size is a lower bound (files missing from the backup log).
    pub lower_bound: bool,
    /// The sizes are the engine's in-memory figures (a `.vpax` source, issue
    /// #108), not files on disk.
    pub in_memory: bool,
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
    /// Storage statistics for tables, columns, and relationships of an
    /// `.abf`/PBIX model (issue #122).
    pub storage: Option<StorageStats>,
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
    /// Report path when the bound artifact is a report-level measure.
    pub bound_artifact_report: Option<String>,
    /// Where the binding lives, as a phrase — the same provenance rendering
    /// the unused findings' `used_by` lines carry. Stable: it keys baselines.
    pub provenance: String,
    /// Where the binding lives, phrased so a person can find it in Power BI
    /// Desktop: the visual's type and title and the page's tab name, with
    /// the generated IDs alongside. Display only — it changes when a visual
    /// is retitled, so nothing keys on it.
    pub location: String,
}

/// One DAX-owning artifact with unresolved references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenArtifactOut {
    pub id: String,
    pub kind: &'static str,
    pub report: Option<String>,
    pub unresolved_references: Vec<String>,
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

/// The machine kind vocabulary every type-selecting flag speaks: the keys
/// [`kind_of`] returns and `--plain`/`--json` emit, and what `scan --type`
/// and `deps --type`/`--consumer` validate against. Deliberately not the
/// [`GROUPS`] keys: breakage kinds are selected by scan's `--broken`, not
/// object types.
pub const KINDS: &[&str] = &[
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
    "bookmark",
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
        ObjectId::Bookmark { .. } => "bookmark",
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

/// Group order and labels: the fixed section order of human output.
pub const GROUPS: &[(&str, &str)] = &[
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
    ("bookmark", "Bookmarks"),
    ("broken_visual", "Broken visual bindings"),
];

/// How many tables `--summary`'s worst-tables breakdown shows (issue #38). Fixed
/// on purpose — the section answers "where do I start", which a top-10 list does
/// without another flag.
pub const WORST_TABLES_LIMIT: usize = 10;

/// The worst-tables breakdown of `--summary` (issue #38).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorstTables {
    /// `(table display name, surviving finding count)`, count descending then
    /// table name, capped at [`WORST_TABLES_LIMIT`] rows.
    pub rows: Vec<(String, usize)>,
    /// Tables with findings beyond the shown rows.
    pub more: usize,
}

/// What to do about new unused findings under `--compare-root`: remove them,
/// or keep one a consumer ripbi cannot see reads (issue #151). Stderr and the
/// Markdown summary carry it; the gate is where authors meet the annotation.
pub const KEEP_HINT: &str = "Remove the new unused objects, or if something outside \
                            these reports reads one (an Excel pivot, another workspace's \
                            report), mark it in the model: `annotation ripbi_keep = <reason>`";

/// The human phrase for one broken binding's reason: what the engine would
/// render as an error state, said statically.
pub fn reason_phrase(binding: &BrokenOut) -> String {
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

/// Groups findings by their model table: where does the bloat concentrate.
/// Table-less findings (report measures, shared expressions, functions) belong to
/// no table and are skipped; relationships count under their "from" side. Count
/// descending, then table name folded (case-insensitively, like every other
/// ordering), so the output is deterministic across runs.
pub fn worst_tables(findings: &[Finding]) -> WorstTables {
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

/// Bytes in 1024-based units, the way VertiPaq Analyzer and DAX Studio show
/// model sizes: `512 B`, `1.2 KB`, `4.2 MB`, `1.0 GB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use ripbi_core::{NameKey, ObjectId};

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
            ObjectId::Bookmark {
                report_index: 0,
                bookmark: ripbi_core::identity::BookmarkKey {
                    name: NameKey::new("B1"),
                    display_name: Some("Saved view".to_string()),
                },
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
}
