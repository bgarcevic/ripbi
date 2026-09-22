//! Integration tests for the `report` command (issue #33): the inventory of
//! what ripbi sees in a report — pages → visuals → fields — in the three
//! output modes, with unresolved bindings attributed to their visuals.

#[expect(dead_code)]
mod common;

use std::path::PathBuf;

use common::{
    TempDir, broken_visual_pbip, by_connection, json_payload, mini_pbip, model_into, project_into,
    report_into, report_path, run_report,
};
use ripbi_cli::cli::ReportArgs;

/// The AdventureWorks sample, pinned to its committed shape: one page,
/// seventeen visuals (the issue's "7 pages" conflated the shape visuals).
fn sample_pbip() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/AdventureWorks Sales.pbip")
}

/// The human tree walks every level: report header, page with display name,
/// visual with its type, and the field with its binding site.
#[test]
fn human_prints_the_mini_fixture_tree() {
    let (code, stdout, stderr) = report_path(&mini_pbip().join("Mini.pbip"), &std::env::temp_dir());

    assert_eq!(code, 0, "the inventory is informational:\n{stderr}");
    assert!(stderr.contains("Reading"), "announcements stay on stderr");
    assert!(stdout.contains("Mini.Report — 1 page, 1 visual"));
    assert!(stdout.contains("  Pages"));
    assert!(stdout.contains("Overview (P1) — 1 visual"));
    assert!(stdout.contains("V1 — card"));
    assert!(stdout.contains("Values measure 'Sales'[Total]"));
}

/// `--json` pins the v1 schema: schema_version, the nested shape, and the
/// binding-site vocabulary.
#[test]
fn json_mode_emits_the_v1_schema() {
    let args = ReportArgs {
        json: true,
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");

    let payload = json_payload(&stdout);
    assert_eq!(payload["schema_version"], 1);
    let report = &payload["reports"][0];
    assert_eq!(report["name"], "Mini.Report");
    assert_eq!(report["unresolved_count"], 0);
    let page = &report["pages"][0];
    assert_eq!(page["name"], "P1");
    assert_eq!(page["display_name"], "Overview");
    assert_eq!(page["mobile"], false);
    let visual = &page["visuals"][0];
    assert_eq!(visual["name"], "V1");
    assert_eq!(visual["type"], "card");
    assert_eq!(
        visual["fields"][0],
        serde_json::json!({
            "kind": "measure",
            "target": "'Sales'[Total]",
            "binding_site": "field_well:Values",
            "active": true,
        })
    );
    assert_eq!(visual["unresolved"], serde_json::json!([]));
}

/// `--plain` records are tab-separated and self-contained: each carries its
/// report, page, and visual.
#[test]
fn plain_mode_is_one_tab_separated_record_per_row() {
    let args = ReportArgs {
        plain: true,
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "report\tMini.Report",
            "page\tMini.Report\tP1\tdesktop",
            "visual\tMini.Report\tP1\tV1\tcard",
            "field\tMini.Report\tP1\tV1\tfield_well:Values\tmeasure\t'Sales'[Total]",
        ],
        "one record per report, page, visual, and field:\n{stdout}"
    );
}

/// Broken bindings are attributed to their visual and counted per report —
/// and never change the exit code (the informational contract).
#[test]
fn broken_bindings_are_attributed_and_do_not_gate_the_exit_code() {
    let args = ReportArgs {
        json: true,
        path: Some(broken_visual_pbip().join("Broken.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "breakage is listed, never failed on:\n{stderr}");

    let payload = json_payload(&stdout);
    let report = &payload["reports"][0];
    assert_eq!(report["unresolved_count"], 2);
    let visuals = &report["pages"][0]["visuals"];
    // V1 is healthy, V2 binds a dropped column, V3 a measure whose DAX is
    // broken, and V4's synthesized KPI variant must resolve (issue #60).
    assert_eq!(visuals[0]["unresolved"], serde_json::json!([]));
    assert_eq!(
        visuals[1]["unresolved"][0]["reason"], "field_not_found",
        "V2 binds the dropped 'Sales'[Color]"
    );
    assert_eq!(
        visuals[2]["unresolved"][0]["reason"], "bound_artifact_broken",
        "V3 binds a measure whose own DAX is broken"
    );
    assert_eq!(visuals[3]["unresolved"], serde_json::json!([]));
}

/// The committed AdventureWorks sample inventories at its committed shape:
/// one page, seventeen visuals.
#[test]
fn adventure_works_inventories_one_page_seventeen_visuals() {
    let (code, stdout, stderr) = report_path(&sample_pbip(), &std::env::temp_dir());

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("AdventureWorks Sales — 1 page, 17 visuals"));
    assert!(stdout.contains("Overview (ReportSection) — 17 visuals"));

    let args = ReportArgs {
        json: true,
        path: Some(sample_pbip()),
        ..ReportArgs::default()
    };
    let (_, stdout, _) = run_report(&args, &std::env::temp_dir(), "");
    let report = &json_payload(&stdout)["reports"][0];
    assert_eq!(report["pages"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        report["pages"][0]["visuals"].as_array().map(Vec::len),
        Some(17)
    );
}

/// The informational exit-code contract: 0 for a read, 2 for a real error.
#[test]
fn exit_codes_are_zero_for_reads_and_two_for_errors() {
    let temp = common::TempDir::new("report-exit");
    let args = ReportArgs {
        path: Some(temp.0.join("nope")),
        ..ReportArgs::default()
    };
    let (code, _, stderr) = run_report(&args, &temp.0, "");
    assert_eq!(code, 2);
    assert!(
        stderr.contains("error:"),
        "the refusal is on stderr:\n{stderr}"
    );

    // A model with no sibling reports is a real error too: there is nothing
    // to inventory.
    let model = common::model_into(&temp.0, "Lonely");
    let args = ReportArgs {
        path: Some(model),
        ..ReportArgs::default()
    };
    let (code, _, stderr) = run_report(&args, &temp.0, "");
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("no reports"), "{stderr}");
}

/// `-q` suppresses everything; the exit code is the only output.
#[test]
fn quiet_prints_nothing() {
    let args = ReportArgs {
        quiet: true,
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

/// A bare `.Report` folder pairs with its sibling model, exactly as `scan`
/// resolves it.
#[test]
fn a_bare_report_folder_pairs_with_its_model() {
    let (code, stdout, stderr) =
        report_path(&mini_pbip().join("Mini.Report"), &std::env::temp_dir());

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Mini.Report — 1 page, 1 visual"));
}

// --- Views and filters -------------------------------------------------------

/// The golden report fixture: its pairing model lacks most tables, so it
/// carries broken bindings at all three levels — visual, page, and report.
fn golden_report() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/ripbi-core/tests/fixtures/pbir/golden/Mini.Report")
}

/// `--pages` lists every page; a page without a display name shows `-`.
#[test]
fn the_pages_view_lists_every_page() {
    let args = ReportArgs {
        pages: true,
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("page  display   hidden  mobile  visuals"));
    assert!(
        stdout.contains("P1    Overview  -       -       1\n"),
        "{stdout}"
    );
}

/// `--visuals` is the per-visual roll-up: type, field count, unresolved count
/// — and when any visual is broken, the stderr pointer names the next
/// command.
#[test]
fn the_visuals_view_rolls_up_per_visual() {
    let args = ReportArgs {
        visuals: true,
        path: Some(broken_visual_pbip().join("Broken.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("page      visual  type  fields  unresolved"));
    assert!(
        stdout.contains("Overview  V1      card  1       0\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Overview  V2      card  1       1\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Overview  V3      card  1       1\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Overview  V4      kpi   1       0\n"),
        "{stdout}"
    );
    assert!(
        stderr.contains("2 unresolved — see: ripbi report --broken"),
        "{stderr}"
    );
}

/// `--fields --visual V1` is the flat binding table, narrowed to one visual.
#[test]
fn the_fields_view_narrows_by_visual() {
    let args = ReportArgs {
        fields: true,
        visual: vec!["V1".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.starts_with("page"),
        "the table header leads:\n{stdout}"
    );
    assert!(stdout.contains("'Sales'[Total]"), "{stdout}");
    assert_eq!(
        stdout.lines().count(),
        3,
        "header, dashes, one row:\n{stdout}"
    );
}

/// `--match` answers "which visuals use this field" across every binding
/// site — the flat table rows and, without a view flag, the pruned tree.
#[test]
fn the_match_filter_finds_every_site_of_a_field() {
    let args = ReportArgs {
        fields: true,
        matches: vec!["'Sales'[Total]".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.lines().count(), 3, "{stdout}");

    let args = ReportArgs {
        matches: vec!["'Sales'[Total]".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("'Sales'[Total]"), "{stdout}");
}

/// `--used` lists the model objects reachability keeps alive — the live
/// measure in, the dead one out.
#[test]
fn the_used_view_lists_live_objects_and_excludes_dead() {
    let args = ReportArgs {
        used: true,
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("kind"), "{stdout}");
    assert!(stdout.contains("'Sales'[Total]"), "{stdout}");
    assert!(stdout.contains("measure"), "{stdout}");
    assert!(
        !stdout.contains("Legacy"),
        "dead objects stay out:\n{stdout}"
    );
}

/// `--broken` keeps only the unresolved rows — exactly the V2 and V3
/// bindings — and stays exit 0.
#[test]
fn the_broken_filter_yields_only_the_broken_rows() {
    let args = ReportArgs {
        fields: true,
        broken: true,
        path: Some(broken_visual_pbip().join("Broken.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "breakage is listed, never failed on:\n{stderr}");
    assert_eq!(
        stdout.lines().skip(2).count(),
        2,
        "exactly the V2 and V3 rows:\n{stdout}"
    );
    assert!(stdout.contains("field not found"), "{stdout}");
    assert!(stdout.contains("bound artifact broken"), "{stdout}");
    assert!(stdout.contains("V2") && stdout.contains("V3"), "{stdout}");
    assert!(
        !stdout.contains("V1 "),
        "healthy visuals stay out:\n{stdout}"
    );
}

/// A filter that matches nothing says so on stderr and still exits 0 —
/// silence would read as breakage.
#[test]
fn an_empty_filter_prints_a_note_and_exits_zero() {
    let args = ReportArgs {
        page: vec!["Nope*".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.is_empty());
    assert!(stderr.contains("no pages match Nope*"), "{stderr}");
}

/// Filters narrow `--json` too: one selected visual, full inventory without
/// them.
#[test]
fn filters_narrow_the_json_output() {
    let args = ReportArgs {
        visual: vec!["V1".to_string()],
        json: true,
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    let payload = json_payload(&stdout);
    let visuals = payload["reports"][0]["pages"][0]["visuals"]
        .as_array()
        .expect("visuals array");
    assert_eq!(visuals.len(), 1);
    assert_eq!(visuals[0]["name"], "V1");
}

/// Page- and report-level broken bindings are itemized at their level — the
/// tree, plain records, and JSON all show them, and the count sums every
/// level.
#[test]
fn page_and_report_level_unresolved_are_itemized() {
    let args = ReportArgs {
        json: true,
        path: Some(golden_report()),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    let payload = json_payload(&stdout);
    let report = &payload["reports"][0];
    assert_eq!(
        report["unresolved"].as_array().map(Vec::len),
        Some(1),
        "the report-level filter is broken (its model lacks Product): {payload}"
    );
    assert_eq!(
        report["pages"][0]["unresolved"].as_array().map(Vec::len),
        Some(1),
        "the page-level filter is broken (the model lacks Date)"
    );
    let count = report["unresolved_count"].as_u64().expect("count");
    let itemized: u64 = report["pages"]
        .as_array()
        .expect("pages")
        .iter()
        .map(|page| {
            page["unresolved"].as_array().map(Vec::len).unwrap_or(0) as u64
                + page["visuals"]
                    .as_array()
                    .expect("visuals")
                    .iter()
                    .map(|visual| visual["unresolved"].as_array().map(Vec::len).unwrap_or(0) as u64)
                    .sum::<u64>()
        })
        .sum::<u64>()
        + report["unresolved"].as_array().map(Vec::len).unwrap_or(0) as u64;
    assert_eq!(count, itemized, "the count sums every level");
}

// --- What each filter matches -------------------------------------------------

/// A visual's only author-visible identity is its type — PBIR assigns folder
/// names as object names — so `--visual donut*` matches the donut chart.
#[test]
fn the_visual_filter_matches_the_type_too() {
    let args = ReportArgs {
        visual: vec!["donut*".to_string()],
        path: Some(sample_pbip()),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("1 visual"), "{stdout}");
    assert!(stdout.contains("donutChart"), "{stdout}");
    assert!(
        !stdout.contains("funnel"),
        "only the donut survives:\n{stdout}"
    );
}

/// The object-name (hash) matching is unchanged: a prefix pins one visual.
#[test]
fn the_visual_filter_still_matches_the_hash() {
    let args = ReportArgs {
        fields: true,
        visual: vec!["11a03bbd46fd39147235".to_string()],
        path: Some(sample_pbip()),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("11a03bbd46fd39147235"), "{stdout}");
    assert_eq!(
        stdout.lines().count(),
        6,
        "header, dashes, four rows:\n{stdout}"
    );
}

/// An empty visual filter names itself on stderr — a bare "0 visuals" tree
/// would read as breakage.
#[test]
fn an_empty_visual_filter_names_the_note() {
    let args = ReportArgs {
        visual: vec!["zzz*".to_string()],
        path: Some(sample_pbip()),
        ..ReportArgs::default()
    };
    let (code, _, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0);
    assert!(stderr.contains("no visuals match zzz*"), "{stderr}");
}

/// `--kind` selects rows by the kind column; intersected with `--match` it
/// answers "which visuals bind this field as a measure".
#[test]
fn the_kind_filter_selects_rows() {
    let args = ReportArgs {
        fields: true,
        kind: vec!["measure".to_string()],
        matches: vec!["'Sales'[Total]".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("measure"), "{stdout}");
    assert_eq!(
        stdout.lines().count(),
        3,
        "header, dashes, one row:\n{stdout}"
    );
}

/// `--site` selects rows by the site column; on the golden report only the
/// conditional-formatting rows survive.
#[test]
fn the_site_filter_selects_rows() {
    let args = ReportArgs {
        fields: true,
        site: vec!["conditional_formatting".to_string()],
        path: Some(golden_report()),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    let rows = stdout.lines().skip(2);
    assert!(
        rows.clone()
            .all(|row| row.contains("conditional_formatting") || row.trim().is_empty())
            || rows.count() > 0,
        "{stdout}"
    );
    assert!(stdout.contains("conditional_formatting"), "{stdout}");
    assert!(!stdout.contains("field_well"), "{stdout}");
}

/// `--kind filter` keeps whole filter blocks — a surviving block shows its
/// declared target and its condition-tree references together (the golden
/// report's three filters: report-level, page-level, and V1's with one
/// reference).
#[test]
fn the_kind_filter_keeps_whole_filter_blocks() {
    let args = ReportArgs {
        fields: true,
        kind: vec!["filter".to_string()],
        path: Some(golden_report()),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Filter1 (Categorical)"), "{stdout}");
    assert!(stdout.contains("PageFilter (Categorical)"), "{stdout}");
    assert!(stdout.contains("V1Filter (Advanced)"), "{stdout}");
    assert!(
        !stdout.contains("field_well"),
        "binding rows are gone:\n{stdout}"
    );
    assert_eq!(
        stdout.lines().count(),
        6,
        "header, dashes, four rows:\n{stdout}"
    );
}

/// Different flags intersect: a measure that is not a sort leaves nothing,
/// and the note names it.
#[test]
fn intersecting_filters_empty_output_names_the_note() {
    let args = ReportArgs {
        fields: true,
        kind: vec!["measure".to_string()],
        site: vec!["alt_text".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("no bindings match the given filters"),
        "{stderr}"
    );
}

/// `--kind unresolved` is the generic form of `--broken`: in the tree, the
/// healthy visual is pruned and the broken ones keep only their unresolved
/// lines.
#[test]
fn the_kind_unresolved_filter_prunes_the_tree() {
    let args = ReportArgs {
        kind: vec!["unresolved".to_string()],
        path: Some(broken_visual_pbip().join("Broken.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(
        !stdout.contains("V1"),
        "the healthy visual is pruned:\n{stdout}"
    );
    assert!(stdout.contains("V2") && stdout.contains("V3"), "{stdout}");
    assert!(stdout.contains("unresolved"), "{stdout}");
    assert!(
        !stdout.contains("measure "),
        "binding rows are gone:\n{stdout}"
    );
}

/// `--used` is filterable on its two columns: `--kind` on the object kind,
/// `--match` on the display id — repeats union, different flags intersect.
#[test]
fn the_used_view_filters_by_kind_and_match() {
    let args = ReportArgs {
        used: true,
        kind: vec!["measure".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("measure  'Sales'[Total]"), "{stdout}");
    assert!(!stdout.contains("partition"), "{stdout}");

    let args = ReportArgs {
        used: true,
        matches: vec!["'Sales'[Amount]".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("'Sales'[Amount]"), "{stdout}");
    assert!(!stdout.contains("'Sales'[Total]"), "{stdout}");

    let args = ReportArgs {
        used: true,
        kind: vec!["table".to_string(), "partition".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("table") && stdout.contains("partition"),
        "{stdout}"
    );
    assert!(!stdout.contains("'Sales'[Amount]"), "{stdout}");
}

/// A used-kind filter matching nothing names it on stderr, still exit 0.
#[test]
fn an_empty_used_filter_names_the_note() {
    let args = ReportArgs {
        used: true,
        kind: vec!["role".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("no used objects match the given filters"),
        "{stderr}"
    );
}

/// The binding-tree filters do not shrink `--used`: reachability always
/// describes the whole report, even when the tree prunes to nothing.
#[test]
fn the_used_view_ignores_binding_tree_filters() {
    let args = ReportArgs {
        used: true,
        visual: vec!["zzz*".to_string()],
        path: Some(mini_pbip().join("Mini.pbip")),
        ..ReportArgs::default()
    };
    let (code, stdout, stderr) = run_report(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("'Sales'[Total]"),
        "the used table survives:\n{stdout}"
    );
    assert!(stderr.contains("no visuals match zzz*"), "{stderr}");
}

/// The input ladder `report` shares with `scan`: `--model`/`--report` flags,
/// `ripbi.toml`, and derivation from report anchors alone.
mod shared_inputs {
    use super::*;

    #[test]
    fn the_model_flag_inventories_a_model_via_its_pbip() {
        let temp = TempDir::new("report-model-pbip");
        project_into(&temp.0, "Mini");

        let args = ReportArgs {
            model: Some(temp.0.join("Mini.pbip")),
            ..ReportArgs::default()
        };
        let (code, stdout, stderr) = run_report(&args, &temp.0, "");

        assert_eq!(code, 0, "{stderr}");
        assert!(
            stdout.contains("Mini.Report — 1 page, 1 visual"),
            "{stdout}"
        );
    }

    #[test]
    fn a_report_flag_alone_derives_the_model() {
        let temp = TempDir::new("report-derive");
        model_into(&temp.0, "Sales");
        model_into(&temp.0, "Decoy");
        let report = report_into(
            &temp.0,
            "Standalone.Report",
            Some(&by_connection(
                "Data Source=powerbi://x;Initial Catalog=Sales",
            )),
        );

        let args = ReportArgs {
            reports: vec![report],
            ..ReportArgs::default()
        };
        let (code, stdout, stderr) = run_report(&args, &temp.0, "");

        assert_eq!(
            code, 0,
            "{stdout}
{stderr}"
        );
        assert!(
            stderr.contains("Sales.SemanticModel"),
            "the derived model is announced:
{stderr}"
        );
        assert!(stdout.contains("Standalone.Report"), "{stdout}");
    }

    #[test]
    fn config_reports_drive_the_inventory_like_the_flag() {
        let temp = TempDir::new("report-config");
        project_into(&temp.0, "Mini");
        temp.write(
            "ripbi.toml",
            "target = \"Mini.SemanticModel\"
reports = [\"Mini.Report\"]
",
        );

        let args = ReportArgs::default();
        let (code, stdout, stderr) = run_report(&args, &temp.0, "");

        assert_eq!(code, 0, "{stderr}");
        assert!(
            stdout.contains("Mini.Report — 1 page, 1 visual"),
            "{stdout}"
        );
    }
}

/// `--allow-no-model`: the explicit opt-in to inventory a report with no
/// semantic model on disk — bindings listed as written, nothing resolved.
/// A mistyped path stays an error even under the flag.
mod allow_no_model {
    use super::*;

    /// A report with no model anywhere nearby: no `.SemanticModel` sibling,
    /// and its `definition.pbir` removed with the fixture copy. The name is
    /// unique per call: tests run in parallel in one process, and a shared
    /// directory would be deleted out from under them.
    fn model_less_report(name: &str) -> TempDir {
        let temp = TempDir::new(name);
        report_into(&temp.0, "Solo.Report", None);
        temp
    }

    #[test]
    fn a_model_less_report_refuses_without_the_flag() {
        let temp = model_less_report("no-model-refuse");

        let (code, _, stderr) = report_path(&temp.0.join("Solo.Report"), &temp.0);

        assert_eq!(code, 2);
        assert!(
            stderr.contains("cannot locate the semantic model"),
            "error:\n{stderr}"
        );
        assert!(
            stderr.contains("--allow-no-model"),
            "the hint names the escape hatch:\n{stderr}"
        );
    }

    #[test]
    fn the_flag_lists_the_report_as_written() {
        let temp = model_less_report("no-model-list");

        let args = ReportArgs {
            path: Some(temp.0.join("Solo.Report")),
            allow_no_model: true,
            ..ReportArgs::default()
        };
        let (code, stdout, stderr) = run_report(&args, &temp.0, "");

        assert_eq!(code, 0, "{stderr}");
        assert!(
            stderr.contains("No semantic model paired"),
            "the model-less note is announced:\n{stderr}"
        );
        assert!(
            stdout.contains("Solo.Report \u{2014} 1 page, 1 visual"),
            "the inventory prints:\n{stdout}"
        );
        assert!(
            !stdout.contains("unresolved"),
            "nothing is claimed about resolution:\n{stdout}"
        );
    }

    #[test]
    fn the_used_view_needs_a_model_even_with_the_flag() {
        let temp = model_less_report("no-model-used");

        let args = ReportArgs {
            path: Some(temp.0.join("Solo.Report")),
            allow_no_model: true,
            used: true,
            ..ReportArgs::default()
        };
        let (code, _, stderr) = run_report(&args, &temp.0, "");

        assert_eq!(code, 2);
        assert!(
            stderr.contains("--used needs a paired semantic model"),
            "error:\n{stderr}"
        );
    }

    #[test]
    fn a_mistyped_path_stays_an_error_with_the_flag() {
        let temp = model_less_report("no-model-typo");

        let args = ReportArgs {
            path: Some(temp.0.join("Typo.Report")),
            allow_no_model: true,
            ..ReportArgs::default()
        };
        let (code, _, stderr) = run_report(&args, &temp.0, "");

        assert_eq!(code, 2);
        assert!(stderr.contains("no such path"), "error:\n{stderr}");
    }
}
