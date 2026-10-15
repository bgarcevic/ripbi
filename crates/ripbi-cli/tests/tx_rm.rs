//! Integration tests for `scan --emit tx-rm`: the unused findings as JSON
//! Lines for tomix's batch removal, `tx rm --from -`.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::fs;
use std::path::PathBuf;

use ripbi_cli::cli::{EmitFormat, ScanArgs};

use common::{TempDir, mini_pbip, run_scan};

fn tx_rm(path: Option<PathBuf>) -> ScanArgs {
    ScanArgs {
        path,
        emit: Some(EmitFormat::TxRm),
        ..ScanArgs::default()
    }
}

/// The target lines: comments (whose header names a platform path) dropped.
fn targets(stdout: &str) -> String {
    stdout
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| format!("{line}\n"))
        .collect()
}

#[test]
fn every_finding_is_one_json_line() {
    let temp = TempDir::new("tx-rm-mini");
    temp.copy_tree(&mini_pbip());

    let (code, stdout, stderr) = run_scan(&tx_rm(None), &temp.0, "");
    assert_eq!(code, 1, "unused findings still gate: {stderr}");
    assert!(stdout.starts_with("# ripbi unused objects in "), "{stdout}");
    assert_eq!(
        targets(&stdout),
        concat!(
            r#"{"type":"column","table":"Sales","name":"Legacy","id":"'Sales'[Legacy]"}"#,
            "\n",
            r#"{"type":"measure","table":"Sales","name":"Legacy Total","id":"'Sales'[Legacy Total]"}"#,
            "\n",
        )
    );
}

#[test]
fn quiet_prints_nothing() {
    let temp = TempDir::new("tx-rm-quiet");
    temp.copy_tree(&mini_pbip());

    let args = ScanArgs {
        quiet: true,
        ..tx_rm(None)
    };
    let (code, stdout, _) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "{stdout}");
}

/// AdventureWorks against a golden file (`RIPBI_BLESS=1` rewrites it). Piped
/// into `tx rm --from - --save`, these 56 targets remove the model's dead
/// code in one change and a rescan finds nothing unused.
#[test]
fn adventure_works_matches_its_golden_list() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sample = manifest.join("../../samples/AdventureWorks Sales.pbip");
    let cwd = sample.parent().expect("samples dir").to_path_buf();

    let (code, stdout, stderr) = run_scan(&tx_rm(Some(sample)), &cwd, "");
    assert_eq!(code, 1, "{stderr}");
    let actual = targets(&stdout);
    let golden = manifest.join("tests/fixtures/tx-rm/adventure-works.jsonl");
    if std::env::var_os("RIPBI_BLESS").is_some() {
        fs::create_dir_all(golden.parent().expect("parent")).expect("mkdir");
        fs::write(&golden, &actual).expect("write golden");
    }
    let expected = fs::read_to_string(&golden)
        .expect("golden file")
        .replace("\r\n", "\n");
    assert_eq!(actual, expected, "set RIPBI_BLESS=1 to accept the new list");
}
