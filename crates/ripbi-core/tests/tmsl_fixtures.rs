use std::path::PathBuf;

use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest::report;
use ripbi_core::ingest::semantic_model;

#[test]
fn golden_tmsl_matches_tmdl() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let tmdl = semantic_model(&root.join("tmdl/golden/Mini.SemanticModel")).unwrap();
    let mut tmsl = semantic_model(&root.join("tmsl/golden/model.bim")).unwrap();
    // A bare model.bim has no database name; the PBIP item's .platform names it.
    tmsl.value.name = tmdl.value.name.clone();
    assert_eq!(tmsl.value, tmdl.value);
    assert!(tmsl.skips.is_empty(), "{:#?}", tmsl.skips);

    let report = report(&root.join("pbir/golden/Mini.Report")).unwrap().value;
    let from_tmdl = DependencyGraph::build(&tmdl.value, &[&report]);
    let from_tmsl = DependencyGraph::build(&tmsl.value, &[&report]);
    assert_eq!(from_tmsl.unused_objects(), from_tmdl.unused_objects());
}

#[test]
fn dynamic_m_parameter_binding_matches_tmdl() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let tmdl =
        semantic_model(&root.join("tmdl/dynamic-m-parameters/DynamicMParameters.SemanticModel"))
            .unwrap();
    let tmsl = semantic_model(&root.join("tmsl/dynamic-m-parameters/model.bim")).unwrap();
    assert_eq!(tmsl.value.expressions, tmdl.value.expressions);
    assert_eq!(tmsl.value.tables, tmdl.value.tables);
}
