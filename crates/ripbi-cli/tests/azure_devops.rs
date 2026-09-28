//! Integration tests for `scan --azure-devops`: one `##vso[task.logissue]`
//! line per finding, at the SARIF rule id and site, and nothing that
//! already existed under `--compare-root`.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use ripbi_cli::Cli;
use ripbi_cli::cli::ScanArgs;

use common::{TempDir, broken_visual_pbip, mini_pbip, run_scan};

fn azure_devops() -> ScanArgs {
    ScanArgs {
        azure_devops: true,
        ..ScanArgs::default()
    }
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
fn mini_pbip_logs_one_issue_per_finding() {
    let temp = TempDir::new("vso-mini");
    temp.copy_tree(&mini_pbip());

    let (code, stdout, stderr) = run_scan(&azure_devops(), &temp.0, "");
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(
        stdout,
        "##vso[task.logissue type=warning;sourcepath=Mini.SemanticModel/definition/tables/Sales.tmdl;linenumber=19;code=RIPBI-UNUSED-COLUMN;]Unused column 'Sales'[Legacy]: no report reaches it. Referenced only by 'Sales'[Legacy Total] (also unused).\n\
         ##vso[task.logissue type=warning;sourcepath=Mini.SemanticModel/definition/tables/Sales.tmdl;linenumber=10;code=RIPBI-UNUSED-MEASURE;]Unused measure 'Sales'[Legacy Total]: no report reaches it.\n"
    );
}

#[test]
fn breakage_logs_errors() {
    let temp = TempDir::new("vso-broken");
    temp.copy_tree(&broken_visual_pbip());

    let (_, stdout, stderr) = run_scan(&azure_devops(), &temp.0, "");
    let errors: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("##vso[task.logissue type=error;"))
        .collect();
    assert!(!errors.is_empty(), "{stdout}\n{stderr}");
    assert!(errors.iter().any(|line| {
        line.contains("sourcepath=Broken.Report/definition/report.json;code=RIPBI-BROKEN-VISUAL;")
    }));
}

#[test]
fn compare_root_logs_only_new_findings() {
    let temp = TempDir::new("vso-compare");
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
        ..azure_devops()
    };
    let (code, stdout, stderr) = run_scan(&args, &head, "");
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    assert!(lines[0].ends_with("]Unused measure 'Sales'[Draft KPI]: no report reaches it."));
}

#[test]
fn azure_devops_is_its_own_output_mode() {
    for other in ["--json", "--plain", "--sarif", "--summary"] {
        assert!(
            Cli::try_parse_from(["ripbi", "scan", "--azure-devops", other]).is_err(),
            "--azure-devops with {other}"
        );
    }
}
