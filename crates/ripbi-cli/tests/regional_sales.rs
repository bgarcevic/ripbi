//! The baseline validation for the Regional Sales sample: `ripbi scan` must
//! find every object the committed baseline marks dead, with the same chain
//! shape, must never flag an object the baseline marks live, and may only
//! exceed it by the documented fully-dead-table delta.
//!
//! This sample's report is the largest in the repository (147 visuals), and
//! its bindings exercise filter kinds the Adventure Works report does not
//! (TopN, Exclude, key-influencers and decomposition-tree visuals, page-level
//! filters) plus a measure referenced only through a slicer's accessibility
//! alt text.
//!
//! Ground truth: `target/regional-sales-unused-analysis.csv`, distilled into
//! `tests/fixtures/regional-sales-baseline.txt` (see its header for the
//! format). The original analysis export stays in `target/`, which is
//! gitignored — the distilled baseline is the committed contract.

#[allow(dead_code)]
mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, run_scan};

/// The committed sample project, present in the repository.
fn sample_pbip() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/Regional Sales Sample.pbip")
}

/// Objects the baseline marks live through binding paths that are easy to
/// miss; if any of these is ever flagged, an ingest or reachability rule
/// regressed.
const LIVE_GUARDS: &[&str] = &[
    // A slicer's accessibility alt text interpolates this measure; a screen
    // reader reads it, so it is live with a single, unusual use.
    "'Calculations'[Forecast Adjustment Slicer Alt Text]",
    // A relationship endpoint: weakly live, and the one use that does NOT keep
    // its (fully dead) table alive.
    "'Contacts'[AccountSeq]",
    // Page-level Advanced filters.
    "'Opportunity Calendar'[RELATIVE MONTH]",
    "'Industries'[IndustrySeq]",
    // TopN visual filters.
    "'Calculations'[Revenue Won]",
    "'Calculations'[Revenue Open]",
    // Exclude filter on a decomposition tree.
    "'Campaigns'[Campaign]",
    // Categorical filters on key-influencers and funnel visuals.
    "'Opportunities'[Status]",
    "'Opportunities'[Purchase Process]",
    "'Owners'[Manager]",
    // Measure filters (Advanced) on visuals.
    "'Calculations'[Rev Goal]",
    "'Calculations'[Close %]",
];

/// The one accepted delta, documented in the baseline fixture header: the
/// fully-dead 'Contacts' table and its partition. The external export lists no
/// table rows at all, but with every non-key Contacts column dead and only a
/// relationship referencing the table, the table is unused by the documented
/// containment rule — and the partition dies with it.
fn is_expected_extra(kind: &str, id: &str) -> bool {
    matches!(kind, "table" | "partition") && id.contains("'Contacts'")
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
            .join("tests/fixtures/regional-sales-baseline.txt"),
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
    assert_eq!(baseline.len(), 25, "the distilled baseline is complete");

    let temp = TempDir::new("rs-baseline");
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

    // 2. No baseline-live object is flagged — the false-positive guard.
    for guard in LIVE_GUARDS {
        assert!(
            !by_id.contains_key(guard),
            "ripbi flags '{guard}' but the baseline proves it live"
        );
    }

    // 3. The findings are exactly baseline + the documented dead table.
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
