//! CLI contracts for TMSL, PBIT and archive-backed report inputs.

#[expect(dead_code)]
mod common;

use std::path::PathBuf;

use common::{TempDir, json_payload, run_deps, run_report, run_scan};
use ripbi_cli::cli::{DepsArgs, ReportArgs, ScanArgs};
use ripbi_cli::discover::{Resolution, resolve_path};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn pbit() -> PathBuf {
    root().join("samples/AdventureWorks Sales.pbit")
}

#[test]
fn scan_pbit_uses_both_embedded_inputs() {
    let temp = TempDir::new("archive-scan");
    let args = ScanArgs {
        path: Some(pbit()),
        json: true,
        ..ScanArgs::default()
    };
    let (code, stdout, stderr) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    let payload = json_payload(&stdout);
    assert_eq!(payload["summary"]["objects"], 130);
    assert_eq!(payload["summary"]["unused"], 56);
    assert_eq!(payload["reports"].as_array().unwrap().len(), 1);
    assert_eq!(payload["skips"]["count"], 0);
}

#[test]
fn report_pbit_lists_embedded_visuals() {
    let temp = TempDir::new("archive-report");
    let args = ReportArgs {
        path: Some(pbit()),
        json: true,
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &temp.0, "");
    assert_eq!(code, 0, "{stderr}");
    let payload = json_payload(&stdout);
    assert_eq!(payload["reports"].as_array().unwrap().len(), 1);
    assert_eq!(payload["reports"][0]["pages"].as_array().unwrap().len(), 1);
    assert_eq!(
        payload["reports"][0]["pages"][0]["visuals"]
            .as_array()
            .unwrap()
            .len(),
        17
    );
}

#[test]
fn deps_pbit_sees_embedded_report_impact() {
    let temp = TempDir::new("archive-deps");
    let args = DepsArgs {
        object: Some("'Sales'[Sales]".to_string()),
        model: Some(pbit()),
        ..DepsArgs::default()
    };
    let (code, stdout, stderr) = run_deps(&args, &temp.0, "");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Reports"));
    assert!(stdout.contains("AdventureWorks"));
}

#[test]
fn model_bim_follows_no_report_rule_and_accepts_an_explicit_report() {
    let temp = TempDir::new("archive-bim");
    let fixture = root().join("crates/ripbi-core/tests/fixtures");
    let model = fixture.join("tmsl/golden/model.bim");
    let no_report = ScanArgs {
        path: Some(model.clone()),
        ..ScanArgs::default()
    };
    let (code, _, stderr) = run_scan(&no_report, &temp.0, "");
    assert_eq!(code, 2);
    assert!(stderr.contains("nothing to scan against"));

    let paired = ScanArgs {
        path: Some(model),
        reports: vec![fixture.join("pbir/golden/Mini.Report")],
        json: true,
        ..ScanArgs::default()
    };
    let (code, stdout, stderr) = run_scan(&paired, &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    assert!(
        !json_payload(&stdout)["unused"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn report_only_pbix_pairs_with_an_explicit_model() {
    let temp = TempDir::new("archive-modern-pbix");
    let report = root().join("crates/ripbi-core/tests/fixtures/legacy/modern-report.pbix");
    let args = ScanArgs {
        model: Some(root().join("samples/AdventureWorks Sales.pbip")),
        reports: vec![report],
        json: true,
        ..ScanArgs::default()
    };
    let (code, stdout, stderr) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    let payload = json_payload(&stdout);
    assert_eq!(payload["summary"]["unused"], 56);
    assert_eq!(payload["skips"]["count"], 0);
}

#[test]
fn report_only_pbix_uses_one_sibling_bim_model() {
    let temp = TempDir::new("archive-sibling-bim");
    let fixture = root().join("crates/ripbi-core/tests/fixtures");
    let report = temp.0.join("Mini.pbix");
    let model = temp.0.join("Mini.bim");
    std::fs::copy(fixture.join("legacy/modern-report.pbix"), &report).unwrap();
    std::fs::copy(fixture.join("tmsl/golden/model.bim"), &model).unwrap();
    let Resolution::Paired(paired) = resolve_path(&report).unwrap() else {
        panic!("PBIX should pair with its sibling BIM model");
    };
    assert_eq!(paired.model, model);
    assert_eq!(paired.reports, vec![report]);
}

#[test]
fn a_folder_with_one_pbit_discovers_its_embedded_pair() {
    let temp = TempDir::new("archive-only-folder");
    let path = temp.0.join("Template.pbit");
    std::fs::copy(pbit(), &path).unwrap();
    let Resolution::Paired(paired) = resolve_path(&temp.0).unwrap() else {
        panic!("single PBIT folder should resolve");
    };
    assert_eq!(paired.model, path);
    assert_eq!(paired.reports, vec![path]);
}

#[test]
fn report_only_pbix_uses_a_sole_differently_named_bim_model() {
    let temp = TempDir::new("archive-sole-bim");
    let fixture = root().join("crates/ripbi-core/tests/fixtures");
    let report = temp.0.join("Report.pbix");
    let model = temp.0.join("Model.bim");
    std::fs::copy(fixture.join("legacy/modern-report.pbix"), &report).unwrap();
    std::fs::copy(fixture.join("tmsl/golden/model.bim"), &model).unwrap();
    let Resolution::Paired(paired) = resolve_path(&report).unwrap() else {
        panic!("PBIX should pair with its sole sibling model");
    };
    assert_eq!(paired.model, model);
}
