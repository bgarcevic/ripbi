//! Integration tests for `ripbi scan` against the field-parameters PBIP
//! fixture (issue #52): parameter tables must never yield false positives,
//! whichever machinery binds them — the per-role `fieldParameters` arrays
//! real exports ship today, or the query-level `queryFieldParametersByRole`
//! map some exports use instead. The fixture's only legitimate findings are
//! the Legacy measure and the column it drags along.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::path::PathBuf;

use ripbi_cli::cli::ScanArgs;

use common::{field_parameters_pbip, json_payload, run_scan, scan_path};

fn fixture_args(path: impl Into<PathBuf>) -> ScanArgs {
    ScanArgs {
        path: Some(path.into()),
        ..ScanArgs::default()
    }
}

fn json_scan(name: &str) -> (i32, serde_json::Value, String) {
    let temp = common::TempDir::new(name);
    let args = ScanArgs {
        json: true,
        ..fixture_args(field_parameters_pbip().join("FieldParameters.pbip"))
    };
    let (code, stdout, stderr) = run_scan(&args, &temp.0, "");
    (code, json_payload(&stdout), stderr)
}

fn finding_ids(payload: &serde_json::Value) -> Vec<&str> {
    payload["unused"]
        .as_array()
        .expect("unused array")
        .iter()
        .map(|finding| finding["id"].as_str().expect("finding id"))
        .collect()
}

#[test]
fn a_parameter_column_bound_only_through_the_role_binding_stays_live() {
    let (code, payload, _stderr) = json_scan("fp-role");

    assert_eq!(code, 1, "only the Legacy chain is unused");
    let findings = finding_ids(&payload);
    assert_eq!(findings.len(), 2, "unexpected finding set: {findings:?}");
    assert!(findings.contains(&"'Sales'[Legacy Total]"));
    assert!(findings.contains(&"'Sales'[Legacy]"));
    for id in findings {
        assert!(
            !id.contains("Toggle"),
            "a parameter table object was flagged: {id}"
        );
    }
}

#[test]
fn the_parameter_expression_source_columns_stay_live() {
    let (_, payload, _stderr) = json_scan("fp-dax");

    // 'Sales'[Region] appears in no visual and no measure — only inside the
    // parameter partitions' NAMEOF() calls.
    let findings = finding_ids(&payload);
    assert!(
        !findings.contains(&"'Sales'[Region]"),
        "the parameter expression's source column was flagged: {findings:?}"
    );
}

#[test]
fn a_visual_bound_through_query_field_parameters_by_role_binds_without_notices() {
    let (code, payload, stderr) = json_scan("fp-by-role");

    // The slicer's Values role hangs off `queryFieldParametersByRole` alone —
    // a key real exports write and the binder must neither reject nor miss.
    assert_eq!(payload["skips"]["count"], 0, "no drift notices");
    assert!(
        !stderr.contains("queryFieldParametersByRole"),
        "the key was treated as drift:\n{stderr}"
    );
    let findings = finding_ids(&payload);
    for id in findings {
        assert!(
            !id.contains("Toggle for variance"),
            "the exclusively role-bound parameter table was flagged: {id}"
        );
    }
    assert_eq!(code, 1, "the Legacy chain still counts");
}

#[test]
fn human_output_reports_the_fixture_summary() {
    let temp = common::TempDir::new("fp-human");
    let (code, stdout, _stderr) = scan_path(
        &field_parameters_pbip().join("FieldParameters.pbip"),
        &temp.0,
    );

    assert_eq!(code, 1);
    assert!(
        stdout.contains("18 objects, 16 reachable from 5 roots, 2 unused"),
        "summary line missing:\n{stdout}"
    );
    assert!(
        !stdout.contains("Toggle"),
        "a live parameter table leaked into the findings:\n{stdout}"
    );
}
