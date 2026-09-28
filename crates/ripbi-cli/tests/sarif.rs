//! Integration tests for `scan --sarif` (issue #142): the log validates
//! against the SARIF 2.1.0 schema, points at the declaring `.tmdl` line (or
//! the model file), is byte-for-byte deterministic, and under
//! `--compare-root` keeps existing findings as suppressed results.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use ripbi_cli::Cli;
use ripbi_cli::cli::ScanArgs;
use serde_json::Value;

use common::{TempDir, auto_datetime_pbip, broken_visual_pbip, json_payload, mini_pbip, run_scan};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Fails with every schema violation listed.
fn assert_valid(log: &Value) {
    let text = fs::read_to_string(fixtures().join("sarif-schema-2.1.0.json")).expect("schema");
    let schema: Value = serde_json::from_str(&text).expect("schema is JSON");
    let validator = jsonschema::draft4::new(&schema).expect("schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(log)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "SARIF schema violations:\n{}",
        errors.join("\n")
    );
}

fn sarif() -> ScanArgs {
    ScanArgs {
        sarif: true,
        ..ScanArgs::default()
    }
}

fn results(log: &Value) -> &Vec<Value> {
    log["runs"][0]["results"].as_array().expect("results")
}

fn by_name<'a>(log: &'a Value, name: &str) -> &'a Value {
    results(log)
        .iter()
        .find(|result| result["locations"][0]["logicalLocations"][0]["fullyQualifiedName"] == name)
        .unwrap_or_else(|| panic!("no result for {name}"))
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("mkdir");
    for entry in fs::read_dir(from).expect("read fixture") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

#[test]
fn mini_pbip_matches_the_golden_log() {
    let temp = TempDir::new("sarif-golden");
    temp.copy_tree(&mini_pbip());

    let (code, stdout, stderr) = run_scan(&sarif(), &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    assert_valid(&json_payload(&stdout));

    // The tool version is the only release-dependent byte.
    let actual = stdout.replace(
        &format!("\"version\": \"{}\"", env!("CARGO_PKG_VERSION")),
        "\"version\": \"<version>\"",
    );
    let golden = fixtures().join("sarif/mini.sarif");
    if std::env::var_os("RIPBI_BLESS").is_some() {
        fs::write(&golden, &actual).expect("write golden");
    }
    let expected = fs::read_to_string(&golden)
        .expect("golden log (RIPBI_BLESS=1 writes it)")
        .replace("\r\n", "\n");
    assert_eq!(actual, expected, "set RIPBI_BLESS=1 to accept the new log");
}

#[test]
fn tmdl_findings_point_at_their_declaration_line() {
    let temp = TempDir::new("sarif-lines");
    temp.copy_tree(&mini_pbip());

    let (_, stdout, _) = run_scan(&sarif(), &temp.0, "");
    let log = json_payload(&stdout);
    let result = by_name(&log, "'Sales'[Legacy Total]");
    assert_eq!(result["ruleId"], "RIPBI-UNUSED-MEASURE");
    let physical = &result["locations"][0]["physicalLocation"];
    assert_eq!(
        physical["artifactLocation"]["uri"],
        "Mini.SemanticModel/definition/tables/Sales.tmdl"
    );
    assert_eq!(physical["artifactLocation"]["uriBaseId"], "%SRCROOT%");
    let line = physical["region"]["startLine"].as_u64().expect("line") as usize;
    let text = fs::read_to_string(
        temp.0
            .join("Mini.SemanticModel/definition/tables/Sales.tmdl"),
    )
    .expect("read");
    let declared = text.lines().nth(line - 1).expect("line exists");
    assert!(
        declared.trim_start().starts_with("measure 'Legacy Total'"),
        "{declared}"
    );
}

#[test]
fn a_single_file_model_points_at_the_file_and_names_the_object() {
    let temp = TempDir::new("sarif-bim");
    let fixture = root().join("crates/ripbi-core/tests/fixtures");
    let args = ScanArgs {
        path: Some(fixture.join("tmsl/golden/model.bim")),
        reports: vec![fixture.join("pbir/golden/Mini.Report")],
        ..sarif()
    };
    let (code, stdout, stderr) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    let log = json_payload(&stdout);
    assert_valid(&log);
    let found = results(&log);
    assert!(!found.is_empty());
    for result in found {
        let location = &result["locations"][0];
        let uri = location["physicalLocation"]["artifactLocation"]["uri"]
            .as_str()
            .expect("uri");
        // Outside the working directory: an absolute file URI, no base id.
        assert!(uri.starts_with("file:///"), "{uri}");
        assert!(location["physicalLocation"]["artifactLocation"]["uriBaseId"].is_null());
        assert!(location["physicalLocation"]["region"].is_null());
        assert!(location["logicalLocations"][0]["fullyQualifiedName"].is_string());
    }
}

#[test]
fn broken_bindings_point_at_their_report() {
    let temp = TempDir::new("sarif-broken");
    temp.copy_tree(&broken_visual_pbip());

    let (_, stdout, stderr) = run_scan(&sarif(), &temp.0, "");
    let log = json_payload(&stdout);
    assert_valid(&log);
    let broken: Vec<&Value> = results(&log)
        .iter()
        .filter(|result| result["ruleId"] == "RIPBI-BROKEN-VISUAL")
        .collect();
    assert!(!broken.is_empty(), "{stdout}\n{stderr}");
    for result in broken {
        assert_eq!(result["level"], "error");
        let uri = &result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"];
        assert_eq!(uri, "Broken.Report/definition/report.json");
    }
}

#[test]
fn auto_date_time_verdicts_validate() {
    let temp = TempDir::new("sarif-auto");
    temp.copy_tree(&auto_datetime_pbip());
    // Bind the plain date column instead of its date hierarchy: the machinery
    // is then unused by reports.
    let visual = temp
        .0
        .join("AutoDateTime.Report/definition/pages/P1/visuals/V2/visual.json");
    let mut value: Value =
        serde_json::from_str(&fs::read_to_string(&visual).expect("read V2")).expect("parse V2");
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
    fs::write(
        &visual,
        serde_json::to_string_pretty(&value).expect("serialize"),
    )
    .expect("write");

    let (_, stdout, _) = run_scan(&sarif(), &temp.0, "");
    let log = json_payload(&stdout);
    assert_valid(&log);
    let row = results(&log)
        .iter()
        .find(|result| result["ruleId"] == "RIPBI-AUTO-DATE-TIME")
        .unwrap_or_else(|| panic!("{stdout}"));
    let uri = &row["locations"][0]["physicalLocation"]["artifactLocation"]["uri"];
    assert!(
        uri.as_str().is_some_and(|uri| uri.ends_with(".tmdl")),
        "{uri}"
    );
}

/// Under `--compare-root` a finding already in the other checkout is kept,
/// suppressed, with the same fingerprint the other checkout's own log gives
/// it; only the new finding is unsuppressed, and only it gates.
#[test]
fn compare_root_suppresses_existing_findings() {
    let temp = TempDir::new("sarif-compare");
    let (base, head) = (temp.0.join("base"), temp.0.join("head"));
    copy(&mini_pbip(), &base);
    copy(&mini_pbip(), &head);
    let sales = head.join("Mini.SemanticModel/definition/tables/Sales.tmdl");
    let text = fs::read_to_string(&sales).expect("read");
    fs::write(
        &sales,
        text.replacen(
            "\tcolumn Amount",
            "\tmeasure 'Draft KPI' = 1\n\t\tlineageTag: 99999999-9999-9999-9999-999999999990\n\n\tcolumn Amount",
            1,
        ),
    )
    .expect("write");

    let args = ScanArgs {
        compare_root: Some(PathBuf::from("../base")),
        ..sarif()
    };
    let (code, stdout, stderr) = run_scan(&args, &head, "");
    assert_eq!(code, 1, "{stderr}");
    let log = json_payload(&stdout);
    assert_valid(&log);

    let (suppressed, new): (Vec<&Value>, Vec<&Value>) = results(&log)
        .iter()
        .partition(|result| result.get("suppressions").is_some());
    assert_eq!(new.len(), 1, "{stdout}");
    assert_eq!(
        new[0]["locations"][0]["logicalLocations"][0]["fullyQualifiedName"],
        "'Sales'[Draft KPI]"
    );
    assert_eq!(suppressed.len(), 2, "{stdout}");
    for result in &suppressed {
        assert_eq!(result["suppressions"][0]["kind"], "external");
        assert_eq!(
            result["suppressions"][0]["justification"],
            "already in ../base"
        );
    }

    // The fingerprint matches the base checkout's own log.
    let (_, base_stdout, _) = run_scan(&sarif(), &base, "");
    let base_log = json_payload(&base_stdout);
    let fingerprint = |log: &Value| {
        by_name(log, "'Sales'[Legacy Total]")["partialFingerprints"]["ripbiFinding/v1"].clone()
    };
    assert_eq!(fingerprint(&log), fingerprint(&base_log));

    // Nothing new: everything suppressed, and the gate passes.
    let (code, stdout, _) = run_scan(&args, &base, "");
    assert_eq!(code, 0, "{stdout}");
    assert!(
        results(&json_payload(&stdout))
            .iter()
            .all(|result| result.get("suppressions").is_some())
    );
}

#[test]
fn sarif_is_its_own_output_mode() {
    for other in ["--json", "--plain", "--summary"] {
        assert!(
            Cli::try_parse_from(["ripbi", "scan", "--sarif", other]).is_err(),
            "--sarif with {other}"
        );
    }
}
