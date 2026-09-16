//! Integration tests for `ripbi scan` against the auto date/time PBIP
//! fixture (issue #52): a visual date field bound through the auto date/time
//! machinery must never false-flag the generated `LocalDateTable_*` columns —
//! the machinery surfaces only through its verdicts (issue #47). The
//! fixture's only legitimate findings are the Legacy measure and the column
//! it drags along.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::path::PathBuf;

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, auto_datetime_pbip, json_payload, run_scan, scan_path};

const LOCAL_DATE_TABLE: &str = "table 'LocalDateTable_00000000-0000-0000-0000-000000000001'";

fn fixture_args(path: impl Into<PathBuf>) -> ScanArgs {
    ScanArgs {
        path: Some(path.into()),
        ..ScanArgs::default()
    }
}

fn finding_ids(payload: &serde_json::Value) -> Vec<&str> {
    payload["unused"]
        .as_array()
        .expect("unused array")
        .iter()
        .map(|finding| finding["id"].as_str().expect("finding id"))
        .collect()
}

/// Rewrites V2's axis to a plain `'Sales'[Date]` column binding — no
/// hierarchy, no variation — in a scratch copy of the fixture.
fn with_plain_date_binding(temp: &TempDir) -> PathBuf {
    temp.copy_tree(&auto_datetime_pbip());
    let visual = temp
        .0
        .join("AutoDateTime.Report/definition/pages/P1/visuals/V2/visual.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&visual).expect("read V2"))
            .expect("parse V2");
    value["visual"]["query"]["queryState"]["Axis"]["projections"][0] = serde_json::json!({
        "field": {
            "Column": {
                "Expression": {"SourceRef": {"Entity": "Sales"}},
                "Property": "Date"
            }
        },
        "queryRef": "Sales.Date",
        "active": true
    });
    std::fs::write(
        &visual,
        serde_json::to_string_pretty(&value).expect("serialize"),
    )
    .expect("write V2");
    temp.0.join("AutoDateTime.pbip")
}

#[test]
fn a_date_hierarchy_visual_keeps_the_machinery_alive_without_flagging_generated_columns() {
    let temp = TempDir::new("ad-hierarchy");
    let args = ScanArgs {
        json: true,
        ..fixture_args(auto_datetime_pbip().join("AutoDateTime.pbip"))
    };
    let (code, stdout, _stderr) = run_scan(&args, &temp.0, "");
    let payload = json_payload(&stdout);

    assert_eq!(code, 1, "only the Legacy chain is unused");

    // The machinery reports exactly one verdict — in use, over its varied
    // date column — with no finding of its own.
    let verdicts = payload["auto_date_time"].as_array().expect("verdict rows");
    assert_eq!(verdicts.len(), 1, "verdict rows: {verdicts:?}");
    assert_eq!(verdicts[0]["verdict"], "in_use");
    assert_eq!(verdicts[0]["id"], LOCAL_DATE_TABLE);
    assert_eq!(verdicts[0]["source_column"], "'Sales'[Date]");
    assert_eq!(
        verdicts[0]["finding"],
        serde_json::Value::Null,
        "an in-use table carries no finding"
    );

    // …and none of the generated columns, hierarchies, or partitions leak
    // into the generic findings.
    let findings = finding_ids(&payload);
    assert_eq!(
        findings,
        ["'Sales'[Legacy]", "'Sales'[Legacy Total]"],
        "unexpected finding set: {findings:?}"
    );
    assert_eq!(
        payload["summary"]["auto_date_time"],
        serde_json::json!({
            "hidden_tables": 1,
            "date_columns": 1,
            "member_findings": 0,
            "in_use": 1,
            "unused_by_reports": 0,
            "dead": 0,
        })
    );
}

#[test]
fn the_human_output_surfaces_only_the_verdict_row_for_the_machinery() {
    let temp = TempDir::new("ad-human");
    let (code, stdout, _stderr) =
        scan_path(&auto_datetime_pbip().join("AutoDateTime.pbip"), &temp.0);

    assert_eq!(code, 1);
    assert!(
        stdout.contains("Auto date/time (1)"),
        "verdict section missing:\n{stdout}"
    );
    assert!(
        stdout.contains("in use — replace with a real date table:"),
        "in-use grouping missing:\n{stdout}"
    );
    assert!(
        stdout.contains(LOCAL_DATE_TABLE),
        "the machinery's verdict row is missing:\n{stdout}"
    );
}

#[test]
fn a_plain_date_column_binding_reports_the_machinery_through_its_verdict() {
    let temp = TempDir::new("ad-plain");
    let args = ScanArgs {
        json: true,
        ..fixture_args(with_plain_date_binding(&temp))
    };
    let (code, stdout, _stderr) = run_scan(&args, &temp.0, "");
    let payload = json_payload(&stdout);

    assert_eq!(code, 1, "the Legacy chain and the dead table still count");

    // Unreachable by reports (the engine relationship carries no report
    // provenance), the machinery gets a deliberate dead verdict with its own
    // finding nested in the row.
    let verdicts = payload["auto_date_time"].as_array().expect("verdict rows");
    assert_eq!(verdicts.len(), 1, "verdict rows: {verdicts:?}");
    assert_eq!(verdicts[0]["verdict"], "dead");
    assert_eq!(verdicts[0]["id"], LOCAL_DATE_TABLE);
    assert_eq!(verdicts[0]["source_column"], "'Sales'[Date]");
    assert_eq!(
        verdicts[0]["finding"]["id"], LOCAL_DATE_TABLE,
        "the dead table's finding rides inside its verdict row"
    );

    // …while its members are covered by the verdict, never standalone
    // findings.
    let findings = finding_ids(&payload);
    assert_eq!(
        findings,
        ["'Sales'[Legacy]", "'Sales'[Legacy Total]"],
        "unexpected finding set: {findings:?}"
    );
    let summary = &payload["summary"]["auto_date_time"];
    assert_eq!(summary["member_findings"], 5, "all members covered");
    assert_eq!(summary["dead"], 1);
}
