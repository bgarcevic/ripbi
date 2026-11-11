//! Integration tests for duplicate-measure detection in `ripbi scan`: measures
//! whose DAX differs only in layout are listed as advice — in every output
//! mode, never in the finding count, never in the exit code.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, json_payload, mini_pbip, run_scan};

/// The mini project plus `Total Copy`, which repeats `Total` with different
/// layout, casing, quoting, and a comment.
fn project(name: &str) -> TempDir {
    let temp = TempDir::new(name);
    temp.copy_tree(&mini_pbip());
    let table = temp
        .0
        .join("Mini.SemanticModel/definition/tables/Sales.tmdl");
    let source = std::fs::read_to_string(&table).expect("read table");
    std::fs::write(
        &table,
        source.replace(
            "column Amount",
            "measure 'Total Copy' = sum ( Sales[AMOUNT] ) -- same thing\n\n\tcolumn Amount",
        ),
    )
    .expect("add the duplicate");
    temp
}

fn args(temp: &TempDir) -> ScanArgs {
    ScanArgs {
        path: Some(temp.0.join("Mini.pbip")),
        ..ScanArgs::default()
    }
}

#[test]
fn a_measure_repeating_another_is_listed_with_the_shared_expression() {
    let temp = project("duplicates-human");
    let (_, stdout, _) = run_scan(&args(&temp), &temp.0, "");
    assert!(
        stdout.contains(
            "Duplicate measures (1)\n  'Sales'[Total]\n    = 'Sales'[Total Copy]\n    \
             ← SUM('Sales'[Amount])\n"
        ),
        "{stdout}"
    );
}

#[test]
fn duplicates_are_advice_not_findings() {
    let baseline = TempDir::new("duplicates-json-baseline");
    baseline.copy_tree(&mini_pbip());
    let (baseline_code, baseline_out, _) = run_scan(
        &ScanArgs {
            json: true,
            ..args(&baseline)
        },
        &baseline.0,
        "",
    );
    let before = json_payload(&baseline_out);
    assert_eq!(before["summary"]["duplicates"], 0);

    let temp = project("duplicates-json");
    let (code, stdout, _) = run_scan(
        &ScanArgs {
            json: true,
            ..args(&temp)
        },
        &temp.0,
        "",
    );
    let payload = json_payload(&stdout);
    assert_eq!(code, baseline_code);
    assert_eq!(payload["summary"]["duplicates"], 1);
    assert_eq!(payload["summary"]["duplicates_total"], 1);
    let members = &payload["duplicates"][0]["members"];
    assert_eq!(members[0]["id"], "'Sales'[Total]");
    assert_eq!(members[1]["id"], "'Sales'[Total Copy]");
    assert_eq!(members[1]["type"], "measure");
    assert_eq!(
        payload["duplicates"][0]["expression"],
        "SUM('Sales'[Amount])"
    );
    // The copy is one more unused measure; the group itself adds nothing.
    assert_eq!(
        payload["summary"]["findings"].as_u64().unwrap(),
        before["summary"]["findings"].as_u64().unwrap() + 1
    );
}

#[test]
fn a_type_selection_hides_duplicates_but_still_counts_them() {
    let temp = project("duplicates-type");
    let (_, stdout, _) = run_scan(
        &ScanArgs {
            json: true,
            types: vec!["column".to_string()],
            ..args(&temp)
        },
        &temp.0,
        "",
    );
    let payload = json_payload(&stdout);
    assert_eq!(payload["summary"]["duplicates"], 0);
    assert_eq!(payload["summary"]["duplicates_total"], 1);
}

#[test]
fn plain_writes_one_record_per_group() {
    let temp = project("duplicates-plain");
    let (_, stdout, _) = run_scan(
        &ScanArgs {
            plain: true,
            ..args(&temp)
        },
        &temp.0,
        "",
    );
    assert!(
        stdout.contains("duplicate_measure\t'Sales'[Total]\t'Sales'[Total Copy]\n"),
        "{stdout}"
    );
}

#[test]
fn an_ignored_measure_leaves_no_group_behind() {
    let temp = project("duplicates-ignore");
    temp.write("ripbi.toml", "[scan]\nignore = [\"*Total Copy*\"]\n");
    let (_, stdout, _) = run_scan(
        &ScanArgs {
            json: true,
            ..args(&temp)
        },
        &temp.0,
        "",
    );
    assert_eq!(json_payload(&stdout)["summary"]["duplicates"], 0);
}

#[test]
fn a_group_the_compared_checkout_already_had_is_not_listed_again() {
    let before = project("duplicates-compare-before");
    let after = project("duplicates-compare-after");
    let (_, stdout, stderr) = run_scan(
        &ScanArgs {
            json: true,
            compare_root: Some(before.0.clone()),
            ..args(&after)
        },
        &after.0,
        "",
    );
    let payload = json_payload(&stdout);
    assert_eq!(payload["summary"]["duplicates"], 0, "{stderr}");
    assert_eq!(payload["summary"]["duplicates_total"], 1);
}
