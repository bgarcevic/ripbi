//! Fixture tests for TMDL ingestion: a golden model parsed exactly, and
//! resilience fixtures asserting both directions — drift parses *and* is
//! noticed, deliberately-unmodeled metadata stays silent.

use std::path::PathBuf;

use ripbi_core::ingest::{SkipKind, semantic_model};
use ripbi_core::model::{
    CalculationGroup, CalculationItem, Column, ColumnKind, Hierarchy, HierarchyLevel, Kpi, Measure,
    Partition, PartitionSource, Relationship, Role, SharedExpression, Table, TablePermission,
    TabularDatabase,
};

fn fixture(groups: &[&str]) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tmdl");
    for group in groups {
        path.push(group);
    }
    path
}

/// The `Mini` golden model, hand-built: what `Mini.SemanticModel` must parse
/// into, field for field.
fn golden_database() -> TabularDatabase {
    TabularDatabase {
        name: Some("Mini".to_string()),
        tables: vec![
            Table {
                name: "Sales".to_string(),
                columns: vec![
                    Column {
                        name: "SalesOrderLineKey".to_string(),
                        kind: ColumnKind::Data,
                        is_hidden: true,
                        ..Default::default()
                    },
                    Column {
                        name: "Sales Amount".to_string(),
                        kind: ColumnKind::Data,
                        ..Default::default()
                    },
                    Column {
                        name: "Margin %".to_string(),
                        kind: ColumnKind::Calculated {
                            expression: "DIVIDE([Sales Amount], 10)".to_string(),
                        },
                        sort_by_column: Some("Sales Amount".to_string()),
                        ..Default::default()
                    },
                ],
                measures: vec![
                    Measure {
                        name: "Sales".to_string(),
                        expression: "SUM('Sales'[Sales Amount])".to_string(),
                        ..Default::default()
                    },
                    Measure {
                        name: "Growth %".to_string(),
                        expression: "VAR prior = CALCULATE([Sales], DATEADD('Date'[Date], -1, YEAR))\nRETURN DIVIDE([Sales] - prior, prior)".to_string(),
                        is_hidden: true,
                        format_string_expression: Some("\"0.0%;-0.0%;0.0%\"".to_string()),
                        detail_rows_expression: Some(
                            "SELECTCOLUMNS('Sales', \"Key\", [SalesOrderLineKey])".to_string(),
                        ),
                        kpi: Some(Kpi {
                            target_expression: Some("[Sales Budget]".to_string()),
                            status_expression: Some("IF([Sales] > [Sales Budget], 1, -1)".to_string()),
                            trend_expression: Some("[Sales] - [Sales Budget]".to_string()),
                        }),
                    },
                ],
                partitions: vec![Partition {
                    name: "Sales-ab12cd34".to_string(),
                    source: PartitionSource::M {
                        expression: "let\n    Source = Excel.Workbook(File.Contents(\"C:\\Data\\Mini.xlsx\"), null, true),\n\n    Sales_Table = Source{[Item=\"Sales\",Kind=\"Table\"]}[Data]\nin\n    Sales_Table".to_string(),
                    },
                }],
                hierarchies: vec![Hierarchy {
                    name: "Fiscal".to_string(),
                    levels: vec![
                        HierarchyLevel {
                            name: "Year".to_string(),
                            column: "YearNum".to_string(),
                        },
                        HierarchyLevel {
                            name: "Month Name".to_string(),
                            column: "Month Name".to_string(),
                        },
                    ],
                    is_hidden: false,
                }],
                ..Default::default()
            },
            Table {
                name: "Sales Order".to_string(),
                columns: vec![Column {
                    name: "It's quoted".to_string(),
                    kind: ColumnKind::Data,
                    ..Default::default()
                }],
                partitions: vec![Partition {
                    name: "SalesOrder-cd34ef56".to_string(),
                    source: PartitionSource::M {
                        expression: "Sql.Database(\"server\", \"db\")".to_string(),
                    },
                }],
                ..Default::default()
            },
            Table {
                name: "Time Intelligence".to_string(),
                columns: vec![Column {
                    name: "Time Intelligence".to_string(),
                    kind: ColumnKind::Data,
                    ..Default::default()
                }],
                calculation_group: Some(CalculationGroup {
                    items: vec![
                        CalculationItem {
                            name: "Current".to_string(),
                            expression: "SELECTEDMEASURE()".to_string(),
                            format_string_expression: None,
                        },
                        CalculationItem {
                            name: "YoY %".to_string(),
                            expression: "DIVIDE(SELECTEDMEASURE(), CALCULATE(SELECTEDMEASURE(), SAMEPERIODLASTYEAR('Date'[Date])))".to_string(),
                            format_string_expression: Some("\"0%;-0%;0%\"".to_string()),
                        },
                    ],
                    no_selection_expression: Some("SELECTEDMEASURE()".to_string()),
                    no_selection_format_string_expression: Some(
                        "SELECTEDMEASUREFORMATSTRING()".to_string(),
                    ),
                    multiple_or_empty_selection_expression: Some(
                        "ERROR(\"Pick exactly one\")".to_string(),
                    ),
                    multiple_or_empty_selection_format_string_expression: Some(
                        "\"General\"".to_string(),
                    ),
                }),
                partitions: vec![Partition {
                    name: "Partition_Time Intelligence".to_string(),
                    source: PartitionSource::Other {
                        kind: Some("calculationGroup".to_string()),
                    },
                }],
                ..Default::default()
            },
        ],
        relationships: vec![
            Relationship {
                name: Some("00000000-0000-0000-0000-000000000001".to_string()),
                from_table: "Sales".to_string(),
                from_column: "SalesOrderLineKey".to_string(),
                to_table: "Sales Order".to_string(),
                to_column: "SalesOrderLineKey".to_string(),
                is_active: true,
            },
            Relationship {
                name: Some("00000000-0000-0000-0000-000000000002".to_string()),
                from_table: "Sales Order".to_string(),
                from_column: "SalesOrder".to_string(),
                to_table: "Customer".to_string(),
                to_column: "SalesOrder".to_string(),
                is_active: true,
            },
            Relationship {
                name: Some("00000000-0000-0000-0000-000000000003".to_string()),
                from_table: "Sales".to_string(),
                from_column: "DueDateKey".to_string(),
                to_table: "Date".to_string(),
                to_column: "DueDateKey".to_string(),
                is_active: false,
            },
        ],
        roles: vec![Role {
            name: "Administrators".to_string(),
            table_permissions: vec![
                TablePermission {
                    table: "Sales".to_string(),
                    filter_expression: Some("FILTER('Sales', [Sales Amount] > 0)".to_string()),
                },
                TablePermission {
                    table: "Sales Order".to_string(),
                    filter_expression: None,
                },
            ],
        }],
        expressions: vec![
            SharedExpression {
                name: "ServerName".to_string(),
                expression: "\"localhost\"".to_string(),
            },
            SharedExpression {
                name: "Calendar".to_string(),
                expression: "let\n    StartDate = #date(2024, 1, 1),\n    EndDate = #date(2024, 12, 31)\nin\n    {StartDate, EndDate}".to_string(),
            },
        ],
        functions: Vec::new(),
    }
}

#[test]
fn golden_model_parses_exactly_with_no_skips() {
    let ingested =
        semantic_model(&fixture(&["golden", "Mini.SemanticModel"])).expect("golden fixture parses");

    assert_eq!(ingested.value, golden_database());
    assert!(
        ingested.skips.is_empty(),
        "deliberately-unmodeled metadata must be silent: {:#?}",
        ingested.skips
    );
}

#[test]
fn accepts_the_definition_folder_directly() {
    let item = fixture(&["golden", "Mini.SemanticModel"]);
    let direct = semantic_model(&item.join("definition")).expect("definition/ parses");
    let via_item = semantic_model(&item).expect("item folder parses");

    assert_eq!(direct.value, via_item.value);
    // `.platform` is still found beside the definition folder.
    assert_eq!(direct.value.name.as_deref(), Some("Mini"));
}

#[test]
fn rejects_a_folder_that_is_not_a_semantic_model() {
    let error = semantic_model(&fixture(&["golden"]));
    assert!(error.is_err(), "the fixtures root has no definition/");
}

#[test]
fn unknown_property_parses_and_is_noticed_once() {
    let ingested = semantic_model(&fixture(&["resilience", "unknown-property"]))
        .expect("drift must not fail the parse");

    assert_eq!(ingested.value.tables.len(), 1);
    assert_eq!(ingested.value.tables[0].name, "Sales");
    assert_eq!(ingested.value.tables[0].columns[0].name, "Amount");

    assert_eq!(ingested.skips.len(), 1);
    let skip = &ingested.skips[0];
    assert_eq!(skip.kind, SkipKind::UnknownProperty);
    assert_eq!(skip.location.as_deref(), Some("line 7"));
    assert!(skip.detail.contains("aiHint"), "{}", skip.detail);
    assert_eq!(
        skip.path.file_name().and_then(|n| n.to_str()),
        Some("Sales.tmdl")
    );
}

#[test]
fn unknown_object_block_parses_and_is_noticed_once() {
    let ingested = semantic_model(&fixture(&["resilience", "unknown-object"]))
        .expect("drift must not fail the parse");

    assert_eq!(ingested.value.tables.len(), 1);

    assert_eq!(ingested.skips.len(), 1);
    let skip = &ingested.skips[0];
    assert_eq!(skip.kind, SkipKind::UnknownObject);
    assert_eq!(skip.location.as_deref(), Some("line 1"));
    assert!(skip.detail.contains("group"), "{}", skip.detail);
    assert_eq!(
        skip.path.file_name().and_then(|n| n.to_str()),
        Some("extras.tmdl")
    );
}

#[test]
fn malformed_relationship_is_dropped_and_noticed() {
    let ingested = semantic_model(&fixture(&["resilience", "malformed-relationship"]))
        .expect("drift must not fail the parse");

    assert!(
        ingested.value.relationships.is_empty(),
        "the unreadable relationship must not reach the AST"
    );
    assert_eq!(ingested.skips.len(), 1);
    assert_eq!(ingested.skips[0].kind, SkipKind::MalformedValue);
    assert_eq!(ingested.skips[0].location.as_deref(), Some("line 1"));
}
