//! CLI contracts for TMSL, PBIT and archive-backed report inputs.

#[expect(dead_code)]
mod common;

use std::path::PathBuf;

use common::{TempDir, json_payload, run_deps, run_report, run_scan};
use ripbi_cli::cli::{DepsArgs, ReportArgs, ScanArgs, SortKey};
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

/// Microsoft's public PBIX: its model exists only as the compressed ABF
/// `DataModel` member.
fn pbix() -> PathBuf {
    root().join("samples/Revenue Opportunities.pbix")
}

/// Writes the PBIX's `DataModel` member out as a standalone `.abf` backup.
fn extract_abf(dir: &std::path::Path, name: &str) -> PathBuf {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(pbix()).unwrap()).unwrap();
    let mut bytes = Vec::new();
    zip.by_name("DataModel")
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn scan_pbix_decodes_its_data_model_and_uses_its_own_report() {
    let temp = TempDir::new("archive-pbix-scan");
    let args = ScanArgs {
        path: Some(pbix()),
        json: true,
        strict: true,
        ..ScanArgs::default()
    };
    let (code, stdout, stderr) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    let payload = json_payload(&stdout);
    assert_eq!(payload["summary"]["objects"], 99);
    assert_eq!(payload["summary"]["unused"], 37);
    assert_eq!(payload["reports"].as_array().unwrap().len(), 1);
    assert_eq!(payload["skips"]["count"], 0);
}

#[test]
fn report_pbix_lists_its_visuals() {
    let temp = TempDir::new("archive-pbix-report");
    let args = ReportArgs {
        path: Some(pbix()),
        json: true,
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &temp.0, "");
    assert_eq!(code, 0, "{stderr}");
    let payload = json_payload(&stdout);
    assert_eq!(payload["reports"][0]["pages"].as_array().unwrap().len(), 3);
}

#[test]
fn deps_pbix_model_sees_its_report() {
    let temp = TempDir::new("archive-pbix-deps");
    let args = DepsArgs {
        object: Some("'Calculations'[Revenue]".to_string()),
        model: Some(pbix()),
        ..DepsArgs::default()
    };
    let (code, stdout, stderr) = run_deps(&args, &temp.0, "");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Reports"), "{stdout}");
}

#[test]
fn abf_follows_the_model_only_rule() {
    let temp = TempDir::new("archive-abf");
    let abf = extract_abf(&temp.0, "Revenue.abf");

    let Resolution::Paired(paired) = resolve_path(&abf).unwrap() else {
        panic!("an .abf resolves as a model");
    };
    assert_eq!(paired.model, abf);
    assert!(paired.reports.is_empty());

    let bare = ScanArgs {
        path: Some(abf.clone()),
        ..ScanArgs::default()
    };
    let (code, _, stderr) = run_scan(&bare, &temp.0, "");
    assert_eq!(code, 2);
    assert!(stderr.contains("nothing to scan against"), "{stderr}");

    let deps = DepsArgs {
        model: Some(abf.clone()),
        ..DepsArgs::default()
    };
    let (code, _, stderr) = run_deps(&deps, &temp.0, "");
    assert_eq!(code, 0, "{stderr}");

    let paired = ScanArgs {
        model: Some(abf),
        reports: vec![root().join("samples/Revenue Opportunities.Report")],
        json: true,
        ..ScanArgs::default()
    };
    let (code, stdout, stderr) = run_scan(&paired, &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(json_payload(&stdout)["summary"]["objects"], 99);
}

#[test]
fn thin_pbix_pairs_with_a_sibling_abf_or_model_pbix() {
    let temp = TempDir::new("archive-thin-pbix");
    let report = temp.0.join("Sales.pbix");
    std::fs::copy(
        root().join("crates/ripbi-core/tests/fixtures/legacy/modern-report.pbix"),
        &report,
    )
    .unwrap();
    let abf = extract_abf(&temp.0, "Sales.abf");
    let Resolution::Paired(paired) = resolve_path(&report).unwrap() else {
        panic!("thin PBIX should pair with its sibling backup");
    };
    assert_eq!(paired.model, abf);
    assert_eq!(paired.reports, vec![report.clone()]);

    std::fs::remove_file(&abf).unwrap();
    let model = temp.0.join("Model.pbix");
    std::fs::copy(pbix(), &model).unwrap();
    let Resolution::Paired(paired) = resolve_path(&report).unwrap() else {
        panic!("thin PBIX should pair with the sole model-bearing PBIX");
    };
    assert_eq!(paired.model, model);
}

#[test]
fn thin_pbix_without_a_model_says_so() {
    let temp = TempDir::new("archive-thin-alone");
    let report = temp.0.join("Alone.pbix");
    std::fs::copy(
        root().join("crates/ripbi-core/tests/fixtures/legacy/modern-report.pbix"),
        &report,
    )
    .unwrap();
    let args = ScanArgs {
        path: Some(report),
        ..ScanArgs::default()
    };
    let (code, _, stderr) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 2);
    assert!(stderr.contains("embeds no model"), "{stderr}");
}

fn revenue_pbix() -> PathBuf {
    root().join("samples/Revenue Opportunities.pbix")
}

/// Issue #122: a PBIX scan carries the storage catalog's sizes — per finding
/// and in total — and PBIP-only fields stay absent elsewhere.
#[test]
fn scan_pbix_reports_storage_sizes() {
    let temp = TempDir::new("archive-storage-json");
    let args = ScanArgs {
        path: Some(revenue_pbix()),
        json: true,
        ..ScanArgs::default()
    };
    let (code, stdout, stderr) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    let payload = json_payload(&stdout);
    let summary = &payload["summary"];
    let model_bytes = summary["model_bytes"].as_u64().unwrap();
    let unused_bytes = summary["unused_bytes"].as_u64().unwrap();
    assert!(unused_bytes > 0 && unused_bytes <= model_bytes);
    assert!(summary.get("unused_bytes_lower_bound").is_none());

    let unused = payload["unused"].as_array().unwrap();
    let mut sized = 0;
    for finding in unused {
        match finding["type"].as_str().unwrap() {
            "column" => {
                sized += 1;
                assert!(finding["bytes"].as_u64().unwrap() > 0, "{finding}");
                assert_eq!(finding["size_basis"], "files");
                assert!(finding["rows"].is_u64() && finding["cardinality"].is_u64());
            }
            "measure" => assert!(finding.get("bytes").is_none(), "{finding}"),
            _ => {}
        }
    }
    assert!(sized > 0);
    // No reported table here, so nothing is covered twice: the total is the sum.
    let sum: u64 = unused.iter().filter_map(|f| f["bytes"].as_u64()).sum();
    assert_eq!(sum, unused_bytes);
}

#[test]
fn scan_pbix_sorts_by_size_on_request() {
    let temp = TempDir::new("archive-storage-sort");
    let plain = |sort| ScanArgs {
        path: Some(revenue_pbix()),
        plain: true,
        sort,
        ..ScanArgs::default()
    };
    let (_, by_name, _) = run_scan(&plain(SortKey::Name), &temp.0, "");
    let (code, by_size, stderr) = run_scan(&plain(SortKey::Size), &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    assert!(!stderr.contains("no storage statistics"), "{stderr}");

    let bytes = |line: &str| line.split('\t').nth(2).map(|b| b.parse::<u64>().unwrap());
    let sizes: Vec<Option<u64>> = by_size.lines().map(bytes).collect();
    // Largest first, then every finding without a size.
    let mut expected = sizes.clone();
    expected.sort_by(|a, b| b.cmp(a));
    assert_eq!(sizes, expected);
    assert_ne!(by_name, by_size);
    let mut a: Vec<_> = by_name.lines().collect();
    let mut b: Vec<_> = by_size.lines().collect();
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b, "sorting only reorders");
}

#[test]
fn scan_pbix_human_output_shows_sizes() {
    let temp = TempDir::new("archive-storage-human");
    let args = ScanArgs {
        path: Some(revenue_pbix()),
        no_color: true,
        ..ScanArgs::default()
    };
    let (code, stdout, stderr) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    let total = stdout.lines().nth(1).unwrap();
    assert!(
        total.starts_with("Unused storage: \u{2248} ") && total.contains(" KB on disk ("),
        "{total}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("  'Opportunity'[Name]  (") && line.ends_with(" KB)")),
        "{stdout}"
    );
}

#[test]
fn sort_by_size_without_storage_keeps_name_order_and_says_so() {
    let temp = TempDir::new("archive-storage-none");
    let args = |sort| ScanArgs {
        path: Some(pbit()),
        plain: true,
        sort,
        ..ScanArgs::default()
    };
    let (_, by_name, _) = run_scan(&args(SortKey::Name), &temp.0, "");
    let (code, by_size, stderr) = run_scan(&args(SortKey::Size), &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(by_name, by_size);
    assert!(stderr.contains("has no storage statistics"), "{stderr}");
}
