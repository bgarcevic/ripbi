//! Integration tests of the `deps` command: the focused object view, the
//! overview, depth, lookup errors, and the input ladder it shares with scan.
//! Output assertions are exact strings — the view is a contract.

#[expect(dead_code)]
mod common;

use common::{
    TempDir, by_connection, json_payload, model_into, project_into, report_into, run_deps,
};
use ripbi_cli::cli::DepsArgs;

/// A scratch directory holding the mini fixture's project under its own
/// name, so cwd discovery pairs it like a real checkout. The name is
/// unique per call: tests run in parallel in one process, and a shared
/// directory would be deleted out from under them.
fn mini_project() -> TempDir {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = TempDir::new(&format!("deps-mini-{seq}"));
    project_into(&dir.0, "Mini");
    dir
}

fn object_in(object: &str, configure: impl FnOnce(&mut DepsArgs)) -> (i32, String, String) {
    let dir = mini_project();
    let mut args = DepsArgs {
        object: Some(object.to_string()),
        ..DepsArgs::default()
    };
    configure(&mut args);
    run_deps(&args, &dir.0, "")
}

fn object(object: &str) -> (i32, String, String) {
    object_in(object, |_| {})
}

/// Workspace monitoring: logged queries are consumers ripbi cannot otherwise
/// see, so the Impact view names the queried objects on the slice.
mod queried {
    use super::*;

    fn log() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../ripbi-core/tests/fixtures/query-log/semantic-model-logs.csv")
    }

    fn with_log(object: &str, configure: impl FnOnce(&mut DepsArgs)) -> (i32, String, String) {
        object_in(object, |args| {
            args.queries_from = Some(log());
            configure(args);
        })
    }

    #[test]
    fn an_excel_query_shows_under_impact() {
        let (code, out, err) = with_log("'Sales'[Legacy]", |args| args.impact = true);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            "'Sales'[Legacy]  column

Impact

Model
└─ 'Sales'[Legacy Total]  measure

Queried
└─ 'Sales'[Legacy Total]  queried 2× by 1 user · last 2026-09-30 · Excel
"
        );
        assert!(
            err.contains("Note: 3 logged queries (2026-09-28 → 2026-09-30) from "),
            "{err}"
        );
        assert!(err.contains("and the logged queries"), "{err}");
    }

    #[test]
    fn a_gzipped_log_reads_like_the_plain_one() {
        let dir = tempfile::tempdir().unwrap();
        let gz = dir.path().join("semantic-model-logs.csv.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&gz).unwrap(),
            flate2::Compression::default(),
        );
        std::io::Write::write_all(&mut encoder, &std::fs::read(log()).unwrap()).unwrap();
        encoder.finish().unwrap();
        let (code, out, err) = object_in("'Sales'[Legacy]", |args| {
            args.queries_from = Some(gz);
            args.impact = true;
        });
        assert_eq!(code, 0, "{err}");
        assert!(
            out.contains("\nQueried\n└─ 'Sales'[Legacy Total]  queried 2× by 1 user"),
            "{out}"
        );
    }

    #[test]
    fn the_queried_root_itself_counts_its_reports() {
        let (code, out, err) = with_log("'Sales'[Total]", |args| args.impact = true);
        assert_eq!(code, 0, "{err}");
        assert!(
            out.ends_with(
                "\nQueried\n└─ queried 1× by 1 user · last 2026-09-28 · PowerBI · 1 report\n"
            ),
            "{out}"
        );
    }

    #[test]
    fn plain_and_json_carry_the_queried_record_without_user_names() {
        let (_, plain, _) = with_log("'Sales'[Legacy]", |args| args.plain = true);
        assert!(
            plain.contains(
                "queried\t'Sales'[Legacy Total]\t2\t2026-09-30 09:45:00.0000000\t1\tExcel\n"
            ),
            "{plain}"
        );
        let (_, json, _) = with_log("'Sales'[Total]", |args| args.json = true);
        assert!(!json.contains("contoso.example"), "{json}");
        assert_eq!(
            json_payload(&json)["queried"],
            serde_json::json!([{
                "object": "'Sales'[Total]",
                "count": 1,
                "last_seen": "2026-09-28 08:15:01.0300000",
                "users": 1,
                "applications": ["PowerBI"],
                "reports": ["00000000-0000-4000-8000-00000000000a"],
            }])
        );
    }

    #[test]
    fn consumer_filters_leave_queried_out() {
        let (_, out, _) = with_log("'Sales'[Legacy]", |args| {
            args.consumer = Some("visual".to_string());
        });
        assert!(!out.contains("Queried"), "{out}");
    }

    #[test]
    fn without_a_log_the_view_and_caveat_are_unchanged() {
        let (_, out, err) = object_in("'Sales'[Legacy]", |args| args.json = true);
        assert_eq!(json_payload(&out)["queried"], serde_json::json!([]));
        assert!(
            err.contains("external consumers (thin reports, Excel, XMLA)"),
            "{err}"
        );
    }

    #[test]
    fn the_config_key_supplies_the_log() {
        let dir = mini_project();
        std::fs::copy(log(), dir.0.join("logs.csv")).expect("copy log");
        std::fs::write(dir.0.join("ripbi.toml"), "queries_from = \"logs.csv\"\n")
            .expect("write config");
        let args = DepsArgs {
            object: Some("'Sales'[Legacy]".to_string()),
            impact: true,
            ..DepsArgs::default()
        };
        let (code, out, err) = run_deps(&args, &dir.0, "");
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("\nQueried\n"), "{out}");
    }

    #[test]
    fn an_unreadable_log_is_a_usage_error() {
        let (code, _, err) = object_in("'Sales'[Legacy]", |args| {
            args.queries_from = Some("does-not-exist.csv".into());
        });
        assert_eq!(code, 2);
        assert!(
            err.contains("error: cannot read queries from does-not-exist.csv"),
            "{err}"
        );
    }

    #[test]
    fn a_log_for_another_model_earns_a_note() {
        let dir = mini_project();
        let text = std::fs::read_to_string(log()).expect("read log");
        std::fs::write(
            dir.0.join("other.csv"),
            text.replace("\"Mini\"", "\"Other\""),
        )
        .expect("write log");
        let args = DepsArgs {
            object: Some("'Sales'[Legacy]".to_string()),
            queries_from: Some(dir.0.join("other.csv")),
            ..DepsArgs::default()
        };
        let (code, _, err) = run_deps(&args, &dir.0, "");
        assert_eq!(code, 0, "{err}");
        assert!(
            err.contains("Note: the query log names Other, not Mini;"),
            "{err}"
        );
    }

    /// A workspace-wide export: `Mini` read a column, `Other` — a different
    /// model with the same object names — ran the Excel query on `[Legacy Total]`.
    const TWO_MODELS: &str = "OperationName,ItemName,ItemId,Timestamp,ApplicationName,EventText\n\
        QueryEnd,Mini,aaaaaaaa-0001,2026-09-01,PowerBI,\"EVALUATE VALUES('Sales'[Amount])\"\n\
        QueryEnd,Other,bbbbbbbb-0002,2026-09-02,Excel,\"SELECT {[Measures].[Legacy Total]} ON 0 FROM [Model]\"\n";

    fn two_models(names: (&str, &str)) -> (TempDir, std::path::PathBuf) {
        let dir = mini_project();
        let path = dir.0.join("workspace.csv");
        let text = TWO_MODELS
            .replace(",Mini,", &format!(",{},", names.0))
            .replace(",Other,", &format!(",{},", names.1));
        std::fs::write(&path, text).expect("write log");
        (dir, path)
    }

    fn legacy_impact(
        dir: &TempDir,
        configure: impl FnOnce(&mut DepsArgs),
    ) -> (i32, String, String) {
        let mut args = DepsArgs {
            object: Some("'Sales'[Legacy]".to_string()),
            impact: true,
            ..DepsArgs::default()
        };
        configure(&mut args);
        run_deps(&args, &dir.0, "")
    }

    #[test]
    fn a_workspace_wide_log_is_narrowed_to_the_model() {
        let (dir, log) = two_models(("Mini", "Other"));
        let (code, out, err) = legacy_impact(&dir, |args| args.queries_from = Some(log));
        assert_eq!(code, 0, "{err}");
        assert!(
            !out.contains("Queried"),
            "Other's query must not count:\n{out}"
        );
        assert!(
            err.contains("Note: 1 logged queries (2026-09-01) from "),
            "{err}"
        );
        assert!(
            err.contains(
                "Note: the query log covers 2 models; ignored 1 of 2 queries logged against the others."
            ),
            "{err}"
        );
    }

    #[test]
    fn queries_item_picks_the_model_by_name_or_id() {
        for key in ["other", "BBBBBBBB-0002"] {
            let (dir, log) = two_models(("Mini", "Other"));
            let (code, out, err) = legacy_impact(&dir, |args| {
                args.queries_from = Some(log);
                args.queries_item = Some(key.to_string());
            });
            assert_eq!(code, 0, "{err}");
            assert!(
                out.contains("└─ 'Sales'[Legacy Total]  queried 1× · last 2026-09-02 · Excel\n"),
                "{key}: {out}"
            );
        }
    }

    #[test]
    fn the_config_key_picks_the_model() {
        let (dir, _) = two_models(("Mini", "Other"));
        std::fs::write(
            dir.0.join("ripbi.toml"),
            "queries_from = \"workspace.csv\"\nqueries_item = \"Other\"\n",
        )
        .expect("write config");
        let (code, out, err) = legacy_impact(&dir, |_| {});
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("\nQueried\n"), "{out}");
    }

    /// The log is supplementary: a model with no rows in it is simply
    /// unqueried, never a failed run.
    #[test]
    fn a_log_of_other_models_only_counts_nothing() {
        let (dir, log) = two_models(("Finance", "Budget"));
        let (code, out, err) = legacy_impact(&dir, |args| args.queries_from = Some(log));
        assert_eq!(code, 0, "{err}");
        assert!(!out.contains("Queried"), "{out}");
        assert!(err.contains("Note: 0 logged queries from "), "{err}");
        assert!(
            err.contains(
                "Note: the query log has no queries for Mini (it names Budget, Finance); \
                 pass --queries-item NAME"
            ),
            "{err}"
        );
    }

    #[test]
    fn an_explicit_item_the_log_lacks_counts_nothing() {
        let (code, out, err) = with_log("'Sales'[Legacy]", |args| {
            args.impact = true;
            args.queries_item = Some("Finance".to_string());
        });
        assert_eq!(code, 0, "{err}");
        assert!(!out.contains("Queried"), "{out}");
        assert!(
            err.contains("Note: the query log has no queries for Finance (it names Mini);"),
            "{err}"
        );
    }
}

/// Issue #151: a `ripbi_keep` annotation is the answer to "why is this
/// alive?" — the Impact view names the kept object and its reason.
mod kept {
    use super::*;

    const SALES: &str = "Mini.SemanticModel/definition/tables/Sales.tmdl";
    /// `[Legacy Total]`'s last line, without its line ending: checkouts may
    /// convert the fixture to CRLF.
    const LEGACY_TOTAL: &str = "lineageTag: 99999999-9999-9999-9999-999999999903";

    /// The mini project with `[Legacy Total]` (and so its input `[Legacy]`)
    /// kept by annotation.
    fn kept_project(annotation: &str) -> TempDir {
        let dir = mini_project();
        let path = dir.0.join(SALES);
        let text = std::fs::read_to_string(&path).expect("read Sales.tmdl");
        assert_eq!(text.matches(LEGACY_TOTAL).count(), 1);
        let text = text.replace(LEGACY_TOTAL, &format!("{LEGACY_TOTAL}\n\t\t{annotation}"));
        std::fs::write(&path, text).expect("write Sales.tmdl");
        dir
    }

    fn run(dir: &TempDir, object: &str, configure: impl FnOnce(&mut DepsArgs)) -> String {
        let mut args = DepsArgs {
            object: Some(object.to_string()),
            ..DepsArgs::default()
        };
        configure(&mut args);
        let (code, out, err) = run_deps(&args, &dir.0, "");
        assert_eq!(code, 0, "{err}");
        out
    }

    #[test]
    fn an_input_of_a_kept_measure_names_it_with_its_reason() {
        let dir = kept_project("annotation ripbi_keep = Used by the Finance Excel pivot");
        let out = run(&dir, "'Sales'[Legacy]", |args| args.impact = true);
        assert_eq!(
            out,
            "'Sales'[Legacy]  column

Impact

Model
└─ 'Sales'[Legacy Total]  measure

Kept
└─ 'Sales'[Legacy Total]  kept: Used by the Finance Excel pivot
"
        );
    }

    #[test]
    fn the_kept_object_itself_shows_its_reason_and_empty_reasons_say_so() {
        let dir = kept_project("annotation ripbi_keep =");
        let out = run(&dir, "'Sales'[Legacy Total]", |args| args.impact = true);
        assert!(
            out.ends_with("\nKept\n└─ kept (no reason given)\n"),
            "{out}"
        );
    }

    #[test]
    fn plain_and_json_carry_the_kept_record() {
        let dir = kept_project("annotation ripbi_keep = Excel");
        let plain = run(&dir, "'Sales'[Legacy]", |args| args.plain = true);
        assert!(
            plain.contains("kept\t'Sales'[Legacy Total]\t'Sales'[Legacy Total]\tExcel\n"),
            "{plain}"
        );
        let json = json_payload(&run(&dir, "'Sales'[Legacy]", |args| args.json = true));
        assert_eq!(
            json["kept"],
            serde_json::json!([{
                "object": "'Sales'[Legacy Total]",
                "annotated": "'Sales'[Legacy Total]",
                "reason": "Excel",
            }])
        );
    }

    /// A consumer or report filter asks about consumers of that kind; a kept
    /// annotation is neither, so it steps aside.
    #[test]
    fn consumer_filters_leave_kept_out() {
        let dir = kept_project("annotation ripbi_keep = Excel");
        let out = run(&dir, "'Sales'[Legacy]", |args| {
            args.consumer = Some("visual".to_string());
        });
        assert!(!out.contains("Kept"), "{out}");
    }
}

mod focused {
    use super::*;

    #[test]
    fn a_focused_object_shows_both_directions_and_report_provenance() {
        let (code, out, _err) = object("'Sales'[Total]");

        assert_eq!(code, 0);
        assert_eq!(
            out,
            "'Sales'[Total]  measure

Dependencies
├─ table 'Sales'  table
│  └─ partition 'Sales'[Sales]  partition
└─ 'Sales'[Amount]  column
   └─ table 'Sales'  table  ↩ already shown

Impact

Model
└─ nothing

Reports
└─ Mini
   └─ P1
      └─ V1  visual
         └─ Values
"
        );
    }

    #[test]
    fn dependencies_narrows_to_upstream() {
        let (code, out, _err) = object_in("'Sales'[Total]", |args| args.dependencies = true);

        assert_eq!(code, 0);
        assert!(out.contains("Dependencies"));
        assert!(!out.contains("Impact"), "no downstream section");
    }

    #[test]
    fn impact_narrows_to_downstream() {
        let (code, out, _err) = object_in("'Sales'[Total]", |args| args.impact = true);

        assert_eq!(code, 0);
        assert!(out.contains("Impact"));
        assert!(!out.contains("Dependencies"), "no upstream section");
    }

    #[test]
    fn both_flags_mean_the_default_view() {
        let (code, out, _err) = object_in("'Sales'[Total]", |args| {
            args.dependencies = true;
            args.impact = true;
        });

        assert_eq!(code, 0);
        assert!(out.contains("Dependencies") && out.contains("Impact"));
    }

    #[test]
    fn an_empty_impact_is_a_zero_exit_and_says_so() {
        let (code, out, _err) = object("'Sales'[Legacy Total]");

        assert_eq!(code, 0);
        assert!(out.contains("Impact\n└─ nothing"));
    }

    #[test]
    fn a_shared_node_is_expanded_once() {
        let (code, out, _err) = object("'Sales'[Legacy Total]");

        assert_eq!(code, 0);
        assert!(
            out.contains("└─ table 'Sales'  table  ↩ already shown"),
            "the table appears twice in the chain; the second is marked:\n{out}"
        );
        assert_eq!(out.matches("partition 'Sales'[Sales]").count(), 1);
    }

    #[test]
    fn depth_one_keeps_direct_producers_only() {
        let (code, out, _err) =
            object_in("'Sales'[Total]", |args| args.depth = Some("1".to_string()));

        assert_eq!(code, 0);
        assert!(out.contains("'Sales'[Amount]  column"));
        assert!(
            !out.contains("partition"),
            "the partition is two edges from the measure:\n{out}"
        );
    }

    #[test]
    fn impact_of_a_column_names_its_consumer() {
        let (code, out, _err) = object("'Sales'[Legacy]");

        assert_eq!(code, 0);
        assert!(out.contains("Model\n└─ 'Sales'[Legacy Total]  measure"));
        assert!(!out.contains("Reports"), "nothing binds the dead column");
    }
}

mod lookup {
    use super::*;

    #[test]
    fn a_typo_suggests_the_nearest_object() {
        let (code, _out, err) = object("'Sales'[Totla]");

        assert_eq!(code, 2);
        assert!(err.contains("error: object \"'Sales'[Totla]\" was not found"));
        assert!(err.contains("Did you mean:"));
        assert!(err.contains("'Sales'[Total]"));
        assert!(err.contains("hint:"));
    }

    #[test]
    fn a_bare_column_name_suggests_the_qualified_form() {
        let (code, _out, err) = object("Amount");

        assert_eq!(code, 2);
        assert!(err.contains("was not found"));
        assert!(
            err.contains("'Sales'[Amount]"),
            "the nearest match is offered: {err}"
        );
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let (code, out, err) = object("'SALES'[total]");
        assert_eq!(code, 0, "stderr: {err}");
        assert!(out.starts_with("'Sales'[Total]  measure"));
    }
}

mod overview {
    use super::*;

    #[test]
    fn the_bare_command_prints_counts_not_the_graph() {
        let dir = mini_project();

        let (code, out, _err) = run_deps(&DepsArgs::default(), &dir.0, "");

        assert_eq!(code, 0);
        assert_eq!(
            out,
            "Dependency graph

6 model objects
7 dependency edges
1 report bindings

By type
  Measures      2
  Columns       2
  Hierarchies   0
  Relationships 0
  Other         2

Try:
  ripbi deps \"'Table'[Name]\"
  ripbi deps --table Sales
  ripbi deps --type measure
"
        );
    }
}

mod inputs {
    use super::*;

    #[test]
    fn model_only_input_is_valid() {
        let dir = TempDir::new("deps-model-only");
        let model = model_into(&dir.0, "Solo");
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            model: Some(model),
            ..DepsArgs::default()
        };

        let (code, out, err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0, "stderr: {err}");
        assert!(err.contains("with no reports"));
        assert!(out.contains("'Sales'[Total]  measure"));
        assert!(out.contains("Dependencies"));
        assert!(!out.contains("Reports"), "no report bindings exist:\n{out}");
    }

    #[test]
    fn a_model_argument_explores_without_a_project_discovery_step() {
        let dir = TempDir::new("deps-model-arg");
        let model = model_into(&dir.0, "Solo");
        let args = DepsArgs {
            object: Some("'Sales'[Amount]".to_string()),
            model: Some(model),
            ..DepsArgs::default()
        };

        let (code, out, err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(
            !err.contains("Discovered"),
            "explicit --model is picker-free: {err}"
        );
        assert!(out.contains("'Sales'[Amount]  column"));
    }

    #[test]
    fn no_project_here_is_an_error_with_a_hint() {
        let dir = TempDir::new("deps-empty");

        let (code, _out, err) = run_deps(&DepsArgs::default(), &dir.0, "");

        assert_eq!(code, 2);
        assert!(err.contains("no Power BI projects found"));
        assert!(err.contains("hint:"));
    }

    #[test]
    fn quiet_prints_nothing() {
        let dir = mini_project();
        let args = DepsArgs {
            quiet: true,
            ..DepsArgs::default()
        };

        let (code, out, err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.is_empty());
        assert!(err.is_empty());
    }
}

mod machine_modes {
    use super::*;

    #[test]
    fn plain_records_are_stable_one_per_line() {
        let (code, out, _err) = object_in("'Sales'[Total]", |args| args.plain = true);

        assert_eq!(code, 0);
        let expected = [
            "dependency\ttable 'Sales'\tpartition 'Sales'[Sales]\ttable_partition",
            "dependency\t'Sales'[Amount]\ttable 'Sales'\ttable_member",
            "dependency\t'Sales'[Total]\ttable 'Sales'\ttable_member",
            "dependency\t'Sales'[Total]\t'Sales'[Amount]\tmeasure_expression",
            "binding\t'Sales'[Total]\tMini\tP1\tV1\tValues",
        ];
        assert_eq!(out, expected.join("\n") + "\n");
    }

    #[test]
    fn plain_impact_records_flip_the_orientation() {
        let args = DepsArgs {
            object: Some("'Sales'[Legacy]".to_string()),
            plain: true,
            ..DepsArgs::default()
        };
        let dir = mini_project();

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        // The queried object comes first: it is the used side.
        assert!(
            out.contains("impact\t'Sales'[Legacy]\t'Sales'[Legacy Total]\tmeasure_expression\n"),
            "{out}"
        );
    }

    #[test]
    fn plain_overview_is_count_records() {
        let dir = mini_project();

        let (code, out, _err) = run_deps(
            &DepsArgs {
                plain: true,
                ..DepsArgs::default()
            },
            &dir.0,
            "",
        );

        assert_eq!(code, 0);
        let expected = [
            "objects\t6",
            "edges\t7",
            "bindings\t1",
            "measures\t2",
            "columns\t2",
            "hierarchies\t0",
            "relationships\t0",
            "other\t2",
        ];
        assert_eq!(out, expected.join("\n") + "\n");
    }

    #[test]
    fn json_exposes_the_typed_graph() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            json: true,
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        let payload = json_payload(&out);
        assert_eq!(payload["schema_version"], 1);
        assert_eq!(payload["root"]["id"], "'Sales'[Total]");
        assert_eq!(payload["root"]["type"], "measure");
        assert_eq!(payload["nodes"].as_array().expect("nodes").len(), 4);
        let edges = payload["edges"].as_array().expect("edges");
        assert!(edges.iter().all(|edge| edge["kind"] == "dependency"));
        assert!(
            edges
                .iter()
                .any(|edge| edge["provenance"] == "measure_expression"),
            "edges keep typed provenance keys"
        );
        let bindings = payload["bindings"].as_array().expect("bindings");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0]["report"], "Mini");
        assert_eq!(bindings[0]["page"], "P1");
        assert_eq!(bindings[0]["visual"], "V1");
        assert_eq!(bindings[0]["binding"], "Values");
        assert_eq!(bindings[0]["kind"], "visual_binding");
        assert_eq!(bindings[0]["bookmark"], serde_json::Value::Null);
        assert_eq!(bindings[0]["mobile"], false);
    }

    #[test]
    fn json_overview_carries_the_counts() {
        let dir = mini_project();

        let (code, out, _err) = run_deps(
            &DepsArgs {
                json: true,
                ..DepsArgs::default()
            },
            &dir.0,
            "",
        );

        assert_eq!(code, 0);
        let payload = json_payload(&out);
        assert_eq!(payload["schema_version"], 1);
        assert_eq!(payload["objects"], 6);
        assert_eq!(payload["edges"], 7);
        assert_eq!(payload["bindings"], 1);
        assert_eq!(payload["by_type"]["measures"], 2);
        assert_eq!(payload["by_type"]["other"], 2);
    }
}

mod selectors {
    use super::*;

    #[test]
    fn a_type_selector_explores_every_match() {
        let dir = mini_project();
        let args = DepsArgs {
            types: vec!["measure".to_string()],
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.contains("'Sales'[Total]  measure"));
        assert!(out.contains("'Sales'[Legacy Total]  measure"));
        // A root header sits on its own line after a blank line; tree nodes
        // carry a connector, so this only matches headers.
        assert!(
            !out.contains("\n\n'Sales'[Amount]  column\n"),
            "columns are not roots:\n{out}"
        );
    }

    #[test]
    fn a_table_selector_roots_every_member() {
        let dir = mini_project();
        let args = DepsArgs {
            table: Some("Sales".to_string()),
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        for root in [
            "table 'Sales'  table",
            "'Sales'[Amount]  column",
            "'Sales'[Legacy]  column",
            "'Sales'[Total]  measure",
        ] {
            assert!(out.contains(root), "{root} must be a root:\n{out}");
        }
        // Traversal is not pruned: the partition is reached from its table.
        assert!(out.contains("partition 'Sales'[Sales]"));
    }

    #[test]
    fn table_and_type_selectors_intersect() {
        let dir = mini_project();
        let args = DepsArgs {
            table: Some("Sales".to_string()),
            types: vec!["measure".to_string()],
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.contains("'Sales'[Total]  measure"));
        assert!(
            !out.contains("\n\n'Sales'[Amount]  column\n"),
            "columns are not roots:\n{out}"
        );
    }

    #[test]
    fn selector_runs_resolve_cross_table_reach_in_principle() {
        // One table only here — the assertion is that a selector root's tree
        // still shows everything it reaches (nothing is hidden for being a
        // different kind than the selector named).
        let dir = mini_project();
        let args = DepsArgs {
            table: Some("Sales".to_string()),
            types: vec!["measure".to_string()],
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(
            out.contains("table 'Sales'  table"),
            "the measure's tree still reaches its table:\n{out}"
        );
    }

    #[test]
    fn an_unknown_type_lists_the_vocabulary() {
        let dir = mini_project();
        let args = DepsArgs {
            types: vec!["measures".to_string()],
            ..DepsArgs::default()
        };

        let (code, _out, err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 2);
        assert!(err.contains("--type measures is not an object type"));
        assert!(err.contains("calculation_item"), "the vocabulary is listed");
    }

    #[test]
    fn a_selector_matching_nothing_is_an_error() {
        let dir = mini_project();
        let args = DepsArgs {
            table: Some("Nope".to_string()),
            ..DepsArgs::default()
        };

        let (code, _out, err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 2);
        assert!(err.contains("no objects match"));
    }

    #[test]
    fn json_of_a_selector_run_has_a_null_root() {
        let dir = mini_project();
        let args = DepsArgs {
            types: vec!["measure".to_string()],
            json: true,
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        let payload = json_payload(&out);
        assert_eq!(payload["root"], serde_json::Value::Null);
        assert_eq!(payload["nodes"].as_array().expect("nodes").len(), 6);
    }
}

mod filters {
    use super::*;

    #[test]
    fn consumer_visual_shows_report_usage_only() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            consumer: Some("visual".to_string()),
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.contains("Reports"));
        assert!(
            !out.contains("\nModel\n"),
            "the model section steps aside:\n{out}"
        );
    }

    #[test]
    fn consumer_of_a_model_kind_hides_report_usage() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Legacy]".to_string()),
            consumer: Some("measure".to_string()),
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.contains("'Sales'[Legacy Total]  measure"));
        assert!(
            !out.contains("Reports"),
            "a binding is not a measure consumer"
        );
    }

    #[test]
    fn an_unmatched_consumer_is_nothing_still_exit_zero() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            consumer: Some("measure".to_string()),
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.contains("Impact\n└─ nothing"));
    }

    #[test]
    fn in_report_keeps_matching_bindings_only() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            in_report: Some("Mini".to_string()),
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.contains("Reports"));
    }

    #[test]
    fn in_report_filtered_to_nothing_is_still_success() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            in_report: Some("Executive".to_string()),
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.contains("Impact\n└─ nothing"));
    }

    #[test]
    fn on_page_matches_the_binding_page() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            on_page: Some("P1".to_string()),
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.contains("Reports"));
    }

    #[test]
    fn filters_with_dependencies_only_are_a_contradiction() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            dependencies: true,
            in_report: Some("Mini".to_string()),
            ..DepsArgs::default()
        };

        let (code, _out, err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 2);
        assert!(err.contains("filter the Impact view"));
        assert!(err.contains("hint: drop --dependencies"));
    }

    #[test]
    fn an_unknown_consumer_kind_lists_the_vocabulary() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            consumer: Some("visuals".to_string()),
            ..DepsArgs::default()
        };

        let (code, _out, err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 2);
        assert!(err.contains("--consumer visuals is not a consumer kind"));
        assert!(err.contains("visual"));
    }

    #[test]
    fn plain_impact_records_obey_the_filters() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            consumer: Some("visual".to_string()),
            plain: true,
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.contains("binding\t"), "bindings remain:\n{out}");
        assert!(
            !out.starts_with("impact\t"),
            "model usage records are filtered out"
        );
    }
}

mod graph_view {
    use super::*;

    #[test]
    fn graph_draws_the_topology_deterministically() {
        let dir = mini_project();
        let args = DepsArgs {
            object: Some("'Sales'[Total]".to_string()),
            graph: true,
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");
        assert_eq!(code, 0);

        let (_code2, out2, _err2) = run_deps(&args, &dir.0, "");
        assert_eq!(out, out2, "stable output between invocations");

        assert!(out.contains("'Sales'[Total]  measure"));
        assert!(out.contains("Dependencies") && out.contains("Impact"));
        assert!(out.contains('┌'), "boxes are drawn");
        assert!(
            out.contains("['Sales'[Total]]") || out.contains("Total"),
            "nodes are named"
        );
    }

    #[test]
    fn graph_of_a_large_view_degrades_gracefully() {
        // The whole fixture graph fits, so force the selector side: every
        // member of Sales draws compactly rather than flooding.
        let dir = mini_project();
        let args = DepsArgs {
            table: Some("Sales".to_string()),
            graph: true,
            ..DepsArgs::default()
        };

        let (code, out, _err) = run_deps(&args, &dir.0, "");

        assert_eq!(code, 0);
        assert!(out.contains('['), "nodes carry numbered or named forms");
    }
}

mod depth_parsing {
    use super::*;

    #[test]
    fn zero_is_a_usage_error_with_the_fix() {
        let (code, _out, err) =
            object_in("'Sales'[Total]", |args| args.depth = Some("0".to_string()));

        assert_eq!(code, 2);
        assert!(err.contains("--depth must be at least 1"));
        assert!(err.contains("hint: use --depth 1"));
    }

    #[test]
    fn a_non_number_is_a_usage_error() {
        let (code, _out, err) = object_in("'Sales'[Total]", |args| {
            args.depth = Some("deep".to_string())
        });

        assert_eq!(code, 2);
        assert!(err.contains("--depth deep is not a number or 'all'"));
    }
}

mod derived_inputs {
    use super::*;

    #[test]
    fn a_report_flag_alone_derives_the_model() {
        let temp = TempDir::new("deps-derive");
        model_into(&temp.0, "Sales");
        model_into(&temp.0, "Decoy");
        let report = report_into(
            &temp.0,
            "Standalone.Report",
            Some(&by_connection(
                "Data Source=powerbi://x;Initial Catalog=Sales",
            )),
        );

        let args = DepsArgs {
            reports: vec![report],
            ..DepsArgs::default()
        };
        let (code, _out, err) = run_deps(&args, &temp.0, "");

        assert_eq!(code, 0, "{err}");
        assert!(
            err.contains("Exploring") && err.contains("Sales.SemanticModel"),
            "the derived model is announced:
{err}"
        );
    }
}
