//! Integration over the workspace's Power BI samples: every `.SemanticModel`
//! folder parses, cleanly enough that the curated ignore list covers all of
//! its metadata, and AdventureWorks matches its known shape.

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_core::SkipNotice;
use ripbi_core::ingest::{Ingested, semantic_model};
use ripbi_core::model::{PartitionSource, TabularDatabase};

fn samples() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .expect("samples/ exists beside the workspace root")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".SemanticModel"))
        })
        .collect();
    found.sort();
    found
}

fn adventure_works() -> PathBuf {
    samples()
        .into_iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("AdventureWorks"))
        })
        .expect("AdventureWorks sample present")
}

#[test]
fn every_sample_parses_without_error() {
    let samples = samples();
    assert!(samples.len() >= 8, "expected the full sample set");

    for sample in &samples {
        let Ingested { value, skips }: Ingested<TabularDatabase> = semantic_model(sample)
            .unwrap_or_else(|error| panic!("{} failed to parse: {error}", sample.display()));
        assert!(
            !value.tables.is_empty(),
            "{} parsed no tables",
            sample.display()
        );
        assert!(
            !value.dax_expressions().is_empty() || !value.m_expressions().is_empty(),
            "{} produced no expressions",
            sample.display()
        );
        let _ = skips; // asserted precisely in the next test
    }
}

/// The curated ignore list must cover every property the samples carry: a
/// notice on a healthy model is noise, and noise trains people to ignore
/// notices — including the ones that matter.
#[test]
fn every_sample_parses_without_notices() {
    for sample in samples() {
        let ingested = semantic_model(&sample)
            .unwrap_or_else(|error| panic!("{} failed to parse: {error}", sample.display()));
        assert!(
            ingested.skips.is_empty(),
            "{} recorded drift: {:#?}",
            sample.display(),
            ingested.skips
        );
    }
}

#[test]
fn adventure_works_matches_its_known_shape() {
    let Ingested { value: db, skips } =
        semantic_model(&adventure_works()).expect("AdventureWorks parses");
    assert!(skips.is_empty());

    // The display name comes from .platform; TMDL records no usable name.
    assert_eq!(db.name.as_deref(), Some("AdventureWorks Sales"));

    // Table order follows model.tmdl's ref table directives.
    let names: Vec<&str> = db.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Customer",
            "Date",
            "Product",
            "Reseller",
            "Sales",
            "Sales Order",
            "Sales Territory",
            "Category",
            "Time Intelligence",
            "Date Role",
        ]
    );

    let sales = &db.tables[4];
    assert_eq!(sales.name, "Sales");
    assert_eq!(sales.measures.len(), 19);
    let measure = sales
        .measures
        .iter()
        .find(|m| m.name == "Sales")
        .expect("the Sales measure exists");
    assert_eq!(measure.expression, "SUM('Sales'[Sales Amount])");

    // One more measure lives on Date.
    let total_measures: usize = db.tables.iter().map(|t| t.measures.len()).sum();
    assert_eq!(total_measures, 20);

    // The M partition body is captured verbatim: multi-line, blank line
    // preserved, inner indentation intact.
    let PartitionSource::M { expression } = &sales.partitions[0].source else {
        panic!("the Sales partition is an M partition");
    };
    assert!(
        expression.starts_with("let\n"),
        "starts with the M let: {expression}"
    );
    assert!(expression.contains("Excel.Workbook"));
    assert!(expression.contains("Table.TransformColumnTypes"));
    assert!(expression.ends_with("#\"Changed Type\""));

    assert_eq!(db.relationships.len(), 9);
    assert_eq!(
        db.relationships.iter().filter(|r| !r.is_active).count(),
        2,
        "DueDateKey and ShipDateKey relationships are inactive"
    );
    let from_sales_order = db
        .relationships
        .iter()
        .find(|r| r.from_table == "Sales Order")
        .expect("the quoted 'Sales Order' reference parses");
    assert_eq!(
        (
            from_sales_order.from_column.as_str(),
            from_sales_order.to_table.as_str(),
            from_sales_order.to_column.as_str()
        ),
        ("SalesOrderLineKey", "Sales", "SalesOrderLineKey")
    );

    let group_table = |name: &str| {
        db.tables
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("table {name} exists"))
            .calculation_group
            .as_ref()
            .expect("a calculation group")
    };
    let time_intelligence = group_table("Time Intelligence");
    assert_eq!(time_intelligence.items.len(), 10);
    let yoy = time_intelligence
        .items
        .iter()
        .find(|item| item.name == "YoY %")
        .expect("the YoY % item exists");
    assert_eq!(
        yoy.format_string_expression.as_deref(),
        Some("\"0%;-0%;0%\"")
    );
    let date_role = group_table("Date Role");
    assert_eq!(date_role.items.len(), 3);
    assert!(
        date_role
            .items
            .iter()
            .any(|item| item.expression.contains("USERELATIONSHIP"))
    );

    // Both calc-group partitions use the drift catch-all with their kind kept.
    assert!(
        db.tables
            .iter()
            .any(|t| t.partitions.iter().any(|p| p.source
                == PartitionSource::Other {
                    kind: Some("calculationGroup".to_string())
                }))
    );

    assert!(!db.dax_expressions().is_empty());
    assert!(!db.m_expressions().is_empty());
}

/// Compile-time proof the notice type is reachable from integration tests
/// without pulling private modules.
#[test]
fn notices_are_public_data() {
    let ingested = semantic_model(&adventure_works()).expect("AdventureWorks parses");
    let _skips: &Vec<SkipNotice> = &ingested.skips;
}
