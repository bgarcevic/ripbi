//! Integration tests for `scan --sarif-file`, `--json-file`, and
//! `--markdown-file` (issue #143): one scan writes every CI output at once,
//! each file byte-identical to its stdout mode, whatever stdout shows.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, broken_visual_pbip, json_payload, mini_pbip, run_scan};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn side_files() -> ScanArgs {
    ScanArgs {
        sarif_file: Some(PathBuf::from("out.sarif")),
        json_file: Some(PathBuf::from("out.json")),
        markdown_file: Some(PathBuf::from("out.md")),
        ..ScanArgs::default()
    }
}

/// Compares `actual` with a golden file; `RIPBI_BLESS=1` rewrites it.
fn assert_golden(actual: &str, name: &str) {
    let golden = fixtures().join("markdown").join(name);
    if std::env::var_os("RIPBI_BLESS").is_some() {
        fs::create_dir_all(golden.parent().expect("parent")).expect("mkdir");
        fs::write(&golden, actual).expect("write golden");
    }
    let expected = read(&golden).replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "set RIPBI_BLESS=1 to accept the new summary"
    );
}

#[test]
fn side_files_match_their_stdout_modes() {
    let temp = TempDir::new("side-match");
    temp.copy_tree(&mini_pbip());

    let (code, human, stderr) = run_scan(&side_files(), &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    assert!(
        human.contains("Measures (1)"),
        "stdout stays human:\n{human}"
    );

    let (_, sarif, _) = run_scan(
        &ScanArgs {
            sarif: true,
            ..ScanArgs::default()
        },
        &temp.0,
        "",
    );
    let (_, json, _) = run_scan(
        &ScanArgs {
            json: true,
            ..ScanArgs::default()
        },
        &temp.0,
        "",
    );
    assert_eq!(read(&temp.0.join("out.sarif")), sarif);
    assert_eq!(read(&temp.0.join("out.json")), json);
    assert_golden(&read(&temp.0.join("out.md")), "mini.md");
}

/// `-q` silences the streams, never a file the user asked for.
#[test]
fn quiet_still_writes_the_files() {
    let temp = TempDir::new("side-quiet");
    temp.copy_tree(&mini_pbip());

    let (code, stdout, stderr) = run_scan(
        &ScanArgs {
            quiet: true,
            ..side_files()
        },
        &temp.0,
        "",
    );
    assert_eq!(code, 1);
    assert_eq!((stdout.as_str(), stderr.as_str()), ("", ""));
    for file in ["out.sarif", "out.json", "out.md"] {
        assert!(temp.0.join(file).is_file(), "{file} written");
    }
}

/// The CI shape: `--compare-root` against the base branch, SARIF keeping the
/// existing findings as suppressed results, JSON and Markdown counting only
/// the new one.
#[test]
fn compare_root_side_files_count_only_new_findings() {
    // Sibling checkouts, so the comparison root is the relative `../base` a
    // pipeline passes and the golden summary stays path-free.
    let temp = TempDir::new("side-compare");
    for side in ["base", "head"] {
        copy(&mini_pbip(), &temp.mkdir(side));
    }
    let sales = temp
        .0
        .join("head/Mini.SemanticModel/definition/tables/Sales.tmdl");
    let text = read(&sales);
    fs::write(
        &sales,
        text.replacen(
            "\tcolumn Amount",
            "\tmeasure 'Draft KPI' = 1\n\t\tlineageTag: 99999999-9999-9999-9999-999999999990\n\n\tcolumn Amount",
            1,
        ),
    )
    .expect("edit");
    let head = temp.0.join("head");

    let (code, _, stderr) = run_scan(
        &ScanArgs {
            compare_root: Some(PathBuf::from("../base")),
            quiet: true,
            ..side_files()
        },
        &head,
        "",
    );
    assert_eq!(code, 1, "{stderr}");

    let json = json_payload(&read(&head.join("out.json")));
    assert_eq!(json["summary"]["findings"], 1);
    assert_eq!(json["compare"]["existing"], 2);

    let sarif = json_payload(&read(&head.join("out.sarif")));
    let results = sarif["runs"][0]["results"].as_array().expect("results");
    let suppressed = results
        .iter()
        .filter(|result| result.get("suppressions").is_some())
        .count();
    assert_eq!((results.len(), suppressed), (3, 2));

    assert_golden(&read(&head.join("out.md")), "mini-compare.md");
}

/// Breakage and auto date/time rows count as findings too: `summary.findings`
/// is the unsuppressed SARIF result count.
#[test]
fn summary_findings_counts_every_reported_kind() {
    let temp = TempDir::new("side-broken");
    temp.copy_tree(&broken_visual_pbip());

    let (_, stdout, _) = run_scan(
        &ScanArgs {
            json: true,
            ..ScanArgs::default()
        },
        &temp.0,
        "",
    );
    let json = json_payload(&stdout);
    let summary = &json["summary"];
    let expected = summary["unused"].as_u64().expect("unused")
        + summary["broken"].as_u64().expect("broken")
        + summary["broken_artifacts"].as_u64().expect("artifacts")
        + summary["auto_date_time"]["unused_by_reports"]
            .as_u64()
            .expect("adt")
        + summary["auto_date_time"]["dead"].as_u64().expect("dead");
    assert!(summary["broken"].as_u64() > Some(0), "fixture has breakage");
    assert_eq!(summary["findings"].as_u64(), Some(expected));

    let (_, sarif, _) = run_scan(
        &ScanArgs {
            sarif: true,
            ..ScanArgs::default()
        },
        &temp.0,
        "",
    );
    let log = json_payload(&sarif);
    assert_eq!(
        log["runs"][0]["results"].as_array().map(Vec::len),
        Some(expected as usize)
    );
}

fn copy(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("read fixture") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            fs::create_dir_all(&target).expect("mkdir");
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

#[test]
fn an_unwritable_file_is_an_error() {
    let temp = TempDir::new("side-unwritable");
    temp.copy_tree(&mini_pbip());

    let (code, _, stderr) = run_scan(
        &ScanArgs {
            json_file: Some(PathBuf::from("missing-dir/out.json")),
            ..ScanArgs::default()
        },
        &temp.0,
        "",
    );
    assert_eq!(code, 2);
    assert!(
        stderr.contains("cannot write --json-file missing-dir"),
        "{stderr}"
    );
}
