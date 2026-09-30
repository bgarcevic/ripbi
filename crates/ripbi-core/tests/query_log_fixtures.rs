//! Workspace-monitoring query logs: the synthetic export mirrors a real
//! `SemanticModelLogs` CSV (BOM, every column quoted, `""`-escaped JSON,
//! engine noise rows, the `[WaitTime]` suffix), and the queries it logs are
//! reachability roots once the graph is built with them.

use std::path::PathBuf;

use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest::semantic_model;
use ripbi_core::usage::{QueryLanguage, read_query_log};
use ripbi_core::{NameKey, ObjectId};

fn log_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/query-log/semantic-model-logs.csv")
}

fn mini_model() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ripbi-cli/tests/fixtures/mini-pbip/Mini.SemanticModel")
}

fn measure(table: &str, measure: &str) -> ObjectId {
    ObjectId::Measure {
        table: NameKey::new(table),
        measure: NameKey::new(measure),
    }
}

#[test]
fn the_export_keeps_only_query_end_rows() {
    let log = read_query_log(&log_path()).expect("reads");
    assert!(log.skips.is_empty(), "{:?}", log.skips);
    let log = log.value;
    assert_eq!(log.rows, 9);
    assert_eq!(log.queries.len(), 3);
    assert_eq!(log.item_names, ["Mini"]);
    assert_eq!(
        log.first_seen.as_deref(),
        Some("2026-09-28 08:15:01.0300000")
    );
    assert_eq!(
        log.last_seen.as_deref(),
        Some("2026-09-30 09:45:00.0000000")
    );

    let dax = &log.queries[0];
    assert_eq!(dax.language, QueryLanguage::Dax);
    assert!(dax.text.ends_with("EVALUATE   __DS0Core"), "{}", dax.text);
    assert_eq!(dax.sources.len(), 1);
    assert_eq!(
        dax.sources[0].report_id.as_deref(),
        Some("00000000-0000-4000-8000-00000000000a")
    );
    assert_eq!(log.queries[1].language, QueryLanguage::Mdx);
    assert_eq!(log.queries[1].application.as_deref(), Some("Excel"));
}

#[test]
fn an_excel_query_keeps_a_measure_no_report_binds() {
    let model = semantic_model(&mini_model()).expect("ingests").value;
    let log = read_query_log(&log_path()).expect("reads").value;
    let unused = |graph: &DependencyGraph| -> Vec<String> {
        graph
            .unused_objects()
            .iter()
            .map(|unused| unused.id.to_string())
            .collect()
    };

    let without = DependencyGraph::build(&model, &[]);
    assert!(unused(&without).contains(&"'Sales'[Legacy Total]".to_string()));

    let with = DependencyGraph::build_with_queries(&model, &[], &log);
    let dead = unused(&with);
    assert!(
        !dead.contains(&"'Sales'[Legacy Total]".to_string()),
        "{dead:?}"
    );
    assert!(!dead.contains(&"'Sales'[Legacy]".to_string()), "{dead:?}");

    let legacy = with
        .queried_by(&measure("Sales", "Legacy Total"))
        .expect("queried");
    assert_eq!(legacy.count, 2);
    assert_eq!(legacy.users, 1);
    assert_eq!(legacy.applications, ["Excel"]);
    assert!(legacy.reports.is_empty());

    let total = with
        .queried_by(&measure("Sales", "Total"))
        .expect("queried");
    assert_eq!(total.count, 1);
    assert_eq!(total.reports, ["00000000-0000-4000-8000-00000000000a"]);
}
