//! Integration tests for model-centric scans: `scan --model X` pairs one
//! semantic model with every PBIR report bound to it, and reinterprets
//! `--report` values that are plain folders as search folders (issue #32).

#[allow(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, mini_pbip, project_into, run_scan, run_scan_tty};

/// Model-centric scan args: `--model` plus the given `--report` values.
fn model_args(model: impl Into<PathBuf>, reports: Vec<PathBuf>) -> ScanArgs {
    ScanArgs {
        model: Some(model.into()),
        reports,
        ..ScanArgs::default()
    }
}

/// Copies the mini fixture's model item into `dir` under `stem`.
fn model_into(dir: &Path, stem: &str) -> PathBuf {
    let target = dir.join(format!("{stem}.SemanticModel"));
    copy_dir(&mini_pbip().join("Mini.SemanticModel"), &target);
    target
}

/// Copies the mini fixture's report item to `dir/relative`, rewriting its
/// `definition.pbir` to `pbir` (`None` removes the reference file).
fn report_into(dir: &Path, relative: &str, pbir: Option<&str>) -> PathBuf {
    let target = dir.join(relative);
    copy_dir(&mini_pbip().join("Mini.Report"), &target);
    match pbir {
        Some(text) => {
            fs::write(target.join("definition.pbir"), text).expect("write definition.pbir");
        }
        None => {
            let _ = fs::remove_file(target.join("definition.pbir"));
        }
    }
    target
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create dir");
    for entry in fs::read_dir(from).expect("read source") {
        let entry = entry.expect("entry");
        let destination = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), &destination).expect("copy file");
        }
    }
}

fn by_path(written: &str) -> String {
    format!("{{\"datasetReference\": {{\"byPath\": {{\"path\": \"{written}\"}}}}}}")
}

fn by_connection(connection: &str) -> String {
    format!(
        "{{\"datasetReference\": {{\"byConnection\": {{\"connectionString\": \"{connection}\"}}}}}}"
    )
}

fn json_payload(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout).expect("valid json")
}

mod happy_path {
    use super::*;

    #[test]
    fn a_search_folder_discovers_every_bound_report() {
        let temp = TempDir::new("model-happy");
        model_into(&temp.0, "X");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );
        report_into(
            &temp.0,
            "reports/sub/B.Report",
            Some(&by_path("../../../X.SemanticModel")),
        );

        let args = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "the mini model keeps its dead chain");
        assert!(
            stderr.contains("with 2 report(s): A.Report, B.Report"),
            "the announce names every bound report:\n{stderr}"
        );
        assert!(
            !stderr.contains("Ignored"),
            "nothing is bound elsewhere:\n{stderr}"
        );
        assert!(stdout.contains("2 unused"), "findings:\n{stdout}");
    }

    #[test]
    fn json_lists_every_connected_report() {
        let temp = TempDir::new("model-happy-json");
        model_into(&temp.0, "X");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );
        report_into(
            &temp.0,
            "reports/sub/B.Report",
            Some(&by_path("../../../X.SemanticModel")),
        );

        let args = ScanArgs {
            json: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        let payload = json_payload(&stdout);
        let reports = payload["reports"].as_array().expect("reports array");
        assert_eq!(reports.len(), 2, "connected reports: {reports:?}");
        assert!(
            reports
                .iter()
                .any(|path| path.as_str().unwrap_or_default().ends_with("A.Report"))
        );
        assert!(
            reports
                .iter()
                .any(|path| path.as_str().unwrap_or_default().ends_with("B.Report"))
        );
    }
}

mod exclusions_and_unresolved {
    use super::*;

    #[test]
    fn a_report_bound_elsewhere_is_ignored_and_a_dangling_reference_is_a_notice() {
        let temp = TempDir::new("model-mixed");
        model_into(&temp.0, "X");
        model_into(&temp.0, "Y");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );
        report_into(
            &temp.0,
            "reports/HR.Report",
            Some(&by_path("../../Y.SemanticModel")),
        );
        report_into(
            &temp.0,
            "reports/Dangling.Report",
            Some(&by_path("../../Gone.SemanticModel")),
        );

        let args = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "the connected report keeps the scan running");
        assert!(
            stderr.contains("Ignored 1 report(s) bound to other models: HR.Report"),
            "exclusions are listed:\n{stderr}"
        );
        assert!(
            stderr.contains("[unresolved_dataset_reference]"),
            "the dangling reference is a skip notice:\n{stderr}"
        );
        assert!(
            stderr.contains("Dangling.Report"),
            "the notice names the report:\n{stderr}"
        );
    }

    #[test]
    fn unresolved_references_ride_the_json_skips_channel() {
        let temp = TempDir::new("model-mixed-json");
        model_into(&temp.0, "X");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );
        report_into(
            &temp.0,
            "reports/HR.Report",
            Some(&by_path("../../Gone.SemanticModel")),
        );

        let args = ScanArgs {
            json: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        let payload = json_payload(&stdout);
        assert_eq!(payload["skips"]["count"], 1);
        let notices = payload["skips"]["notices"].as_array().expect("notices");
        assert_eq!(notices[0]["kind"], "unresolved_dataset_reference");
        assert!(
            notices[0]["detail"]
                .as_str()
                .expect("detail")
                .contains("Gone.SemanticModel"),
            "the detail names the dangling path: {notices:?}"
        );
    }
}

mod refusals {
    use super::*;

    #[test]
    fn an_empty_search_folder_refuses_with_the_report_hint() {
        let temp = TempDir::new("model-empty");
        model_into(&temp.0, "X");
        temp.mkdir("reports");

        let args = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert!(
            stderr.contains("has no reports (no report items found under"),
            "message:\n{stderr}"
        );
        assert!(stderr.contains("--report"), "hint:\n{stderr}");
    }

    #[test]
    fn a_folder_with_only_unbound_items_counts_each_category() {
        let temp = TempDir::new("model-no-bounds");
        model_into(&temp.0, "X");
        model_into(&temp.0, "Y");
        report_into(
            &temp.0,
            "reports/HR.Report",
            Some(&by_path("../../Y.SemanticModel")),
        );
        report_into(
            &temp.0,
            "reports/Dangling.Report",
            Some(&by_path("../../Gone.SemanticModel")),
        );

        let args = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 2);
        assert!(
            stderr.contains(
                "has no reports (1 bound to other models, 1 unresolved dataset references under"
            ),
            "per-category counts:\n{stderr}"
        );
    }
}

mod default_search_folder {
    use super::*;

    #[test]
    fn no_report_flag_defaults_to_the_models_parent_folder() {
        let temp = TempDir::new("model-default");
        model_into(&temp.0, "X");
        report_into(&temp.0, "X.Report", Some(&by_path("../X.SemanticModel")));

        let args = model_args(temp.0.join("X.SemanticModel"), Vec::new());
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(
            stderr.contains("with 1 report(s): X.Report"),
            "the sibling report is found:\n{stderr}"
        );
    }
}

mod explicit_reports {
    use super::*;

    #[test]
    fn an_explicit_unbound_report_wins_without_a_notice() {
        let temp = TempDir::new("model-explicit-unbound");
        model_into(&temp.0, "X");
        report_into(
            &temp.0,
            "Loose.Report",
            Some(&by_path("../../Gone.SemanticModel")),
        );

        let args = model_args(
            temp.0.join("X.SemanticModel"),
            vec![temp.0.join("Loose.Report")],
        );
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "explicit reports are scanned at face value");
        assert!(
            !stderr.contains("unresolved_dataset_reference"),
            "explicit is explicit, no notice:\n{stderr}"
        );
        assert!(stdout.contains("2 unused"), "findings:\n{stdout}");
    }

    #[test]
    fn an_explicit_report_is_not_duplicated_by_its_search_folder() {
        let temp = TempDir::new("model-explicit-dedupe");
        model_into(&temp.0, "X");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );
        report_into(
            &temp.0,
            "reports/sub/B.Report",
            Some(&by_path("../../../X.SemanticModel")),
        );

        let args = ScanArgs {
            json: true,
            ..model_args(
                temp.0.join("X.SemanticModel"),
                vec![temp.0.join("reports/A.Report"), temp.0.join("reports")],
            )
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(
            stderr.contains("with 2 report(s)"),
            "A.Report is not counted twice:\n{stderr}"
        );
        let payload = json_payload(&stdout);
        let reports = payload["reports"].as_array().expect("reports array");
        assert_eq!(reports.len(), 2, "reports: {reports:?}");
        assert_eq!(payload["skips"]["count"], 0, "no walk notice for explicit");
    }

    #[test]
    fn nested_search_folders_do_not_duplicate_reports() {
        let temp = TempDir::new("model-overlap");
        model_into(&temp.0, "X");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );
        report_into(
            &temp.0,
            "reports/sub/B.Report",
            Some(&by_path("../../../X.SemanticModel")),
        );

        let args = ScanArgs {
            json: true,
            ..model_args(
                temp.0.join("X.SemanticModel"),
                vec![temp.0.join("reports"), temp.0.join("reports/sub")],
            )
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        let payload = json_payload(&stdout);
        assert_eq!(payload["reports"].as_array().expect("reports").len(), 2);
    }
}

mod discovery_is_disabled {
    use super::*;

    #[test]
    fn model_mode_never_touches_cwd_discovery_or_the_picker() {
        let temp = TempDir::new("model-cwd");
        model_into(&temp.0, "X");
        project_into(&temp.0, "Alpha");
        project_into(&temp.0, "Beta");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );

        let args = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
        let (code, _, stderr) = run_scan_tty(&args, &temp.0, "1\n", true);

        assert_eq!(code, 1);
        assert!(!stderr.contains("Discovered"), "no discovery:\n{stderr}");
        assert!(!stderr.contains("Selected"), "no selection:\n{stderr}");
        assert!(!stderr.contains("Select a project"), "no picker:\n{stderr}");
    }

    #[test]
    fn the_config_target_is_ignored_but_its_reports_are_search_folders() {
        let temp = TempDir::new("model-config");
        model_into(&temp.0, "X");
        model_into(&temp.0, "Other");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );
        temp.write(
            "ripbi.toml",
            "target = \"Other.SemanticModel\"\nreports = [\"reports\"]\n",
        );

        let args = model_args(temp.0.join("X.SemanticModel"), Vec::new());
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "the explicit model wins over the config target");
        assert!(
            stderr.contains("Scanning") && stderr.contains("X.SemanticModel"),
            "the explicit model is scanned:\n{stderr}"
        );
        assert!(
            stderr.contains("with 1 report(s): A.Report"),
            "the config reports value was walked as a search folder:\n{stderr}"
        );
        assert!(stdout.contains("2 unused"), "findings:\n{stdout}");
    }
}

mod name_based_binding {
    use super::*;

    #[test]
    fn by_connection_connects_by_dataset_name_with_a_note() {
        let temp = TempDir::new("model-by-connection");
        model_into(&temp.0, "X");
        temp.write(
            "X.SemanticModel/.platform",
            "{\"metadata\": {\"displayName\": \"Sales Model\"}}",
        );
        report_into(
            &temp.0,
            "reports/Thin.Report",
            Some(&by_connection(
                "Data Source=powerbi://api;Initial Catalog=\\\"Sales Model\\\"",
            )),
        );
        report_into(&temp.0, "reports/X.Report", None); // stem tier

        let args = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(
            stderr.contains("with 2 report(s)"),
            "both tiers connect:\n{stderr}"
        );
        assert!(
            stderr.contains("Note:")
                && stderr.contains("Thin.Report matched by dataset name only")
                && stderr.contains("'initial catalog' = 'Sales Model'"),
            "name-only matches are flagged:\n{stderr}"
        );
    }
}

mod binding_spellings {
    use super::*;

    #[test]
    fn a_backslash_by_path_resolves_on_every_platform() {
        let temp = TempDir::new("model-backslash");
        model_into(&temp.0, "X");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path(r"..\\..\\X.SemanticModel")),
        );

        let args = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(
            stderr.contains("with 1 report(s): A.Report"),
            "the backslash spelling connects:\n{stderr}"
        );
    }
}

mod strict_mode {
    use super::*;

    #[test]
    fn a_healthy_multi_model_folder_passes_strict() {
        let temp = TempDir::new("model-strict-healthy");
        model_into(&temp.0, "X");
        model_into(&temp.0, "Y");
        report_into(&temp.0, "X.Report", Some(&by_path("../X.SemanticModel")));
        report_into(&temp.0, "Y.Report", Some(&by_path("../Y.SemanticModel")));

        let args = ScanArgs {
            strict: true,
            ..model_args(temp.0.join("X.SemanticModel"), Vec::new())
        };
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(
            code, 1,
            "bound-elsewhere is informational, only the dead chain gates:\n{stderr}"
        );
        assert!(
            stderr.contains("Ignored 1 report(s) bound to other models: Y.Report"),
            "the exclusion is still visible:\n{stderr}"
        );
    }

    #[test]
    fn an_unresolved_reference_fails_strict() {
        let temp = TempDir::new("model-strict-unresolved");
        model_into(&temp.0, "X");
        report_into(&temp.0, "X.Report", Some(&by_path("../X.SemanticModel")));
        report_into(
            &temp.0,
            "Dangling.Report",
            Some(&by_path("../Gone.SemanticModel")),
        );

        let strict = ScanArgs {
            strict: true,
            ..model_args(temp.0.join("X.SemanticModel"), Vec::new())
        };
        let (code, _, stderr) = run_scan(&strict, &temp.0, "");
        assert_eq!(code, 2, "unresolved references are strict-fatal:\n{stderr}");

        let lenient = model_args(temp.0.join("X.SemanticModel"), Vec::new());
        let (code, _, _) = run_scan(&lenient, &temp.0, "");
        assert_eq!(code, 1, "without --strict the scan still runs");
    }

    #[test]
    fn malformed_reference_drift_surfaces_through_skips() {
        let temp = TempDir::new("model-strict-malformed");
        model_into(&temp.0, "X");
        report_into(&temp.0, "X.Report", Some(&by_path("../X.SemanticModel")));
        report_into(&temp.0, "Bad.Report", Some("{\"datasetReference\": {}}"));

        let args = ScanArgs {
            json: true,
            ..model_args(temp.0.join("X.SemanticModel"), Vec::new())
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        let payload = json_payload(&stdout);
        let notices = payload["skips"]["notices"].as_array().expect("notices");
        let kinds: Vec<&str> = notices
            .iter()
            .map(|notice| notice["kind"].as_str().unwrap_or_default())
            .collect();
        assert!(kinds.contains(&"malformed_value"), "core drift: {kinds:?}");
        assert!(
            kinds.contains(&"unresolved_dataset_reference"),
            "the unresolved item: {kinds:?}"
        );
    }
}

mod argument_validation {
    use super::*;

    #[test]
    fn a_plain_folder_needs_model_to_be_a_search_folder() {
        let temp = TempDir::new("model-plain-without");
        temp.mkdir("plain");
        let args = ScanArgs {
            path: Some(mini_pbip().join("Mini.pbip")),
            reports: vec![temp.0.join("plain")],
            ..ScanArgs::default()
        };
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 2);
        assert!(
            stderr.contains("is a folder, but not a report item"),
            "error:\n{stderr}"
        );
        assert!(
            stderr.contains("--model"),
            "the hint points at model mode:\n{stderr}"
        );
    }

    #[test]
    fn a_report_suffixed_folder_without_an_anchor_is_malformed() {
        let temp = TempDir::new("model-malformed");
        model_into(&temp.0, "X");
        temp.mkdir("broken.Report");

        let args = model_args(
            temp.0.join("X.SemanticModel"),
            vec![temp.0.join("broken.Report")],
        );
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 2);
        assert!(
            stderr.contains("malformed report folder") && stderr.contains("missing report.json"),
            "error:\n{stderr}"
        );
    }
}
