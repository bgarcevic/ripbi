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

    /// The model has no auto date/time machinery: the section exists in the
    /// schema, is empty, and changes neither the exit code nor the findings.
    #[test]
    fn a_model_without_auto_datetime_tables_has_an_empty_verdict_section() {
        let temp = TempDir::new("json-no-auto");
        let args = ScanArgs {
            json: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "the generic findings still gate the exit code");
        let payload: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
        let auto = payload["auto_date_time"].as_array().expect("auto array");
        assert!(auto.is_empty());
        assert_eq!(payload["summary"]["auto_date_time"]["in_use"], 0);
        assert_eq!(payload["summary"]["auto_date_time"]["unused_by_reports"], 0);
        assert_eq!(payload["summary"]["auto_date_time"]["dead"], 0);
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
    fn summary_mode_prints_counts_without_the_findings() {
        let temp = TempDir::new("summary");
        let args = ScanArgs {
            summary: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(
            stdout.contains("6 objects, 4 reachable from 1 roots, 2 unused"),
            "summary line:\n{stdout}"
        );
        assert!(stdout.contains("Measures: 1"), "count line:\n{stdout}");
        assert!(stdout.contains("Columns: 1"), "count line:\n{stdout}");
        assert!(
            !stdout.contains("'Sales'[Legacy"),
            "no finding ids on stdout:\n{stdout}"
        );
        assert!(!stdout.contains("←"), "no annotations:\n{stdout}");
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

mod type_filters {
    use super::*;

    /// The fixture has exactly one unused measure and one unused column, so
    /// every flag's effect is directly visible.
    #[test]
    fn a_type_flag_selects_only_its_group() {
        let temp = TempDir::new("filter-measures");
        let args = ScanArgs {
            measures: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1, "the selected measure is still unused");
        assert!(stdout.contains("Measures (1)"), "group header:\n{stdout}");
        assert!(
            stdout.contains("'Sales'[Legacy Total]"),
            "finding:\n{stdout}"
        );
        assert!(
            !stdout.contains("Columns"),
            "other groups vanish:\n{stdout}"
        );
        assert!(
            !stdout.contains("'Sales'[Legacy]"),
            "the hidden finding is gone, id included:\n{stdout}"
        );
        assert!(
            stdout.contains("(1 unused hidden by type filters)"),
            "hidden note:\n{stdout}"
        );
    }

    #[test]
    fn type_flags_passed_together_union_their_groups() {
        let temp = TempDir::new("filter-union");
        let args = ScanArgs {
            measures: true,
            columns: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(stdout.contains("Measures (1)"), "group header:\n{stdout}");
        assert!(stdout.contains("Columns (1)"), "group header:\n{stdout}");
        assert!(
            !stdout.contains("hidden by type filters"),
            "nothing was hidden:\n{stdout}"
        );
    }

    #[test]
    fn a_filter_that_hides_every_finding_exits_clean() {
        let temp = TempDir::new("filter-none");
        let args = ScanArgs {
            tables: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 0, "the exit code describes what was reported");
        assert!(
            stdout.contains("6 objects, 4 reachable from 1 roots, 0 unused"),
            "model-wide counts stand, findings do not:\n{stdout}"
        );
        assert!(
            stdout.contains("(2 unused hidden by type filters)"),
            "hidden note:\n{stdout}"
        );
        assert!(stdout.contains("No unused objects."));
    }

    #[test]
    fn quiet_mode_honors_the_filter_in_its_exit_code() {
        let temp = TempDir::new("filter-quiet");
        let args = ScanArgs {
            quiet: true,
            tables: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, stderr) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    #[test]
    fn json_mode_reflects_the_filter_in_unused_and_summary() {
        let temp = TempDir::new("filter-json");
        let args = ScanArgs {
            json: true,
            measures: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        let payload: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
        let unused = payload["unused"].as_array().expect("unused array");
        assert_eq!(unused.len(), 1, "only the measure survives:\n{unused:?}");
        assert_eq!(unused[0]["type"], "measure");
        assert_eq!(payload["summary"]["unused"], 1, "the filtered length");
        assert_eq!(
            payload["summary"]["unused_total"], 2,
            "the model-wide count"
        );
        assert_eq!(payload["summary"]["objects"], 6, "model-wide, unfiltered");
        assert_eq!(payload["summary"]["reachable"], 4, "model-wide, unfiltered");
    }

    #[test]
    fn plain_mode_emits_only_the_selected_records() {
        let temp = TempDir::new("filter-plain");
        let args = ScanArgs {
            plain: true,
            measures: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(
            lines,
            vec!["measure\t'Sales'[Legacy Total]"],
            "records:\n{stdout}"
        );
    }

    #[test]
    fn summary_mode_counts_only_the_selected_groups() {
        let temp = TempDir::new("filter-summary");
        let args = ScanArgs {
            summary: true,
            measures: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(stdout.contains("Measures: 1"), "count line:\n{stdout}");
        assert!(
            !stdout.contains("Columns"),
            "unselected counts vanish:\n{stdout}"
        );
    }

    /// `[scan].ignore` runs before the type flags: an object matched by both
    /// is a suppression, not a filter hiding. The measure id ends in
    /// `Legacy Total`; the column is bare `Legacy`, so this pattern only
    /// ever matches the measure.
    #[test]
    fn ignore_applies_before_the_type_filter() {
        let temp = TempDir::new("filter-ignore");
        project_into(&temp.0, "Mini");
        temp.write(
            "ripbi.toml",
            "target = \"Mini.SemanticModel\"\n\n[scan]\nignore = [\"*Legacy Total*\"]\n",
        );

        let args = ScanArgs {
            measures: true,
            ..ScanArgs::default()
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");
        assert_eq!(
            code, 0,
            "ignored measure + filtered column = nothing reported"
        );
        assert!(
            stdout.contains("(1 objects suppressed by [scan].ignore)"),
            "suppression note:\n{stdout}"
        );
        assert!(
            stdout.contains("(1 unused hidden by type filters)"),
            "the column is filter-hidden while the measure is ignore-suppressed:\n{stdout}"
        );

        let args = ScanArgs {
            columns: true,
            ..ScanArgs::default()
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");
        assert_eq!(code, 1, "the selected column is still unused");
        assert!(
            stdout.contains("column\t'Sales'[Legacy]") || stdout.contains("'Sales'[Legacy]"),
            "the ignored measure must not drag the column away:\n{stdout}"
        );
        assert!(
            stdout.contains("(1 objects suppressed by [scan].ignore)"),
            "suppression note:\n{stdout}"
        );
        assert!(
            !stdout.contains("Measures ("),
            "the suppressed measure has no group of its own (its chain note may still name it):\n{stdout}"
        );
    }
}

mod power_query_annotation {
    use super::*;

    /// The supply-chain annotation is cleanup-time guidance, not verdict
    /// information: hidden by default, shown with `--power-query` (issue #57).
    #[test]
    fn the_power_query_annotation_is_hidden_by_default() {
        let temp = TempDir::new("pq-default");
        let (code, stdout, _) = scan_path(&mini_pbip().join("Mini.pbip"), &temp.0);

        assert_eq!(code, 1);
        assert!(
            !stdout.contains("Power Query also names it"),
            "default output must not annotate:\n{stdout}"
        );
    }

    #[test]
    fn power_query_flag_shows_the_annotation() {
        let temp = TempDir::new("pq-flag");
        let args = ScanArgs {
            power_query: true,
            ..fixture_args(mini_pbip().join("Mini.pbip"))
        };
        let (code, stdout, _) = run_scan(&args, &temp.0, "");

        assert_eq!(code, 1);
        assert!(
            stdout.contains(
                "⭘ Power Query also names it ('Sales' partition) — safe to stop loading; \
                 removing it from the script means editing those steps too",
            ),
            "annotation:\n{stdout}"
        );
    }

    /// The JSON field is part of the additive schema and stays unconditional:
    /// the flag is a human-output knob, machine consumers filter themselves.
    #[test]
    fn json_carries_named_in_power_query_with_and_without_the_flag() {
        let temp = TempDir::new("pq-json");
        for power_query in [false, true] {
            let args = ScanArgs {
                json: true,
                power_query,
                ..fixture_args(mini_pbip().join("Mini.pbip"))
            };
            let (code, stdout, _) = run_scan(&args, &temp.0, "");
            assert_eq!(code, 1);
            let payload: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
            let legacy = payload["unused"]
                .as_array()
                .expect("unused array")
                .iter()
                .find(|finding| finding["id"] == "'Sales'[Legacy]")
                .expect("the dead column");
            assert_eq!(
                legacy["named_in_power_query"],
                serde_json::json!(["'Sales' partition"]),
                "flag={power_query}"
            );
        }
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
