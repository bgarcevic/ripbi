//! Integration tests for PBIR `NativeMeasure` bindings: report-scoped DAX
//! written inline where a field would be. The fixture binds three: V1's card
//! projects one over `ExampleTable` and `[Example Total]` and colors its
//! labels with another over `[Example Target]`; V2's table projects and sorts
//! by one over `ExampleBudget[Budget]`. Every model object those expressions
//! name stays reachable, a name the model lacks is a `broken_visual`
//! finding, and an expression ripbi cannot read is a strict-fatal skip.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, json_payload, run_scan};

const V1: &str = "Native.Report/definition/pages/P1/visuals/V1/visual.json";
const V2: &str = "Native.Report/definition/pages/P1/visuals/V2/visual.json";

/// V1's projected expression, as the fixture writes it inside the JSON string.
const V1_TAIL: &str = "_rows[Amount]), [Example Total])";

fn native_measure_pbip() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("native-measure-pbip")
}

/// Rewrites one file under `root`, replacing `from` with `to` exactly once.
fn edit(root: &Path, relative: &str, from: &str, to: &str) {
    let path = root.join(relative);
    let text = fs::read_to_string(&path).expect("read fixture file");
    assert_eq!(text.matches(from).count(), 1, "{from:?} in {relative}");
    fs::write(&path, text.replace(from, to)).expect("write fixture file");
}

fn scan_args(root: &Path) -> ScanArgs {
    ScanArgs {
        json: true,
        path: Some(root.join("Native.pbip")),
        ..ScanArgs::default()
    }
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("read fixture") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            fs::create_dir_all(&target).expect("mkdir");
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

fn copy(name: &str) -> TempDir {
    let temp = TempDir::new(name);
    temp.copy_tree(&native_measure_pbip());
    temp
}

fn targets(payload: &serde_json::Value, key: &str, field: &str) -> Vec<String> {
    payload[key]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row[field].as_str().expect("string").to_string())
        .collect()
}

#[test]
fn native_measure_dependencies_stay_reachable_under_strict_broken() {
    let temp = copy("native-valid");
    let args = ScanArgs {
        broken: true,
        strict: true,
        ..scan_args(&temp.0)
    };
    let (code, stdout, stderr) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 0, "valid native measures pass:\n{stdout}\n{stderr}");
    let payload = json_payload(&stdout);
    assert_eq!(payload["skips"]["count"], 0, "{stdout}");
    assert_eq!(payload["summary"]["broken_total"], 0, "{stdout}");

    let (_, stdout, _) = run_scan(&scan_args(&temp.0), &temp.0, "");
    let payload = json_payload(&stdout);
    assert_eq!(
        targets(&payload, "unused", "id"),
        vec!["'ExampleTable'[Example Unused]"],
        "only the measure nothing names is dead:\n{stdout}"
    );
}

#[test]
fn missing_model_references_in_a_native_measure_are_broken_visuals() {
    let temp = copy("native-missing");
    edit(
        &temp.0,
        V1,
        V1_TAIL,
        "_rows[Amount]), [Missing Total]) + SUM(ExampleTable[Gone]) + SUM(MissingTable[Amount])",
    );
    let args = ScanArgs {
        broken: true,
        ..scan_args(&temp.0)
    };
    let (code, stdout, _) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 1, "breakage gates under --broken:\n{stdout}");
    let payload = json_payload(&stdout);
    let rows: Vec<(String, String)> = payload["broken"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| {
            (
                row["target"].as_str().expect("target").to_string(),
                row["reason"].as_str().expect("reason").to_string(),
            )
        })
        .collect();
    for expected in [
        ("[Missing Total]", "measure_not_found"),
        ("'ExampleTable'[Gone]", "field_not_found"),
        ("'MissingTable'[Amount]", "table_not_found"),
    ] {
        assert!(
            rows.contains(&(expected.0.to_string(), expected.1.to_string())),
            "{expected:?} in {rows:?}"
        );
    }
    assert_eq!(rows.len(), 3, "valid references stay unflagged: {rows:?}");
}

#[test]
fn a_native_measure_on_a_broken_model_measure_inherits_it() {
    let temp = copy("native-inherited");
    edit(
        &temp.0,
        "Native.SemanticModel/definition/tables/ExampleTable.tmdl",
        "measure 'Example Target' = 100",
        "measure 'Example Target' = SUM(ExampleTable[Gone])",
    );
    let (_, stdout, _) = run_scan(&scan_args(&temp.0), &temp.0, "");
    let payload = json_payload(&stdout);
    assert_eq!(
        targets(&payload, "broken", "bound_artifact"),
        vec!["'ExampleTable'[Example Target]"],
        "the formatting expression lands on the broken measure:\n{stdout}"
    );
}

#[test]
fn a_malformed_native_measure_is_a_strict_fatal_skip() {
    for (name, relative, from, to, message) in [
        (
            "native-unbalanced",
            V2,
            "\"SUM('ExampleBudget'[Budget]) - SUM(ExampleTable[Amount])\",\n                  \"DataType\": 3",
            "\"SUM('ExampleBudget'[Budget] - SUM(ExampleTable[Amount])\",\n                  \"DataType\": 3",
            "not well-formed DAX: unbalanced parentheses",
        ),
        (
            "native-language",
            V1,
            "\"Language\": \"dax\",\n                  \"Expression\": \"VAR",
            "\"Language\": \"mdx\",\n                  \"Expression\": \"VAR",
            "is not DAX",
        ),
        (
            "native-no-expression",
            V1,
            "\"Expression\": \"IF(",
            "\"Formula\": \"IF(",
            "NativeMeasure carries no Expression string",
        ),
    ] {
        let temp = copy(name);
        edit(&temp.0, relative, from, to);
        let (_, stdout, _) = run_scan(&scan_args(&temp.0), &temp.0, "");
        let payload = json_payload(&stdout);
        let notices = payload["skips"]["notices"].as_array().expect("notices");
        assert_eq!(notices.len(), 1, "{name}: {stdout}");
        assert_eq!(notices[0]["kind"], "malformed_value", "{name}: {stdout}");
        assert!(
            notices[0]["detail"]
                .as_str()
                .expect("message")
                .contains(message),
            "{name}: {stdout}"
        );

        let args = ScanArgs {
            broken: true,
            strict: true,
            ..scan_args(&temp.0)
        };
        let (code, _, _) = run_scan(&args, &temp.0, "");
        assert_eq!(code, 2, "{name}: the skip fails --strict");
    }
}

/// `--compare-root` against a baseline that carries the same native measures:
/// nothing is new, so the strict broken gate passes; a reference the change
/// breaks is new and gates.
#[test]
fn native_measures_compare_against_a_baseline() {
    let temp = TempDir::new("native-compare");
    for side in ["base", "head"] {
        copy_dir(&native_measure_pbip(), &temp.mkdir(side));
    }
    let head = temp.0.join("head");
    let args = || ScanArgs {
        json: true,
        broken: true,
        strict: true,
        compare_root: Some(temp.0.join("base")),
        ..ScanArgs::default()
    };

    let (code, stdout, stderr) = run_scan(&args(), &head, "");
    assert_eq!(code, 0, "identical baseline:\n{stdout}\n{stderr}");

    edit(&head, V1, V1_TAIL, "_rows[Amount]), [Missing Total])");
    let (code, stdout, _) = run_scan(&args(), &head, "");
    assert_eq!(code, 1, "new breakage gates:\n{stdout}");
    let payload = json_payload(&stdout);
    assert_eq!(
        targets(&payload, "broken", "target"),
        vec!["[Missing Total]"],
        "{stdout}"
    );

    // The same breakage already in the baseline is no longer new.
    edit(
        &temp.0.join("base"),
        V1,
        V1_TAIL,
        "_rows[Amount]), [Missing Total])",
    );
    let (code, stdout, _) = run_scan(&args(), &head, "");
    assert_eq!(code, 0, "baseline breakage does not gate:\n{stdout}");
}
