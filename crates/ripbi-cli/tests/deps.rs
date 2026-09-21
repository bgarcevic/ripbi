//! Integration tests of the `deps` command: the focused object view, the
//! overview, depth, lookup errors, and the input ladder it shares with scan.
//! Output assertions are exact strings — the view is a contract.

#[expect(dead_code)]
mod common;

use common::{TempDir, json_payload, model_into, project_into, run_deps};
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
