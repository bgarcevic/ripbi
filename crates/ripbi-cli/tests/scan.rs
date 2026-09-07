//! Integration tests for `ripbi scan` against the mini PBIP fixture: output
//! modes, exit codes, discovery, `ripbi.toml`, and the interactive picker.

#[allow(dead_code)]
mod common;

use std::path::PathBuf;

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, mini_pbip, project_into, run_scan, run_scan_tty, scan_path};

fn fixture_args(path: impl Into<PathBuf>) -> ScanArgs {
    ScanArgs {
        path: Some(path.into()),
        ..ScanArgs::default()
    }
}

mod output_modes {
    use super::*;

    #[test]
    fn human_output_summarizes_and_annotates_the_chain() {
        let temp = TempDir::new("human");
        let (code, stdout, stderr) = scan_path(&mini_pbip().join("Mini.pbip"), &temp.0);

        assert_eq!(code, 1, "unused objects found");
        assert!(
            stdout.contains("6 objects, 4 reachable from 1 roots, 2 unused"),
            "summary line missing:\n{stdout}"
        );
        assert!(stdout.contains("Measures (1)"), "group header:\n{stdout}");
        assert!(
            stdout.contains("'Sales'[Legacy Total]"),
            "finding:\n{stdout}"
        );
        assert!(
            stdout.contains(
                "← only used by 'Sales'[Legacy Total] — measure expression (also unused)"
            ),
            "chain annotation:\n{stdout}"
        );
        assert!(stderr.contains("Scanning"), "announce missing:\n{stderr}");
        assert!(
            stderr.contains("external consumers (thin reports, Excel, XMLA) are invisible"),
            "caveat missing:\n{stderr}"
        );
    }

    #[test]
    fn json_mode_emits_the_v1_schema() {
        let temp = TempDir::new("json");
        let args = ScanArgs {
            json: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(stderr.contains("Scanning"), "announcements stay on stderr");

        let payload: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
        assert_eq!(payload["schema_version"], 1);
        assert_eq!(payload["summary"]["objects"], 6);
        assert_eq!(payload["summary"]["reachable"], 4);
        assert_eq!(payload["summary"]["roots"], 1);
        assert_eq!(payload["summary"]["unused"], 2);
        assert_eq!(payload["summary"]["ignored"], 0);
        assert_eq!(payload["skips"]["count"], 0);
        let unused = payload["unused"].as_array().expect("unused array");
        assert_eq!(unused.len(), 2);
        let legacy_total = unused
            .iter()
            .find(|finding| finding["id"] == "'Sales'[Legacy Total]")
            .expect("finding");
        assert_eq!(legacy_total["type"], "measure");
        assert!(
            legacy_total["used_by"]
                .as_array()
                .expect("used_by")
                .is_empty(),
            "an orphan has no consumers"
        );
        let legacy = unused
            .iter()
            .find(|finding| finding["id"] == "'Sales'[Legacy]")
            .expect("finding");
        assert_eq!(legacy["used_by"][0]["id"], "'Sales'[Legacy Total]");
        assert_eq!(legacy["used_by"][0]["also_unused"], true);
    }

    #[test]
    fn plain_mode_is_one_tab_separated_record_per_finding() {
        let temp = TempDir::new("plain");
        let args = ScanArgs {
            plain: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 2, "one record per finding:\n{stdout}");
        assert!(lines.contains(&"measure\t'Sales'[Legacy Total]"));
        assert!(lines.contains(&"column\t'Sales'[Legacy]"));
    }

    #[test]
    fn quiet_mode_prints_nothing_and_still_sets_the_exit_code() {
        let temp = TempDir::new("quiet");
        let args = ScanArgs {
            quiet: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
        assert!(stderr.is_empty(), "stderr must be empty:\n{stderr}");
    }
}

mod refusals {
    use super::*;

    #[test]
    fn a_model_with_no_report_refuses_and_suggests_report_flag() {
        let temp = TempDir::new("model-only");
        temp.mkdir("Lone.SemanticModel");
        let (code, stdout, stderr) = scan_path(&temp.0.join("Lone.SemanticModel"), &temp.0);

        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert!(
            stderr.contains("nothing to scan against"),
            "refusal message:\n{stderr}"
        );
        assert!(
            stderr.contains("--report"),
            "hint must suggest --report:\n{stderr}"
        );
    }

    #[test]
    fn an_archive_path_reports_it_is_not_yet_supported() {
        let temp = TempDir::new("archive");
        temp.write("Model.pbix", "PK\u{3}\u{4}");
        let (code, _, stderr) = scan_path(&temp.0.join("Model.pbix"), &temp.0);

        assert_eq!(code, 2);
        assert!(stderr.contains("not supported yet"), "error:\n{stderr}");
    }

    #[test]
    fn an_unrecognized_path_errors_with_guidance() {
        let temp = TempDir::new("unrecognized");
        temp.mkdir("plain");
        let (code, _, stderr) = scan_path(&temp.0.join("plain"), &temp.0);

        assert_eq!(code, 2);
        assert!(
            stderr.contains("not a Power BI project"),
            "error:\n{stderr}"
        );
    }

    #[test]
    fn a_missing_path_errors_clearly() {
        let temp = TempDir::new("missing");
        let (code, _, stderr) = scan_path(&temp.0.join("nope.pbip"), &temp.0);

        assert_eq!(code, 2);
        assert!(stderr.contains("no such path"), "error:\n{stderr}");
    }

    #[test]
    fn a_non_report_extra_report_flag_errors() {
        let temp = TempDir::new("bad-report");
        let args = ScanArgs {
            reports: vec![temp.0.join("NotAReport")],
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        temp.mkdir("NotAReport");
        let (code, _, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 2);
        assert!(
            stderr.contains("is not a report folder"),
            "error:\n{stderr}"
        );
    }
}

mod discovery {
    use super::*;

    #[test]
    fn a_dedicated_folder_displays_one_project_and_announces_it() {
        let temp = TempDir::new("discover-one");
        project_into(&temp.0, "Mini");
        let (code, stdout, stderr) = run_scan(&ScanArgs::default(), &temp.0, "");

        assert_eq!(code, 1);
        assert!(
            stderr.contains("Discovered 'Mini'."),
            "announce missing:\n{stderr}"
        );
        assert!(stdout.contains("2 unused"), "findings:\n{stdout}");
    }

    #[test]
    fn several_projects_fail_off_tty_with_the_candidate_list() {
        let temp = TempDir::new("discover-many");
        project_into(&temp.0, "Alpha");
        project_into(&temp.0, "Beta");
        let (code, _, stderr) = run_scan(&ScanArgs::default(), &temp.0, "");

        assert_eq!(code, 2);
        assert!(stderr.contains("multiple projects in"), "error:\n{stderr}");
        assert!(stderr.contains("- Alpha"), "candidate list:\n{stderr}");
        assert!(stderr.contains("- Beta"), "candidate list:\n{stderr}");
    }

    #[test]
    fn several_projects_prompt_on_a_tty_and_the_choice_is_scanned() {
        let temp = TempDir::new("discover-pick");
        project_into(&temp.0, "Alpha");
        project_into(&temp.0, "Beta");
        let (code, stdout, stderr) = run_scan_tty(&ScanArgs::default(), &temp.0, "2\n", true);

        assert_eq!(code, 1);
        assert!(
            stderr.contains("Select a project [1-2]:"),
            "prompt missing:\n{stderr}"
        );
        assert!(
            stderr.contains("Selected 'Beta'."),
            "selection announce missing:\n{stderr}"
        );
        assert!(stdout.contains("2 unused"), "findings:\n{stdout}");
    }

    #[test]
    fn the_picker_reprompts_on_garbage_input() {
        let temp = TempDir::new("picker-garbage");
        project_into(&temp.0, "Alpha");
        project_into(&temp.0, "Beta");
        let (code, _, stderr) = run_scan_tty(&ScanArgs::default(), &temp.0, "x\n99\n1\n", true);

        assert_eq!(code, 1);
        assert_eq!(
            stderr.matches("Enter a number between 1 and 2.").count(),
            2,
            "two invalid choices re-prompt:\n{stderr}"
        );
        assert!(stderr.contains("Selected 'Alpha'."));
    }

    #[test]
    fn an_empty_pick_fails_rather_than_guessing() {
        let temp = TempDir::new("picker-eof");
        project_into(&temp.0, "Alpha");
        project_into(&temp.0, "Beta");
        let (code, _, stderr) = run_scan(&ScanArgs::default(), &temp.0, "");

        assert_eq!(code, 2);
        assert!(stderr.contains("multiple projects"), "error:\n{stderr}");
    }

    #[test]
    fn no_input_flag_forbids_the_picker() {
        let temp = TempDir::new("picker-no-input");
        project_into(&temp.0, "Alpha");
        project_into(&temp.0, "Beta");
        let args = ScanArgs {
            no_input: true,
            ..ScanArgs::default()
        };
        let (code, _, stderr) = run_scan(&args, &temp.0, "2\n");

        assert_eq!(code, 2, "--no-input must not prompt");
        assert!(stderr.contains("multiple projects"));
    }

    #[test]
    fn discovery_in_an_empty_directory_fails_with_guidance() {
        let temp = TempDir::new("discover-none");
        let (code, _, stderr) = run_scan(&ScanArgs::default(), &temp.0, "");

        assert_eq!(code, 2);
        assert!(
            stderr.contains("no Power BI projects found"),
            "error:\n{stderr}"
        );
    }
}

mod config {
    use super::*;

    #[test]
    fn ripbi_toml_supplies_target_and_ignore_patterns() {
        let temp = TempDir::new("config");
        project_into(&temp.0, "Mini");
        temp.write(
            "ripbi.toml",
            "target = \"Mini.SemanticModel\"\n\n[scan]\nignore = [\"*Legacy*\"]\n",
        );
        let (code, stdout, stderr) = run_scan(&ScanArgs::default(), &temp.0, "");

        assert_eq!(code, 0, "both findings are ignored");
        assert!(
            stdout.contains("2 objects suppressed by [scan].ignore"),
            "suppressed note:\n{stdout}"
        );
        assert!(stdout.contains("No unused objects."));
        assert!(stderr.contains("Scanning"), "still announces:\n{stderr}");
    }

    #[test]
    fn a_report_flag_replaces_the_config_reports() {
        let temp = TempDir::new("config-reports");
        project_into(&temp.0, "Mini");
        temp.write(
            "ripbi.toml",
            "target = \"Mini.SemanticModel\"\nreports = [\"gone.Report\"]\n",
        );
        let args = ScanArgs {
            reports: vec![temp.0.join("Mini.Report")],
            ..ScanArgs::default()
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "the flag's report is used, the config's is not");
        assert!(stdout.contains("2 unused"), "findings:\n{stdout}");
    }

    #[test]
    fn a_broken_config_is_a_hard_error() {
        let temp = TempDir::new("config-broken");
        project_into(&temp.0, "Mini");
        temp.write("ripbi.toml", "target = [nope");
        let (code, _, stderr) = run_scan(&ScanArgs::default(), &temp.0, "");

        assert_eq!(code, 2);
        assert!(stderr.contains("cannot parse"), "error:\n{stderr}");
    }
}

mod extras {
    use super::*;

    #[test]
    fn a_report_flag_supplies_roots_for_a_model_path() {
        let temp = TempDir::new("flag-report");
        project_into(&temp.0, "Mini");
        let args = ScanArgs {
            reports: vec![temp.0.join("Mini.Report")],
            ..fixture_args(temp.0.join("Mini.SemanticModel"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(stdout.contains("2 unused"), "findings:\n{stdout}");
    }

    #[test]
    fn strict_mode_promotes_skip_notices_to_an_error() {
        let temp = TempDir::new("strict");
        project_into(&temp.0, "Mini");
        let visual = temp
            .0
            .join("Mini.Report/definition/pages/P1/visuals/V1/visual.json");
        let text = std::fs::read_to_string(&visual).expect("read visual.json");
        let drifted = text.replace(
            "\"visualType\": \"card\",",
            "\"visualType\": \"card\",\n    \"mysteryProperty\": 42,",
        );
        assert_ne!(text, drifted, "the drift must actually be injected");
        std::fs::write(&visual, drifted).expect("write visual.json");

        let base = fixture_args(temp.0.join("Mini.pbip"));
        let (lenient_code, _, lenient_stderr) = run_scan(&base, &temp.0, "");
        let strict_args = ScanArgs {
            strict: true,
            ..fixture_args(temp.0.join("Mini.pbip"))
        };
        let (strict_code, _, _) = run_scan(&strict_args, &temp.0, "");

        assert_eq!(lenient_code, 1, "drift alone does not fail the scan");
        assert!(
            lenient_stderr.contains("skip notice(s)"),
            "notices go to stderr:\n{lenient_stderr}"
        );
        assert!(
            lenient_stderr.contains("mysteryProperty"),
            "the notice names the property:\n{lenient_stderr}"
        );
        assert_eq!(strict_code, 2, "--strict turns the notice into an error");
    }

    #[test]
    fn duplicate_report_paths_are_ingested_once() {
        let temp = TempDir::new("dedupe");
        let pbip = mini_pbip().join("Mini.pbip");
        let report = mini_pbip().join("Mini.Report");
        let args = ScanArgs {
            json: true,
            reports: vec![report.clone(), report],
            ..fixture_args(pbip)
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        let payload: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
        let reports = payload["reports"].as_array().expect("reports array");
        assert_eq!(
            reports.len(),
            1,
            "the report list must be deduplicated: {reports:?}"
        );
    }
}
