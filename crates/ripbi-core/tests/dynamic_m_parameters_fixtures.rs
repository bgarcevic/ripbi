//! Integration tests for the dynamic-m-parameters fixture: a scrubbed real
//! Power BI Desktop model (issue #50) with one dynamic M query parameter —
//! `MinDays`, bound from `DaysList[Days]` via `parameterValuesColumn` and
//! consumed by the SampleData partition's M. The whole model is one live
//! chain, so nothing in it may be flagged unused.

use std::path::PathBuf;

use ripbi_core::graph::{DependencyGraph, Provenance, StructuralEdge};
use ripbi_core::identity::{NameKey, ObjectId};
use ripbi_core::ingest::semantic_model;
use ripbi_core::model::{ParameterValuesColumn, TabularDatabase};
use ripbi_core::report::{
    DatasetReference, FieldTarget, FieldWell, Page, Projection, ReportModel, Visual,
};

fn fixture_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    for group in [
        "fixtures",
        "tmdl",
        "dynamic-m-parameters",
        "DynamicMParameters.SemanticModel",
    ] {
        path.push(group);
    }
    path
}

/// The model's only live chain: a visual on SampleData[Days]. Nothing binds
/// DaysList[Days] directly — it survives through the parameter binding alone.
fn report() -> ReportModel {
    ReportModel {
        name: Some("Probe Report".to_string()),
        dataset: DatasetReference::Unresolved,
        pages: vec![Page {
            name: NameKey::new("P1"),
            display_name: None,
            is_hidden: false,
            filters: Vec::new(),
            binding: None,
            visuals: vec![Visual {
                name: NameKey::new("Slicer"),
                visual_type: "slicer".to_string(),
                wells: vec![FieldWell {
                    role: "Fields".to_string(),
                    projections: vec![Projection {
                        target: FieldTarget::Column {
                            table: NameKey::new("SampleData"),
                            column: NameKey::new("Days"),
                        },
                        query_ref: None,
                        active: true,
                    }],
                }],
                filters: Vec::new(),
                sorts: Vec::new(),
                conditional_formatting: Vec::new(),
                alt_text: Vec::new(),
                tooltip_page: None,
            }],
        }],
        ..Default::default()
    }
}

fn expression_id(name: &str) -> ObjectId {
    ObjectId::Expression {
        name: NameKey::new(name),
    }
}

fn column_id(table: &str, column: &str) -> ObjectId {
    ObjectId::Column {
        table: NameKey::new(table),
        column: NameKey::new(column),
    }
}

fn partition_id(table: &str, partition: &str) -> ObjectId {
    ObjectId::Partition {
        table: NameKey::new(table),
        partition: NameKey::new(partition),
    }
}

/// The real Desktop shape ingests with zero drift: DirectQuery partitions,
/// `sourceProviderType`, `valueFilterBehavior`, and the column-side
/// `ParameterMetadata` marker are all recognized, while the binding itself is
/// modeled — the parameter names its column.
#[test]
fn the_real_shape_ingests_with_the_binding_recorded_and_no_drift() {
    let ingested = semantic_model(&fixture_path()).expect("fixture parses");
    assert!(
        ingested.skips.is_empty(),
        "the validated shape must not surface as drift: {:?}",
        ingested.skips
    );

    let db: &TabularDatabase = &ingested.value;
    assert_eq!(db.expressions.len(), 1);
    assert_eq!(
        db.expressions[0].parameter_values_column,
        Some(ParameterValuesColumn {
            table: "DaysList".to_string(),
            column: "Days".to_string(),
        })
    );
}

/// End to end: the report makes the SampleData side live, its M keeps MinDays
/// alive, and the binding keeps the never-referenced DaysList[Days] alive —
/// the exact false "unused" finding issue #50 is about.
#[test]
fn the_binding_keeps_the_bound_column_alive_end_to_end() {
    let db = semantic_model(&fixture_path())
        .expect("fixture parses")
        .value;
    let report = report();

    let graph = DependencyGraph::build(&db, &[&report]);
    assert!(
        graph.unused_objects().is_empty(),
        "every object sits on the one live chain: {:?}",
        graph.unused_objects()
    );

    // The bound column's only consumer is the parameter, via the new
    // structural edge; the parameter's consumer is the consuming partition.
    assert_eq!(
        graph.consumers_of(&column_id("DaysList", "Days")),
        [(
            expression_id("MinDays"),
            Provenance::Structural {
                role: StructuralEdge::MParameterBinding,
            }
        )]
    );
    assert_eq!(
        graph.consumers_of(&expression_id("MinDays")),
        [(partition_id("SampleData", "SampleData"), Provenance::M)]
    );
}
