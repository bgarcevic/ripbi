//! Excel workbooks as `--report` inputs (issue #172): their pivots, slicers,
//! and CUBE formulas keep what they use alive, a stale pivot field is a broken
//! binding, and binary workbooks are refused.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, json_payload, run_scan};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ripbi-core/tests/fixtures/xlsx")
}

/// A temp folder holding the fixture model and the fields workbook.
fn project(name: &str) -> TempDir {
    let temp = TempDir::new(name);
    fs::copy(fixtures().join("model.bim"), temp.0.join("model.bim")).unwrap();
    fs::copy(
        fixtures().join("olap-pivot-pbiazure-fields.xlsx"),
        temp.0.join("book.xlsx"),
    )
    .unwrap();
    temp
}

fn args(dir: &Path, reports: &[&str]) -> ScanArgs {
    ScanArgs {
        model: Some(dir.join("model.bim")),
        reports: reports.iter().map(|report| dir.join(report)).collect(),
        allow_no_reports: reports.is_empty(),
        ..ScanArgs::default()
    }
}

#[test]
fn a_measure_used_only_from_excel_is_live_with_the_workbook() {
    let temp = project("excel-live");
    // A thin stub report binding nothing, so the model scans without the book.
    temp.mkdir("Stub.Report/definition");
    temp.write("Stub.Report/definition/report.json", "{}");
    let (_, without, _) = run_scan(&args(&temp.0, &["Stub.Report"]), &temp.0, "");
    assert!(without.contains("'Sales'[Total Sales]"), "{without}");

    let (code, with, stderr) = run_scan(&args(&temp.0, &["book.xlsx"]), &temp.0, "");
    assert_eq!(code, 1, "stderr:\n{stderr}");
    assert!(!with.contains("'Sales'[Total Sales]"), "{with}");
    assert!(!with.contains("'Product'[Category]"), "{with}");
    assert!(with.contains("'Product'[Size]"), "{with}");
    assert!(
        stderr.contains("read 1 Excel workbook(s): book.xlsx"),
        "the workbook is listed among the inputs:\n{stderr}"
    );
    assert!(
        stderr.contains("check thin reports, other Excel workbooks and XMLA clients"),
        "the caveat narrows:\n{stderr}"
    );
    assert!(
        stderr.contains("Sheet2!A24 [not_analyzed]"),
        "the cell-referenced CUBE argument is a notice:\n{stderr}"
    );
}

#[test]
fn json_lists_the_workbook_and_a_stale_pivot_field_as_broken() {
    let temp = project("excel-broken");
    let model = fs::read_to_string(temp.0.join("model.bim")).unwrap();
    let stale = model.replace(
        r#"{ "name": "Category", "dataType": "string", "sourceColumn": "Category" },"#,
        "",
    );
    assert_ne!(model, stale);
    fs::write(temp.0.join("model.bim"), stale).unwrap();

    let mut scan = args(&temp.0, &["book.xlsx"]);
    scan.json = true;
    let (_, stdout, stderr) = run_scan(&scan, &temp.0, "");
    let payload = json_payload(&stdout);
    let reports = payload["reports"].as_array().expect("reports");
    assert_eq!(reports.len(), 1);
    assert!(reports[0].as_str().unwrap().ends_with("book.xlsx"));
    let broken = payload["broken"].as_array().expect("broken");
    assert_eq!(broken.len(), 1, "stderr:\n{stderr}");
    assert_eq!(broken[0]["target"], "[Product].[Category].[Category]");
    assert_eq!(broken[0]["reason"], "field_not_found");
    assert_eq!(
        broken[0]["location"],
        "field well 'Rows' — pivotTable visual 'Sales Model' on page 'Sheet2' in report 'book.xlsx'"
    );
    let kinds: Vec<&str> = payload["skips"]["notices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|notice| notice["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["not_analyzed"]);
}

#[test]
fn ripbi_toml_reports_accept_a_workbook() {
    let temp = project("excel-config");
    temp.write(
        "ripbi.toml",
        "target = \"model.bim\"\nreports = [\"book.xlsx\"]\n",
    );
    let (code, stdout, stderr) = run_scan(&ScanArgs::default(), &temp.0, "");
    assert_eq!(code, 1, "stderr:\n{stderr}");
    assert!(stderr.contains("read 1 Excel workbook(s)"), "{stderr}");
    assert!(!stdout.contains("'Sales'[Total Sales]"), "{stdout}");
}

#[test]
fn a_binary_workbook_is_refused() {
    let temp = project("excel-binary");
    for name in ["old.xls", "big.xlsb"] {
        temp.write(name, "");
        let (code, _, stderr) = run_scan(&args(&temp.0, &[name]), &temp.0, "");
        assert_eq!(code, 2, "{name}: {stderr}");
        assert!(stderr.contains("is a binary workbook"), "{stderr}");
        assert!(stderr.contains("save it as .xlsx"), "{stderr}");
    }
}

#[test]
fn a_workbook_alone_cannot_name_its_model() {
    let temp = project("excel-no-model");
    let scan = ScanArgs {
        reports: vec![temp.0.join("book.xlsx")],
        ..ScanArgs::default()
    };
    let (code, _, stderr) = run_scan(&scan, &temp.0, "");
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("pass --model"), "{stderr}");
}

#[test]
fn a_search_folder_does_not_pick_up_workbooks() {
    let temp = project("excel-search");
    temp.mkdir("reports");
    fs::rename(temp.0.join("book.xlsx"), temp.0.join("reports/book.xlsx")).unwrap();
    let (_, _, stderr) = run_scan(&args(&temp.0, &["reports"]), &temp.0, "");
    assert!(!stderr.contains("Excel workbook"), "{stderr}");
}
