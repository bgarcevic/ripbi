//! Integration tests for model-centric scans: `scan --model X` pairs one
//! semantic model with every PBIR report bound to it, and reinterprets
//! `--report` values that are plain folders as search folders (issue #32).

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_cli::cli::ScanArgs;

use common::{
    TempDir, by_connection, by_path, json_payload, mini_pbip, model_into, project_into,
    report_into, run_scan, run_scan_tty,
};

/// Model-centric scan args: `--model` plus the given `--report` values.
fn model_args(model: impl Into<PathBuf>, reports: Vec<PathBuf>) -> ScanArgs {
    ScanArgs {
        model: Some(model.into()),
        reports,
        ..ScanArgs::default()
    }
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

        let args = ScanArgs {
            verbose: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "the mini model keeps its dead chain");
        assert!(
            stderr.contains("with 2 report(s): A.Report, B.Report"),
            "--verbose names every bound report:\n{stderr}"
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

    /// A long ignored list reads as one capped line in the default output —
    /// three names, then the tail as a count and the pointer at `--verbose` —
    /// and as the full list under the flag.
    #[test]
    fn a_long_ignored_list_is_capped_and_verbose_lists_it() {
        let temp = TempDir::new("model-ignored-cap");
        model_into(&temp.0, "X");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );
        model_into(&temp.0, "Y");
        for name in ["HR", "Finans", "Salg", "Drift", "Support"] {
            report_into(
                &temp.0,
                &format!("reports/{name}.Report"),
                Some(&by_path("../../Y.SemanticModel")),
            );
        }

        let args = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
        let (_, _, stderr) = run_scan(&args, &temp.0, "");

        assert!(
            stderr.contains(
                "Ignored 5 report(s) bound to other models: \
                 Drift.Report, Finans.Report, HR.Report, … and 2 more \
                 — rerun with --verbose to list them"
            ),
            "the ignored list is capped with the pointer:\n{stderr}"
        );

        let args = ScanArgs {
            verbose: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (_, _, stderr) = run_scan(&args, &temp.0, "");
        assert!(
            stderr.contains("Ignored 5 report(s) bound to other models: ")
                && stderr.contains("Support.Report"),
            "--verbose lists every ignored report:\n{stderr}"
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

    #[test]
    fn an_anchorless_report_folder_in_a_search_walk_is_a_notice() {
        let temp = TempDir::new("model-walk-malformed");
        model_into(&temp.0, "X");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );
        temp.mkdir("reports/Broken.Report");

        let args = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "the valid report keeps the scan running");
        assert!(
            stderr.contains("[malformed_report_item]"),
            "the anchor-less folder is a skip notice:\n{stderr}"
        );
        assert!(
            stderr.contains("Broken.Report"),
            "the notice names the folder:\n{stderr}"
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
        assert_eq!(notices[0]["kind"], "malformed_report_item");
        assert!(
            notices[0]["path"]
                .as_str()
                .expect("path")
                .ends_with("Broken.Report"),
            "the notice carries the folder path: {notices:?}"
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

mod allow_no_reports {
    use super::*;

    /// The flag turns the model-mode refusal into a skip: exit 0, a notice
    /// with the walk's per-category counts, no scan output. A pipeline can
    /// point the scan at every model and let each run decide whether it has
    /// anything to scan against — a model that gains a report is scanned,
    /// with no exclusion list to keep honest.
    #[test]
    fn an_unbound_model_is_skipped_with_a_notice_instead_of_refused() {
        let temp = TempDir::new("model-allow-empty");
        model_into(&temp.0, "X");
        temp.mkdir("reports");

        let args = ScanArgs {
            allow_no_reports: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "nothing was scanned:\n{stdout}");
        assert!(
            stderr.contains("Skipped") && stderr.contains("no connected reports"),
            "the skip names the model:\n{stderr}"
        );
        assert!(
            stderr.contains("(no report items found under"),
            "the skip carries the walk's detail:\n{stderr}"
        );
    }

    #[test]
    fn the_skip_notice_counts_the_categories_like_the_refusal() {
        let temp = TempDir::new("model-allow-mixed");
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

        let args = ScanArgs {
            allow_no_reports: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 0);
        assert!(
            stderr.contains(
                "no connected reports (1 bound to other models, \
                 1 unresolved dataset references under"
            ),
            "the same per-category counts as the refusal:\n{stderr}"
        );
    }

    #[test]
    fn a_model_that_gains_a_report_is_scanned_normally() {
        let temp = TempDir::new("model-allow-gained");
        model_into(&temp.0, "X");
        report_into(&temp.0, "X.Report", Some(&by_path("../X.SemanticModel")));

        let args = ScanArgs {
            allow_no_reports: true,
            ..model_args(temp.0.join("X.SemanticModel"), Vec::new())
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "the mini model keeps its dead chain");
        assert!(
            stderr.contains("with 1 report(s)"),
            "the report is found and scanned:\n{stderr}"
        );
        assert!(
            !stderr.contains("Skipped"),
            "nothing is skipped once a report binds:\n{stderr}"
        );
        assert!(stdout.contains("2 unused"), "findings:\n{stdout}");
    }

    /// A model-naming PATH without reports never had a walk, so the skip
    /// carries no per-category detail — the fact and the model are enough.
    #[test]
    fn a_model_path_without_reports_is_skipped_without_a_walk_detail() {
        let temp = TempDir::new("model-allow-path");
        let model = model_into(&temp.0, "X");

        let args = ScanArgs {
            allow_no_reports: true,
            path: Some(model),
            ..ScanArgs::default()
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(
            stderr.contains("no connected reports"),
            "the skip notice:\n{stderr}"
        );
        assert!(
            !stderr.contains("bound to other models"),
            "no walk ran, so no per-category counts:\n{stderr}"
        );
    }

    /// A skip is an announcement, not a parse skip notice: `--strict` leaves
    /// it at 0, so a strict pipeline can still sweep report-less models.
    #[test]
    fn strict_does_not_fail_a_skipped_model() {
        let temp = TempDir::new("model-allow-strict");
        model_into(&temp.0, "X");
        temp.mkdir("reports");

        let args = ScanArgs {
            allow_no_reports: true,
            strict: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (code, _, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 0);
    }

    /// The JSON schema is untouched: a skipped model prints nothing on
    /// stdout — the notice rides on stderr, the exit code carries the rest.
    #[test]
    fn json_mode_skips_without_stdout_output() {
        let temp = TempDir::new("model-allow-json");
        model_into(&temp.0, "X");
        temp.mkdir("reports");

        let args = ScanArgs {
            allow_no_reports: true,
            json: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "no partial JSON payload:\n{stdout}");
        assert!(
            stderr.contains("Skipped"),
            "the notice stays on stderr:\n{stderr}"
        );
    }

    /// `-q` keeps the exit code as the only output, as everywhere else.
    #[test]
    fn quiet_skips_silently() {
        let temp = TempDir::new("model-allow-quiet");
        model_into(&temp.0, "X");
        temp.mkdir("reports");

        let args = ScanArgs {
            allow_no_reports: true,
            quiet: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 0);
        assert!(stdout.is_empty() && stderr.is_empty());
    }
}

mod default_search_folder {
    use super::*;

    #[test]
    fn no_report_flag_defaults_to_the_models_parent_folder() {
        let temp = TempDir::new("model-default");
        model_into(&temp.0, "X");
        report_into(&temp.0, "X.Report", Some(&by_path("../X.SemanticModel")));

        let args = ScanArgs {
            verbose: true,
            ..model_args(temp.0.join("X.SemanticModel"), Vec::new())
        };
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(
            stderr.contains("with 1 report(s): X.Report"),
            "--verbose names the sibling report:\n{stderr}"
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

        let args = ScanArgs {
            verbose: true,
            ..model_args(temp.0.join("X.SemanticModel"), Vec::new())
        };
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
                && stderr.contains(
                    "1 report(s) matched by dataset name only (byConnection 'initial catalog' = 'Sales Model'): Thin.Report"
                ),
            "name-only matches are flagged, capped to one line:\n{stderr}"
        );
    }

    /// A model whose display name `Sales Model` and `count` thin reports
    /// connected only through it, plus the stem-tier `X.Report`.
    fn thin_reports(dir: &Path, count: usize) -> Vec<PathBuf> {
        model_into(dir, "X");
        fs::write(
            dir.join("X.SemanticModel/.platform"),
            "{\"metadata\": {\"displayName\": \"Sales Model\"}}",
        )
        .expect("write .platform");
        let connection =
            by_connection("Data Source=powerbi://api;Initial Catalog=\\\"Sales Model\\\"");
        let mut reports = vec![report_into(dir, "reports/X.Report", None)];
        for index in 1..=count {
            reports.push(report_into(
                dir,
                &format!("reports/Thin{index}.Report"),
                Some(&connection),
            ));
        }
        reports
    }

    /// The #65 collapse is every mode's shape now: the human default reads
    /// one line per catalog, not one line per report.
    #[test]
    fn the_default_mode_collapses_to_one_line_per_catalog() {
        let temp = TempDir::new("model-notes-human");
        thin_reports(&temp.0, 2);

        let args = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
        let (_, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.starts_with("Note: ")
                    && line.contains("matched by dataset name only"))
                .count(),
            1,
            "the human mode reads the collapsed line:\n{stderr}"
        );
    }

    /// `--verbose` restores the per-report audit trail: one note per report,
    /// with its full path.
    #[test]
    fn verbose_keeps_one_note_per_report() {
        let temp = TempDir::new("model-notes-verbose");
        thin_reports(&temp.0, 2);

        let args = ScanArgs {
            verbose: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (_, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.starts_with("Note: ")
                    && line.contains("matched by dataset name only"))
                .count(),
            2,
            "--verbose lists every report:\n{stderr}"
        );
    }

    #[test]
    fn count_oriented_modes_collapse_the_notes_to_one_line_per_catalog() {
        let temp = TempDir::new("model-notes-collapsed");
        thin_reports(&temp.0, 2);

        for mode in ["summary", "plain", "json"] {
            let mut args = ScanArgs {
                summary: mode == "summary",
                plain: mode == "plain",
                json: mode == "json",
                ..ScanArgs::default()
            };
            args.model = Some(temp.0.join("X.SemanticModel"));
            args.reports = vec![temp.0.join("reports")];
            let (code, _, stderr) = run_scan(&args, &temp.0, "");

            assert_eq!(code, 1, "{mode}: exit code");
            let notes: Vec<&str> = stderr
                .lines()
                .filter(|line| line.contains("matched by dataset name only"))
                .collect();
            assert_eq!(notes.len(), 1, "{mode}: one line per catalog:\n{stderr}");
            assert!(
                notes[0].contains("2 report(s) matched by dataset name only")
                    && notes[0].contains("'initial catalog' = 'Sales Model'")
                    && notes[0].contains("Thin1.Report")
                    && notes[0].contains("Thin2.Report"),
                "{mode}: the line carries count, catalog, and names:\n{stderr}"
            );
        }
    }

    #[test]
    fn a_long_name_tail_is_capped_with_and_n_more() {
        let temp = TempDir::new("model-notes-cap");
        thin_reports(&temp.0, 5);

        let args = ScanArgs {
            summary: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (_, _, stderr) = run_scan(&args, &temp.0, "");

        assert!(
            stderr.contains(
                "5 report(s) matched by dataset name only \
                 (byConnection 'initial catalog' = 'Sales Model'): \
                 Thin1.Report, Thin2.Report, Thin3.Report, … and 2 more"
            ),
            "the tail is capped:\n{stderr}"
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

        let args = ScanArgs {
            verbose: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(
            stderr.contains("with 1 report(s): A.Report"),
            "--verbose names the report the backslash spelling connects:\n{stderr}"
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
    fn an_anchorless_report_folder_in_a_search_walk_fails_strict() {
        let temp = TempDir::new("model-strict-walk-malformed");
        model_into(&temp.0, "X");
        report_into(
            &temp.0,
            "reports/A.Report",
            Some(&by_path("../../X.SemanticModel")),
        );
        temp.mkdir("reports/Broken.Report");

        let strict = ScanArgs {
            strict: true,
            ..model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")])
        };
        let (code, _, stderr) = run_scan(&strict, &temp.0, "");
        assert_eq!(code, 2, "malformed walk items are strict-fatal:\n{stderr}");

        let lenient = model_args(temp.0.join("X.SemanticModel"), vec![temp.0.join("reports")]);
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

/// `--model` accepts a `.pbip` naming the project, and a `.pbip` passed to
/// `--report` expands to its project's reports.
mod pbip_anchors {
    use super::*;

    #[test]
    fn the_model_flag_accepts_a_pbip() {
        let temp = TempDir::new("model-pbip");
        project_into(&temp.0, "Mini");

        let args = ScanArgs {
            model: Some(temp.0.join("Mini.pbip")),
            ..ScanArgs::default()
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(
            code, 1,
            "the mini fixture's dead chain:\n{stdout}\n{stderr}"
        );
        assert!(
            stderr.contains("Mini.SemanticModel"),
            "the pbip resolved to its model:\n{stderr}"
        );
        assert!(stdout.contains("2 unused"), "findings:\n{stdout}");
    }

    #[test]
    fn a_pbip_report_anchor_expands_to_its_project_reports() {
        let temp = TempDir::new("pbip-anchor");
        project_into(&temp.0, "Mini");

        let args = model_args(
            temp.0.join("Mini.SemanticModel"),
            vec![temp.0.join("Mini.pbip")],
        );
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "{stdout}\n{stderr}");
        assert!(
            stdout.contains("2 unused"),
            "the project's report is a root:\n{stdout}"
        );
    }
}
