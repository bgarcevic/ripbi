//! The baseline validation for the Artificial Intelligence sample: `ripbi
//! scan` must find every object the committed baseline marks dead, with the
//! same chain shape, must never flag an object the export marks live, and may
//! only exceed it by the deltas documented in the fixture header.
//!
//! This is the one committed sample with the auto date/time feature enabled
//! (LocalDateTable_/DateTableTemplate_ machinery) and with AI-narrative
//! textboxes whose visual-calculation Transform steps consume aliased model
//! fields. The export's dead set is large (150 unique objects), so this test
//! is the main guard against both false positives and missed bindings.
//!
//! Ground truth: `target/artificial-intelligence-unused-analysis.csv`,
//! distilled into
//! `tests/fixtures/artificial-intelligence-baseline.txt` (see its header for
//! the format and the documented deltas). The original export stays in
//! `target/`, which is gitignored — the distilled baseline is the committed
//! contract.

#[allow(dead_code)]
mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, run_scan};

/// The committed sample project, present in the repository.
fn sample_pbip() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/Artificial Intelligence Sample.pbip")
}

/// Objects the export marks dead that ripbi deliberately keeps alive. Each
/// entry names the policy: bookmarks re-bind their saved filters when
/// re-applied, and an inactive relationship keeps both key columns alive as
/// long as either endpoint table is reachable (USERELATIONSHIP).
const POLICY_KEPT_ALIVE: &[(&str, &str)] = &[
    ("'Cases'[Subject]", "bookmark saved filters"),
    ("'Cases'[Agent]", "bookmark saved filters"),
    ("'Cases'[Origin]", "bookmark saved filters"),
    ("'Cases'[Severity]", "bookmark saved filters"),
    ("'Cases'[Is Escalated]", "bookmark saved filters"),
    ("'Cases'[Is SLA Violation]", "bookmark saved filters"),
    ("'Opportunities'[PipelineStep]", "bookmark saved filters"),
    (
        "'Opportunity Calendar'[RELATIVE MONTH]",
        "bookmark saved filters",
    ),
    (
        "'Case Calendar'[RELATIVE 30 DAY PERIOD]",
        "bookmark saved filters",
    ),
    ("'Cases'[SystemUserSeq]", "inactive relationship endpoint"),
    (
        "'Opportunities'[SystemUserSeq]",
        "inactive relationship endpoint",
    ),
    ("'Owners'[SystemUserSeq]", "inactive relationship endpoint"),
];

/// Objects the export marks live that ripbi proves live through binding paths
/// that are easy to miss; if any of these is ever flagged, an ingest or
/// reachability rule regressed.
const LIVE_GUARDS: &[&str] = &[
    // A smart-narrative textbox's Transform step consumes this column through
    // the subquery's alias scope.
    "'Opportunity Calendar'[YEAR MONTH]",
    // The sort-by column of that date label: kept alive transitively.
    "'Opportunity Calendar'[YEAR MONTH NUMBER]",
    // Weak liveness: an auto date/time table's Date column stays alive through
    // its variation relationship, though its table does not.
    "'LocalDateTable_9e0bbdfc-9803-41d0-b204-481ce398f228'[Date]",
    // Plain visual and page-filter usage.
    "'Opportunities'[Status]",
    "'Opportunities'[Purchase Process]",
    "'Territories'[Territory]",
    "'Territories'[Region]",
];

/// The only findings allowed beyond the baseline: the engine-generated auto
/// date/time machinery (unused — the report reaches it only through the date
/// variations this crate does not model, a documented known gap), the four
/// LocalDateTable hierarchy columns the export counts as used through that
/// machinery, the fully-dead relationship-only tables with their partitions,
/// and the orphaned `Query1` expression.
fn is_expected_extra(kind: &str, id: &str) -> bool {
    let machinery_table = matches!(kind, "table" | "partition")
        && (id.contains("LocalDateTable")
            || id.contains("DateTableTemplate")
            || id.contains("'Contacts'")
            || id.contains("'Opportunity Forecast Adjustment'"));
    let variation_gap_column = kind == "column" && id.contains("LocalDateTable");
    let orphaned_expression = kind == "expression" && id.contains("'Query1'");
    machinery_table || variation_gap_column || orphaned_expression
}

#[test]
fn scan_agrees_with_the_committed_baseline() {
    let sample = sample_pbip();
    assert!(
        sample.exists(),
        "the committed sample is missing: {}",
        sample.display()
    );

    let baseline_text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/artificial-intelligence-baseline.txt"),
    )
    .expect("read baseline");
    let baseline: Vec<(String, String, String, String)> = baseline_text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            (
                parts[0].to_string(),
                parts[1].to_string(),
                parts[2].to_string(),
                parts[3].to_string(),
            )
        })
        .collect();
    assert_eq!(baseline.len(), 138, "the distilled baseline is complete");

    let temp = TempDir::new("ai-baseline");
    let args = ScanArgs {
        json: true,
        path: Some(sample),
        ..ScanArgs::default()
    };
    let (code, stdout, _) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 1, "the sample has unused objects");

    let payload: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let findings = payload["unused"].as_array().expect("unused array");
    let by_id: HashMap<&str, &serde_json::Value> = findings
        .iter()
        .map(|finding| (finding["id"].as_str().expect("id"), finding))
        .collect();

    // 1. Every baseline-dead object is a ripbi finding with the matching chain.
    for (kind, table, name, chain) in &baseline {
        let id = match kind.as_str() {
            "column" | "measure" => format!("'{table}'[{name}]"),
            "hierarchy" => format!("hierarchy '{table}'[{name}]"),
            other => panic!("unhandled baseline type {other}"),
        };
        let finding = by_id.get(id.as_str()).unwrap_or_else(|| {
            panic!("the baseline marks '{id}' dead, but ripbi does not report it");
        });
        assert_eq!(
            finding["type"].as_str().expect("type"),
            kind.as_str(),
            "{id}: ripbi and the baseline disagree on the object type"
        );
        let used_by = finding["used_by"].as_array().expect("used_by");
        match chain.as_str() {
            "orphan" => assert!(
                used_by.is_empty(),
                "{id}: baseline says unused (0 uses); ripbi sees consumers: {used_by:?}"
            ),
            "chained" => assert!(
                !used_by.is_empty() && used_by.iter().all(|used| used["also_unused"] == true),
                "{id}: baseline says used by unused; ripbi's consumers must all be unused: {used_by:?}"
            ),
            other => panic!("unhandled chain class {other}"),
        }
    }

    // 2. No live object is flagged — the false-positive guard...
    for guard in LIVE_GUARDS {
        assert!(
            !by_id.contains_key(guard),
            "ripbi flags '{guard}' but the baseline proves it live"
        );
    }

    // 3. ...and the objects kept alive by documented policy stay unflagged.
    for (id, policy) in POLICY_KEPT_ALIVE {
        assert!(
            !by_id.contains_key(id),
            "ripbi flags '{id}': kept alive only by {policy}, which must count as usage"
        );
    }

    // 4. The findings are exactly baseline + the documented extras.
    let baseline_ids: Vec<String> = baseline
        .iter()
        .map(|(kind, table, name, _)| match kind.as_str() {
            "column" | "measure" => format!("'{table}'[{name}]"),
            "hierarchy" => format!("hierarchy '{table}'[{name}]"),
            other => panic!("unhandled baseline type {other}"),
        })
        .collect();
    let extras: Vec<&serde_json::Value> = findings
        .iter()
        .filter(|finding| {
            let id = (*finding)["id"].as_str().expect("id");
            !baseline_ids.iter().any(|baseline_id| baseline_id == id)
        })
        .collect();
    assert_eq!(
        extras.len(),
        findings.len() - baseline.len(),
        "every finding is baseline or a known extra"
    );
    for extra in &extras {
        let kind = (**extra)["type"].as_str().expect("type");
        let id = (**extra)["id"].as_str().expect("id");
        assert!(
            is_expected_extra(kind, id),
            "undocumented extra finding '{id}' ({kind}) — triage it against the baseline export"
        );
    }
}
