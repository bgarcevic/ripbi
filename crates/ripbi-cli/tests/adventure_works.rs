//! The baseline validation: `ripbi scan` on the committed Adventure Works
//! sample must find every object the baseline marks dead, with the same chain
//! shape, must never flag an object the baseline marks live, and may only
//! exceed it by the documented field-parameter delta.
//!
//! Ground truth: `target/adventure-works-unused-analysis.csv`, distilled into
//! `tests/fixtures/adventure-works-baseline.txt` (see its header for the
//! format). The original analysis exports stay in `target/`, which is
//! gitignored — the distilled baseline is the committed contract.

#[allow(dead_code)]
mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, run_scan};

/// The committed sample project, present in the repository.
fn sample_pbip() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/AdventureWorks Sales.pbip")
}

/// Objects the baseline marks live through binding paths that are easy to
/// miss; if any of these is ever flagged, an ingest or reachability rule
/// regressed.
const LIVE_GUARDS: &[&str] = &[
    "'Date'[Latest year]",         // conditional-formatting FillRule input
    "'Sales'[Customers %]",        // tooltip field well
    "'Sales'[Orders]",             // visual-level Advanced filter
    "'Reseller'[Business Type]",   // visual-level Categorical filter
    "'Date Role'[Date Role]",      // advanced slicer over the calc group
    "'Date'[Year]",                // lineChart Series
    "'Date'[Mth]",                 // lineChart Category
    "'Date'[Date]",                // slicer + sort-by + calendar
    "'Product'[Category]",         // donutChart + relationship + sort-by
    "'Product'[Subcategory]",      // Products hierarchy (live)
    "'Product'[Model]",            // Products hierarchy (live)
    "'Sales'[Sales]",              // the core measure everything references
    "'Sales'[Profit]",             // cardVisual
    "'Sales'[Cost]",               // funnel Y-axis
    "'Sales'[Products]",           // cardVisual
    "'Sales'[Customers]",          // cardVisual
    "'Sales'[Total Product Cost]", // Cost measure's DAX
    "'Sales'[Sales Amount]",       // Sales measure's DAX
    "'Sales Order'[Sales Order]",  // Orders measure's DAX (source table column)
    "'Sales Territory'[Group]",    // clusteredBarChart
    "'Sales Territory'[Region]",   // clusteredBarChart
    "'Sales Order'[Channel]",      // clusteredColumnChart
    "'Customer'[Country-Region]",  // azureMap Category
    "'Customer'[CustomerKey]",     // relationship key (weak liveness)
    "'Product'[Sorting]",          // sort-by column for Category (live)
    "'Category'[Sorting]",         // used by Product's sort-by (live)
];

/// The one accepted static-analysis delta, documented in docs/output.md:
/// the dead 'Time Intelligence' field-parameter cluster. ripbi reports it,
/// because nothing anywhere references it — not even a single binding.
fn is_expected_extra(kind: &str, id: &str) -> bool {
    id.contains("'Time Intelligence'")
        && matches!(
            kind,
            "table" | "partition" | "calculation_item" | "expression"
        )
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
            .join("tests/fixtures/adventure-works-baseline.txt"),
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
    assert_eq!(baseline.len(), 44, "the distilled baseline is complete");

    let temp = TempDir::new("aw-baseline");
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

    // 3. The findings are exactly baseline + the documented TI cluster.
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
