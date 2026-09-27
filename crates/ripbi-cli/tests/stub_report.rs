//! CLI contracts for `ripbi stub-report` (issue #130) and how `scan` treats
//! the stub it writes.

#[expect(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{TempDir, json_payload, mini_pbip, run_scan};
use ripbi_cli::Streams;
use ripbi_cli::cli::{ScanArgs, StubReportArgs};
use ripbi_cli::stub;
use ripbi_core::ingest;

fn run_stub(args: &StubReportArgs, cwd: &Path) -> (i32, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut input: &[u8] = b"";
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        input: &mut input,
        stdin_is_tty: false,
        stdout_is_tty: false,
        stderr_is_tty: false,
    };
    let code = stub::run_in(args, cwd, &mut streams);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn stub_args(path: Option<&str>) -> StubReportArgs {
    StubReportArgs {
        path: path.map(PathBuf::from),
        ..StubReportArgs::default()
    }
}

/// Copies the tree under `from` into `to` (a `TempDir` wrapper would delete
/// `to` on drop).
fn copy_into(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_into(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A model-only project: the mini fixture's `.SemanticModel` alone.
fn model_only(temp: &TempDir, stem: &str) -> PathBuf {
    let model = temp.0.join(format!("{stem}.SemanticModel"));
    copy_into(&mini_pbip().join("Mini.SemanticModel"), &model);
    model
}

#[test]
fn writes_a_stub_beside_the_model_that_binds_it_and_nothing_else() {
    let temp = TempDir::new("stub-writes");
    model_only(&temp, "Central");
    let (code, stdout, stderr) = run_stub(&stub_args(Some("Central.SemanticModel")), &temp.0);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("Created Central.pbip and Central.Report"),
        "{stdout}"
    );
    assert!(
        stderr.contains("Central.SemanticModel/.pbi/cache.abf"),
        "{stderr}"
    );
    assert!(stderr.contains(".gitignore"), "{stderr}");

    let pbip = fs::read_to_string(temp.0.join("Central.pbip")).unwrap();
    assert!(pbip.contains("\"path\": \"Central.Report\""), "{pbip}");
    let report = ingest::report(&temp.0.join("Central.Report")).unwrap();
    assert!(report.skips.is_empty(), "{:?}", report.skips);
    assert!(report.value.stub);
    assert!(report.value.bindings().is_empty());
}

#[test]
fn quiet_writes_and_prints_nothing() {
    let temp = TempDir::new("stub-quiet");
    model_only(&temp, "Central");
    let args = StubReportArgs {
        quiet: true,
        ..stub_args(Some("Central.SemanticModel"))
    };
    let (code, stdout, stderr) = run_stub(&args, &temp.0);
    assert_eq!((code, stdout.as_str(), stderr.as_str()), (0, "", ""));
    assert!(temp.0.join("Central.Report/definition.pbir").is_file());
}

#[test]
fn discovers_the_only_model_and_accepts_its_definition_folder() {
    let temp = TempDir::new("stub-discover");
    model_only(&temp, "Central");
    let (code, _, stderr) = run_stub(&stub_args(None), &temp.0);
    assert_eq!(code, 0, "{stderr}");
    assert!(temp.0.join("Central.pbip").is_file());

    let other = TempDir::new("stub-definition");
    model_only(&other, "Central");
    let args = stub_args(Some("Central.SemanticModel/definition"));
    let (code, _, stderr) = run_stub(&args, &other.0);
    assert_eq!(code, 0, "{stderr}");
    assert!(other.0.join("Central.Report").is_dir());
}

#[test]
fn refuses_ambiguous_missing_or_wrong_targets() {
    let temp = TempDir::new("stub-targets");
    let (code, _, stderr) = run_stub(&stub_args(None), &temp.0);
    assert_eq!(code, 2);
    assert!(stderr.contains("no .SemanticModel folder"), "{stderr}");

    model_only(&temp, "A");
    model_only(&temp, "B");
    let (code, _, stderr) = run_stub(&stub_args(None), &temp.0);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("A.SemanticModel, B.SemanticModel"),
        "{stderr}"
    );

    temp.mkdir("plain");
    let (code, _, stderr) = run_stub(&stub_args(Some("plain")), &temp.0);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("is not a .SemanticModel folder"),
        "{stderr}"
    );

    let args = StubReportArgs {
        name: Some("a/b".to_string()),
        ..stub_args(Some("A.SemanticModel"))
    };
    let (code, _, stderr) = run_stub(&args, &temp.0);
    assert_eq!(code, 2);
    assert!(stderr.contains("invalid --name"), "{stderr}");
    assert!(!temp.0.join("A.pbip").exists());
}

#[test]
fn never_overwrites_without_force_and_never_replaces_a_real_report() {
    let temp = TempDir::new("stub-overwrite");
    model_only(&temp, "Central");
    let args = stub_args(Some("Central.SemanticModel"));
    assert_eq!(run_stub(&args, &temp.0).0, 0);

    // An existing stub: refused, then replaced under --force.
    let (code, _, stderr) = run_stub(&args, &temp.0);
    assert_eq!(code, 2);
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(stderr.contains("--force"), "{stderr}");
    let forced = StubReportArgs {
        force: true,
        ..stub_args(Some("Central.SemanticModel"))
    };
    let marker = temp.write("Central.Report/leftover.txt", "x");
    assert_eq!(run_stub(&forced, &temp.0).0, 0);
    assert!(!marker.exists(), "--force replaces the stub folder whole");

    // A real report under the same stem: refused even with --force.
    fs::remove_dir_all(temp.0.join("Central.Report")).unwrap();
    copy_into(
        &mini_pbip().join("Mini.Report"),
        &temp.0.join("Central.Report"),
    );
    let (code, _, stderr) = run_stub(&forced, &temp.0);
    assert_eq!(code, 2);
    assert!(stderr.contains("is not a ripbi stub"), "{stderr}");
    assert!(temp.0.join("Central.Report/definition/pages").is_dir());

    // A lone existing .pbip: refused without --force.
    let other = TempDir::new("stub-overwrite-pbip");
    model_only(&other, "Central");
    other.write("Central.pbip", "{}");
    let (code, _, stderr) = run_stub(&args, &other.0);
    assert_eq!(code, 2);
    assert!(stderr.contains("Central.pbip already exists"), "{stderr}");
    assert!(
        !other.0.join("Central.Report").exists(),
        "a refusal writes nothing"
    );
}

#[test]
fn scan_refuses_a_model_whose_only_report_is_a_stub() {
    let temp = TempDir::new("stub-scan-alone");
    model_only(&temp, "Central");
    assert_eq!(run_stub(&stub_args(None), &temp.0).0, 0);

    let args = ScanArgs {
        path: Some(temp.0.clone()),
        ..ScanArgs::default()
    };
    let (code, _, stderr) = run_scan(&args, &temp.0, "");
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("Note: Central.Report is a ripbi stub (no bindings)."),
        "{stderr}"
    );
    assert!(stderr.contains("has only ripbi stub reports"), "{stderr}");

    let skip = ScanArgs {
        allow_no_reports: true,
        ..args
    };
    let (code, stdout, stderr) = run_scan(&skip, &temp.0, "");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.is_empty());
    assert!(stderr.contains("(only ripbi stubs)"), "{stderr}");
}

#[test]
fn a_stub_beside_real_reports_leaves_findings_unchanged() {
    let temp = TempDir::new("stub-scan-parity");
    model_only(&temp, "Central");
    let thin = temp.0.join("Thin.Report");
    copy_into(&mini_pbip().join("Mini.Report"), &thin);
    temp.write(
        "Thin.Report/definition.pbir",
        r#"{"version": "4.0", "datasetReference": {"byPath": {"path": "../Central.SemanticModel"}}}"#,
    );
    let scan = |reports: Vec<PathBuf>| {
        let args = ScanArgs {
            model: Some(temp.0.join("Central.SemanticModel")),
            reports,
            json: true,
            ..ScanArgs::default()
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");
        (code, json_payload(&stdout), stderr)
    };

    let (before_code, before, _) = scan(vec![thin.clone()]);
    assert_eq!(run_stub(&stub_args(None), &temp.0).0, 0);
    let (after_code, after, stderr) = scan(vec![thin, temp.0.join("Central.Report")]);

    assert_eq!(before_code, after_code);
    assert_eq!(before["findings"], after["findings"]);
    assert_eq!(before["summary"]["unused"], after["summary"]["unused"]);
    assert_eq!(before["summary"]["roots"], after["summary"]["roots"]);
    assert_eq!(
        after["reports"].as_array().unwrap().len(),
        before["reports"].as_array().unwrap().len() + 1
    );
    assert!(
        stderr.contains("Central.Report is a ripbi stub"),
        "{stderr}"
    );
}
