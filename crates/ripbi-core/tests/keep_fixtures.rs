//! The `ripbi_keep` annotation (issue #151) reads identically from TMDL,
//! TMSL `model.bim`, and the ABF/PBIX `DataModel` catalog, and a kept object
//! is a reachability root in every one of them.

mod common;

use std::path::PathBuf;

use common::{Backup, decoded_backup, metadata_db};
use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest::semantic_model;
use ripbi_core::{NameKey, ObjectId, TabularDatabase};

fn fixture(parts: &[&str]) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    path.extend(parts);
    path
}

fn table(name: &str) -> ObjectId {
    ObjectId::Table {
        table: NameKey::new(name),
    }
}

fn column(table: &str, column: &str) -> ObjectId {
    ObjectId::Column {
        table: NameKey::new(table),
        column: NameKey::new(column),
    }
}

fn measure(table: &str, measure: &str) -> ObjectId {
    ObjectId::Measure {
        table: NameKey::new(table),
        measure: NameKey::new(measure),
    }
}

fn kept(model: &TabularDatabase) -> Vec<(String, String)> {
    model
        .kept_objects()
        .into_iter()
        .map(|(id, reason)| (id.to_string(), reason.to_string()))
        .collect()
}

#[test]
fn tmdl_reads_every_kept_object_with_its_reason() {
    let ingested = semantic_model(&fixture(&["tmdl", "keep", "Keep.SemanticModel"])).unwrap();
    assert!(ingested.skips.is_empty(), "{:#?}", ingested.skips);
    let expected = [
        ("'Sales'[Key]", "Case does not matter"),
        (
            "'Sales'[Excel Margin %]",
            "Used by the Finance Excel pivot (FIN-231)",
        ),
        ("hierarchy 'Sales'[Drill]", "Kept for an upcoming report"),
        ("table 'Archive'", ""),
        ("calculation item 'Periods'[YTD]", "Excel slicer"),
        (
            "relationship 'Sales'[Key] -> 'Archive'[Id]",
            "USERELATIONSHIP in the Excel workbook",
        ),
        ("expression 'Raw'", "Read by a dataflow"),
        ("function 'Double'", "Shared DAX library"),
    ];
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(id, reason)| (id.to_string(), reason.to_string()))
        .collect();
    assert_eq!(kept(&ingested.value), expected);
}

#[test]
fn tmsl_matches_tmdl() {
    let tmdl = semantic_model(&fixture(&["tmdl", "keep", "Keep.SemanticModel"])).unwrap();
    let mut tmsl = semantic_model(&fixture(&["tmsl", "keep", "model.bim"])).unwrap();
    assert!(tmsl.skips.is_empty(), "{:#?}", tmsl.skips);
    tmsl.value.name = tmdl.value.name.clone();
    assert_eq!(kept(&tmsl.value), kept(&tmdl.value));
}

/// With no report at all, the kept objects and everything they reference are
/// live; the rest is not.
#[test]
fn kept_objects_are_roots() {
    let model = semantic_model(&fixture(&["tmdl", "keep", "Keep.SemanticModel"]))
        .unwrap()
        .value;
    let graph = DependencyGraph::build(&model, &[]);
    let unused: Vec<ObjectId> = graph
        .unused_objects()
        .into_iter()
        .map(|finding| finding.id)
        .collect();
    for live in [
        measure("Sales", "Excel Margin %"),
        measure("Sales", "Margin"),
        column("Sales", "Amount"),
        column("Sales", "Cost"),
        column("Sales", "Key"),
        table("Sales"),
        table("Archive"),
        column("Archive", "Id"),
        table("Periods"),
    ] {
        assert!(!unused.contains(&live), "{live} must be live");
    }
    let unused: Vec<String> = unused.iter().map(ToString::to_string).collect();
    assert_eq!(unused, ["calculation item 'Periods'[MTD]"]);
    assert_eq!(
        graph.kept_by(&column("Archive", "Id")).map(|k| &k.id),
        Some(&table("Archive")),
        "a kept table carries its members"
    );
}

#[test]
fn the_abf_catalog_reads_keep_annotations() {
    // ObjectType codes: 3 table, 4 column, 7 relationship, 8 measure,
    // 9 hierarchy, 41 expression, 47 calculation item, 63 function.
    let db = metadata_db(
        r#"
        CREATE TABLE "Annotation" (ID INTEGER, ObjectID INTEGER, ObjectType INTEGER,
            Name TEXT, Value TEXT);
        INSERT INTO "Annotation" VALUES
            (1, 20, 3, 'ripbi_keep', 'Excel'),
            (2, 11, 4, 'ripbi_keep', NULL),
            (3, 15, 8, 'Ripbi_Keep', 'Finance pivot'),
            (4, 60, 9, 'ripbi_keep', 'h'),
            (5, 50, 7, 'ripbi_keep', 'r'),
            (6, 80, 41, 'ripbi_keep', 'e'),
            (7, 402, 47, 'ripbi_keep', 'i'),
            (8, 90, 63, 'ripbi_keep', 'f'),
            (9, 12, 4, 'SummarizationSetBy', 'Automatic'),
            (10, 13, 8, 'ripbi_keep', 'wrong type: 13 is a column, not a measure');
        "#,
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model.abf");
    std::fs::write(&path, decoded_backup(&db, &Backup::default())).unwrap();
    let model = semantic_model(&path).unwrap().value;
    let got = kept(&model);
    let expected = [
        ("'Sales'[Amount]", ""),
        ("'Sales'[Total]", "Finance pivot"),
        ("table 'Product'", "Excel"),
        ("hierarchy 'Product'[Products]", "h"),
        ("calculation item 'Time Intelligence'[YTD]", "i"),
        ("relationship 'Sales'[ProductKey] -> 'Product'[Key]", "r"),
        ("expression 'Raw'", "e"),
        ("function 'Double'", "f"),
    ];
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(id, reason)| (id.to_string(), reason.to_string()))
        .collect();
    assert_eq!(got, expected);
}
