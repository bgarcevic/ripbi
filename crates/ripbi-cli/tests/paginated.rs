//! Exercise the same CLI entry points used by CI and compare-root checkouts.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

const RDL: &str = include_str!("fixtures/rdl/Invoice.rdl");

fn run(dir: &Path, args: &[&str], code: i32) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_ripbi"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn json(dir: &Path, args: &[&str], code: i32) -> Value {
    serde_json::from_slice(&run(dir, args, code).stdout).unwrap()
}

#[test]
fn report_local_scan_scopes_fields_and_locates_filters() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    fs::write(dir.join("Invoice.rdl"), RDL).unwrap();
    let scan = json(dir, &["scan", "Invoice.rdl", "--json"], 1);
    let unused = scan["unused"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(unused.iter().any(|id| id.ends_with("Invoices.Draft")));
    assert!(unused.iter().any(|id| id.ends_with("OtherInvoices.Amount")));
    assert!(!unused.iter().any(|id| id.ends_with("/ Invoices.Amount")));
    let broken = &scan["broken"][0];
    assert!(broken.to_string().contains("InvoiceRows"));
    let sarif = json(dir, &["scan", "Invoice.rdl", "--broken", "--sarif"], 1);
    let site = &sarif["runs"][0]["results"][0]["locations"][0]["physicalLocation"];
    assert_eq!(site["artifactLocation"]["uri"], "Invoice.rdl");
    assert_eq!(site["region"]["startLine"], 22);
    let inventory = json(dir, &["report", "Invoice.rdl", "--json"], 0);
    assert_eq!(
        inventory["reports"][0]["declarations"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    run(dir, &["deps", "Invoice.rdl", "--json"], 0);
    let tx = run(dir, &["scan", "Invoice.rdl", "--emit", "tx-rm"], 1);
    assert!(
        String::from_utf8(tx.stdout)
            .unwrap()
            .lines()
            .all(|s| s.starts_with('#'))
    );
    fs::create_dir(dir.join("base")).unwrap();
    fs::write(
        dir.join("base/Invoice.rdl"),
        RDL.replace("<DataSets>", "\n<DataSets>"),
    )
    .unwrap();
    run(
        dir,
        &[
            "scan",
            "Invoice.rdl",
            "--compare-root",
            "base",
            "--broken",
            "--json",
        ],
        0,
    );
}

#[test]
fn model_pairing_keeps_the_correct_query_dependencies() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    fs::write(dir.join("Invoice.rdl"), RDL).unwrap();
    fs::write(dir.join("model.bim"),r#"{"name":"Mini","model":{"tables":[{"name":"Sales","measures":[{"name":"Legacy Total","expression":"1"},{"name":"Other Measure","expression":"2"}]}]}}"#).unwrap();
    let args = [
        "scan",
        "--model",
        "model.bim",
        "--report",
        "Invoice.rdl",
        "--type",
        "measure",
        "--json",
    ];
    let scan = json(dir, &args, 1);
    assert_eq!(scan["unused"].as_array().unwrap().len(), 1);
    assert!(scan["unused"][0].to_string().contains("Other Measure"));
    let discovered = json(
        dir,
        &[
            "scan",
            "--model",
            "model.bim",
            "--report",
            ".",
            "--type",
            "measure",
            "--json",
        ],
        1,
    );
    assert_eq!(discovered["unused"], scan["unused"]);
    let deps = json(
        dir,
        &[
            "deps",
            "[Legacy Total]",
            "--model",
            "model.bim",
            "--report",
            "Invoice.rdl",
            "--json",
        ],
        0,
    );
    assert!(deps.to_string().contains("Invoices"));
    let field = json(
        dir,
        &[
            "deps",
            "rdl_field Invoice.rdl / Invoices.Amount",
            "--model",
            "model.bim",
            "--report",
            "Invoice.rdl",
            "--dependencies",
            "--depth",
            "1",
            "--json",
        ],
        0,
    );
    assert!(field.to_string().contains("Legacy Total"));
    assert!(!field.to_string().contains("Other Measure"));
}

#[test]
fn dynamic_field_access_retains_its_dataset_only() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    fs::write(
        dir.join("Invoice.rdl"),
        RDL.replace(
            "<DataSets>",
            "<ReportParameters><ReportParameter Name=\"Which\"/></ReportParameters><DataSets>",
        )
        .replace(
            "Fields!Amount.Value",
            "Fields(Parameters!Which.Value).Value",
        ),
    )
    .unwrap();
    let scan = json(dir, &["scan", "Invoice.rdl", "--strict", "--json"], 2);
    assert!(scan["skips"].to_string().contains("dynamic collection"));
    assert!(!scan["unused"].to_string().contains("Invoices.Draft"));
    assert!(scan["unused"].to_string().contains("OtherInvoices.Amount"));
    assert_eq!(scan["broken"].as_array().unwrap().len(), 0);
}

#[test]
fn lookup_and_aggregate_fields_resolve_in_their_explicit_datasets() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let report=RDL.replace("=Fields!Missing.Value",r#"=Lookup(Fields!Amount.Value, Fields!Amount.Value, Fields!Unknown.Value, "OtherInvoices") + First(Fields!Lost.Value, "Invoices") + Parameters!Absent.Value"#);
    fs::write(dir.join("Invoice.rdl"), report).unwrap();
    let output = json(dir, &["scan", "Invoice.rdl", "--broken", "--json"], 1);
    let rows = output["broken"].as_array().unwrap();
    let targets: Vec<_> = rows.iter().map(|r| r["target"].as_str().unwrap()).collect();
    assert_eq!(
        targets,
        ["OtherInvoices.Unknown", "Invoices.Lost", "Absent"]
    );
}

#[test]
fn dynamic_model_query_keeps_potential_model_objects_and_marks_scan_incomplete() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let report = RDL.replace(
        "EVALUATE ROW(\"Amount\", [Legacy Total])",
        "=Parameters!Command.Value",
    );
    fs::write(dir.join("Invoice.rdl"), report).unwrap();
    fs::write(dir.join("model.bim"),r#"{"name":"Mini","model":{"tables":[{"name":"Sales","measures":[{"name":"Potential","expression":"1"}]}]}}"#).unwrap();
    let output = json(
        dir,
        &[
            "scan",
            "--model",
            "model.bim",
            "--report",
            "Invoice.rdl",
            "--strict",
            "--type",
            "measure",
            "--json",
        ],
        2,
    );
    assert_eq!(output["unused"].as_array().unwrap().len(), 0);
    assert!(output["skips"].to_string().contains("dataset Invoices"));
}

#[test]
fn report_builder_query_filter_keeps_its_model_column() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let query = r#"EVALUATE CALCULATETABLE(ROW("Amount", 1), RSCustomDaxFilter(@Year, EqualToCondition, [Dates].[Year], Int64))"#;
    let report = RDL.replace(r#"EVALUATE ROW("Amount", [Legacy Total])"#, query);
    fs::write(dir.join("Invoice.rdl"), report).unwrap();
    let model = r#"{"name":"Mini","model":{"tables":[{"name":"Dates","columns":[{"name":"Year","dataType":"int64"},{"name":"Unused","dataType":"int64"}]}]}}"#;
    fs::write(dir.join("model.bim"), model).unwrap();
    let output = json(
        dir,
        &[
            "scan",
            "--model",
            "model.bim",
            "--report",
            "Invoice.rdl",
            "--type",
            "column",
            "--json",
        ],
        1,
    );
    let unused = output["unused"].as_array().unwrap();
    assert_eq!(unused.len(), 1);
    assert!(unused[0]["id"].as_str().unwrap().contains("[Unused]"));
}
