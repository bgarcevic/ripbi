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

/// Objects the export marks dead that ripbi agrees are dead, but annotates
/// with the inactive relationship that names them: an inactive relationship
/// confers no liveness — only a live `USERELATIONSHIP` reference can activate
/// it — so its key columns surface as findings pointing back at it.
const INACTIVE_RELATIONSHIP_FINDINGS: &[&str] = &[
    "'Cases'[SystemUserSeq]",
    "'Opportunities'[SystemUserSeq]",
    "'Owners'[SystemUserSeq]",
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
    // The variation-bound machinery: the report's date hierarchy resolves
    // through 'Opportunity Calendar'[Date]'s variation declaration onto
    // LocalDateTable_9e0bbdfc-…, keeping the whole table alive.
    "'LocalDateTable_9e0bbdfc-9803-41d0-b204-481ce398f228'[Date]",
    // Plain visual and page-filter usage.
    "'Opportunities'[Status]",
    "'Opportunities'[Purchase Process]",
    "'Territories'[Territory]",
    "'Territories'[Region]",
];

/// The only findings allowed beyond the baseline: the engine-generated auto
/// date/time machinery of the five date columns whose hierarchies no visual
/// binds (the tables and partitions; the export lists no table rows), the
/// stale-bookmark cascade on the 'Cases' and 'Case Calendar' tables, the two
/// unactivated inactive relationships and their SystemUserSeq key columns, and
/// the orphaned `Query1` expression. The one bound table
/// (`LocalDateTable_9e0bbdfc-…`) is fully live and its columns are gone from
/// the findings entirely.
fn is_expected_extra(kind: &str, id: &str) -> bool {
    let machinery_table = matches!(kind, "table" | "partition")
        && (id.contains("LocalDateTable")
            || id.contains("DateTableTemplate")
            || id.contains("'Contacts'")
            || id.contains("'Opportunity Forecast Adjustment'"));
    // With the deleted pages' saved filters no longer binding (issue #48),
    // every remaining consumer of 'Cases' and 'Case Calendar' is itself
    // unused: the tables and their partitions fall to the containment rule,
    // and with them the active relationship between the two and the columns
    // it and the calculated-table partition were the last consumers of.
    let stale_bookmark_cascade = matches!(kind, "table" | "partition")
        && (id.contains("'Cases'") || id.contains("'Case Calendar'"))
        || kind == "relationship"
            && id.contains("'Cases'[Case Created On] -> 'Case Calendar'[Date]")
        || kind == "column" && (id == "'Case Calendar'[Date]" || id == "'Cases'[Case Created On]");
    let inactive_relationship =
        id.contains("SystemUserSeq") && matches!(kind, "column" | "relationship");
    let orphaned_expression = kind == "expression" && id.contains("'Query1'");
    machinery_table || stale_bookmark_cascade || inactive_relationship || orphaned_expression
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
    assert_eq!(baseline.len(), 144, "the distilled baseline is complete");

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

    // 3. The inactive-relationship keys follow the standard chain shape: the
    //     relationship naming them is itself an unactivated finding, so the
    //     keys read `only used by … (also unused)`.
    for id in INACTIVE_RELATIONSHIP_FINDINGS {
        let finding = by_id.get(id).unwrap_or_else(|| {
            panic!("ripbi stopped flagging '{id}': the inactive relationship that names it must not keep it alive")
        });
        let used_by = finding["used_by"].as_array().expect("used_by");
        assert!(
            !used_by.is_empty()
                && used_by.iter().all(|used| {
                    used["also_unused"] == true
                        && used["provenance"] == "inactive relationship endpoint"
                }),
            "{id}: expected only the unactivated relationship as an also-unused reference: {used_by:?}"
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

    // 5. The auto date/time verdicts: the one bound table is in use and fully
    //    live; the machinery of the five unbound date columns is dead — and
    //    its tables' own findings moved out of the generic list into the
    //    section, where the verdict and the dead chain read together.
    let auto = payload["auto_date_time"].as_array().expect("auto array");
    let by_table: HashMap<&str, &serde_json::Value> = auto
        .iter()
        .map(|row| (row["id"].as_str().expect("id"), row))
        .collect();
    assert_eq!(
        auto.len(),
        6,
        "every flagged table gets exactly one verdict"
    );

    let bound = by_table
        .get("table 'LocalDateTable_9e0bbdfc-9803-41d0-b204-481ce398f228'")
        .expect("the bound table has a verdict");
    assert_eq!(bound["verdict"], "in_use");
    assert_eq!(
        bound["source_column"], "'Opportunity Calendar'[Date]",
        "the verdict names the varied column the machinery serves"
    );
    assert!(
        !by_id.contains_key("table 'LocalDateTable_9e0bbdfc-9803-41d0-b204-481ce398f228'"),
        "an in-use table is alive, so it has no generic finding to move"
    );

    for unbound in [
        "table 'DateTableTemplate_0039983e-de71-45fb-bd88-812f61c0ff38'",
        "table 'LocalDateTable_16a9f7af-fed3-4e54-9b5a-15d1cdfee444'",
        "table 'LocalDateTable_36e7cc16-9aa7-44be-8b25-7a2fa51c55d8'",
        "table 'LocalDateTable_b0573d09-ef3e-45e2-9f39-8a331c91c6c3'",
        "table 'LocalDateTable_de73616c-e116-4a69-92e1-907ce2a4d5db'",
    ] {
        let row = by_table.get(unbound).unwrap_or_else(|| {
            panic!("'{unbound}' has a verdict — it is the section that flags the bloat")
        });
        assert_eq!(row["verdict"], "dead", "{unbound}: nothing binds it");
        if unbound.contains("DateTableTemplate") {
            // The template relates to no user column, so it has none to name.
            assert!(
                row["source_column"].is_null(),
                "{unbound}: no source column"
            );
        } else {
            let source = row["source_column"].as_str().unwrap_or_else(|| {
                panic!("{unbound}: the varied user column is the verdict's context")
            });
            assert!(
                source.starts_with("'Opportunity Calendar'[")
                    || source.starts_with("'Opportunities'["),
                "{unbound}: the source column is the varied user column, got {source}"
            );
        }
        let finding = &row["finding"];
        assert!(
            finding.is_object(),
            "{unbound}: the dead table's own finding lives in the section, chain included"
        );
        assert_eq!(
            finding["type"], "table",
            "{unbound}: the moved finding is the table row"
        );
        assert!(
            !by_id.contains_key(unbound),
            "{unbound}: the row is section-owned, not a generic finding"
        );
    }
}

/// This sample's five dead auto date/time tables normally force exit 1. The
/// type flags hide the whole section unless `--tables` is among them, and a
/// hidden section cannot fail the run — with no findings left, the exit code
/// is clean even though five dead tables exist.
#[test]
fn the_auto_datetime_section_follows_the_tables_flag() {
    let sample = sample_pbip();
    let temp = TempDir::new("ai-section");

    let measures = ScanArgs {
        json: true,
        measures: true,
        path: Some(sample.clone()),
        ..ScanArgs::default()
    };
    let (code, stdout, _) = run_scan(&measures, &temp.0, "");
    assert_eq!(code, 1, "the sample has 17 unused measures");
    let payload: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert!(
        payload["auto_date_time"]
            .as_array()
            .expect("auto array")
            .is_empty(),
        "without --tables the section is hidden entirely"
    );
    assert_eq!(
        payload["summary"]["auto_date_time"]["dead"], 0,
        "the hidden section's counts are zero too"
    );
    assert_eq!(
        payload["summary"]["unused"], 17,
        "only the selected measures are reported"
    );
    assert_eq!(
        payload["summary"]["unused_total"], 171,
        "the model-wide count is unfiltered, dead tables included"
    );

    let tables = ScanArgs {
        json: true,
        tables: true,
        path: Some(sample),
        ..ScanArgs::default()
    };
    let (code, stdout, _) = run_scan(&tables, &temp.0, "");
    assert_eq!(
        code, 1,
        "--tables keeps the section and its exit-code weight"
    );
    let payload: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let rows = payload["auto_date_time"].as_array().expect("auto array");
    assert_eq!(rows.len(), 6, "--tables restores all six verdict rows");
    assert_eq!(payload["summary"]["auto_date_time"]["in_use"], 1);
    assert_eq!(
        payload["summary"]["auto_date_time"]["dead"], 5,
        "the dead verdicts gate the exit code again"
    );
}
