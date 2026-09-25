//! The PBIT was exported from Microsoft's public 2026 AdventureWorks PBIX.

use std::path::PathBuf;

use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest::{report, semantic_model};

fn samples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples")
}

#[test]
fn adventureworks_pbit_matches_its_pbip_conversion() {
    let root = samples();
    let archive = root.join("AdventureWorks Sales.pbit");
    let tmdl = semantic_model(&root.join("AdventureWorks Sales.SemanticModel")).unwrap();
    let tmsl = semantic_model(&archive).unwrap();
    let pbir = report(&root.join("AdventureWorks Sales.Report")).unwrap();
    let archived_report = report(&archive).unwrap();

    assert!(tmsl.skips.is_empty(), "{:#?}", tmsl.skips);
    assert!(
        archived_report.skips.is_empty(),
        "{:#?}",
        archived_report.skips
    );
    assert_eq!(tmsl.value.tables.len(), tmdl.value.tables.len());
    assert_eq!(
        archived_report.value.bindings().len(),
        pbir.value.bindings().len()
    );

    let model_m = tmdl.value.m_expressions();
    let archive_m = tmsl.value.m_expressions();
    assert_eq!(archive_m.len(), model_m.len());
    let mut expected_m: Vec<_> = model_m
        .iter()
        .map(|item| (item.owner.to_object_id(), item.text))
        .collect();
    let mut actual_m: Vec<_> = archive_m
        .iter()
        .map(|item| (item.owner.to_object_id(), item.text))
        .collect();
    expected_m.sort();
    actual_m.sort();
    assert_eq!(actual_m, expected_m);

    let original = DependencyGraph::build(&tmdl.value, &[&pbir.value]);
    let exported = DependencyGraph::build(&tmsl.value, &[&archived_report.value]);
    assert_eq!(exported.unused_objects(), original.unused_objects());
}
