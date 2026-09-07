//! Integration tests for the dead-chain fixture: a model engineered so every
//! liveness rule has one object exercising it, paired with a hand-built report
//! that keeps exactly one chain alive. The golden expectations assert the
//! exact unused set and the chain annotations.

use std::path::PathBuf;

use ripbi_core::graph::{DependencyGraph, Provenance, StructuralEdge};
use ripbi_core::identity::{NameKey, ObjectId};
use ripbi_core::ingest::semantic_model;
use ripbi_core::model::{DaxExpressionKind, TabularDatabase};
use ripbi_core::report::{
    DatasetReference, FieldTarget, FieldWell, Filter, Page, Projection, ReportMeasure, ReportModel,
    Visual,
};

fn fixture(groups: &[&str]) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    for group in groups {
        path.push(group);
    }
    path
}

fn table_id(name: &str) -> ObjectId {
    ObjectId::Table {
        table: NameKey::new(name),
    }
}

fn column_id(table: &str, column: &str) -> ObjectId {
    ObjectId::Column {
        table: NameKey::new(table),
        column: NameKey::new(column),
    }
}

fn measure_id(table: &str, measure: &str) -> ObjectId {
    ObjectId::Measure {
        table: NameKey::new(table),
        measure: NameKey::new(measure),
    }
}

fn hierarchy_id(table: &str, hierarchy: &str) -> ObjectId {
    ObjectId::Hierarchy {
        table: NameKey::new(table),
        hierarchy: NameKey::new(hierarchy),
    }
}

fn partition_id(table: &str, partition: &str) -> ObjectId {
    ObjectId::Partition {
        table: NameKey::new(table),
        partition: NameKey::new(partition),
    }
}

fn calculation_item_id(table: &str, item: &str) -> ObjectId {
    ObjectId::CalculationItem {
        table: NameKey::new(table),
        item: NameKey::new(item),
    }
}

fn expression_id(name: &str) -> ObjectId {
    ObjectId::Expression {
        name: NameKey::new(name),
    }
}

fn report_measure_id(name: &str) -> ObjectId {
    ObjectId::ReportMeasure {
        measure: NameKey::new(name),
    }
}

/// The finding for `id`, panicking with a readable message when absent.
fn find<'a>(unused: &'a [ripbi_core::UnusedObject], id: &ObjectId) -> &'a ripbi_core::UnusedObject {
    unused
        .iter()
        .find(|finding| &finding.id == id)
        .unwrap_or_else(|| panic!("{id} expected in the unused set"))
}

fn not_unused(unused: &[ripbi_core::UnusedObject], id: &ObjectId) {
    assert!(
        !unused.iter().any(|finding| &finding.id == id),
        "{id} must be live"
    );
}

/// Ingests the dead-chain fixture: a model engineered so every liveness rule
/// has one object exercising it.
fn dead_chain() -> (TabularDatabase, ReportModel) {
    let db = semantic_model(&fixture(&["tmdl", "dead-chain", "DeadChain.SemanticModel"]))
        .expect("dead-chain fixture parses")
        .value;
    // The report that shares it, hand-built: one live card on `Total Revenue`,
    // one live report filter on `Status`, and one deliberately unbound report
    // measure.
    let report = ReportModel {
        name: Some("Chain Report".to_string()),
        dataset: DatasetReference::Unresolved,
        filters: vec![Filter {
            target: Some(FieldTarget::Column {
                table: NameKey::new("Sales"),
                column: NameKey::new("Status"),
            }),
            ..Default::default()
        }],
        pages: vec![Page {
            name: NameKey::new("P1"),
            display_name: None,
            is_hidden: false,
            filters: Vec::new(),
            binding: None,
            visuals: vec![Visual {
                name: NameKey::new("Card"),
                visual_type: "card".to_string(),
                wells: vec![FieldWell {
                    role: "Values".to_string(),
                    projections: vec![Projection {
                        target: FieldTarget::Measure {
                            home_table: Some(NameKey::new("Sales")),
                            measure: NameKey::new("Total Revenue"),
                        },
                        query_ref: None,
                        active: true,
                    }],
                }],
                filters: Vec::new(),
                sorts: Vec::new(),
                conditional_formatting: Vec::new(),
                tooltip_page: None,
            }],
        }],
        bookmarks: Vec::new(),
        measures: vec![ReportMeasure {
            name: NameKey::new("Local Total"),
            expression: "SUM('Sales'[Old Amount])".to_string(),
            format_string: None,
        }],
    };
    (db, report)
}

/// The engineered unused set: every object the two live bindings and the role
/// cannot reach, exactly as the liveness policy demands.
#[test]
fn the_dead_chain_produces_the_exact_unused_set() {
    let (db, report) = dead_chain();
    let graph = DependencyGraph::build(&db, &[&report]);
    let unused = graph.unused_objects();

    let mut expected = vec![
        // The dead `Date` hierarchy and its level columns. The Date table
        // itself stays live: the live YTD item references `'Date'[Date]`,
        // which matches nothing, so the table fallback keeps the qualifying
        // table (and its partition) alive.
        column_id("Date", "Year"),
        column_id("Date", "Month"),
        hierarchy_id("Date", "Calendar"),
        // The far table of the live relationship: dead, but its key column is
        // alive through the relationship endpoint.
        table_id("DimOld"),
        column_id("DimOld", "Notes"),
        partition_id("DimOld", "DimOld"),
        // The dead measures and their chains.
        measure_id("Sales", "Legacy Total"),
        measure_id("Sales", "Ghost Ref"),
        column_id("Sales", "Old Amount"),
        // The dead calculated column and the dead sort-by chain.
        column_id("Sales", "Flag"),
        column_id("Sales", "Month Name"),
        column_id("Sales", "Month Num"),
        // The calculation item no expression selects.
        calculation_item_id("Time Intelligence", "Current"),
        // The shared expression only the dead partition's M mentions.
        expression_id("LegacyParam"),
        // The report measure no visual binds.
        report_measure_id("Local Total"),
    ];
    expected.sort();

    let ids: Vec<ObjectId> = unused.iter().map(|finding| finding.id.clone()).collect();
    assert_eq!(ids, expected, "exact unused set for the dead chain");
}

/// Every object the policy keeps alive despite no direct report binding.
#[test]
fn the_dead_chain_keeps_policy_live_objects_alive() {
    let (db, report) = dead_chain();
    let graph = DependencyGraph::build(&db, &[&report]);
    let unused = graph.unused_objects();

    // The live chain: card → measure → column → table.
    not_unused(&unused, &measure_id("Sales", "Total Revenue"));
    not_unused(&unused, &column_id("Sales", "Amount"));
    not_unused(&unused, &table_id("Sales"));
    not_unused(&unused, &column_id("Sales", "Status"));
    // RLS keeps the filtered column alive.
    not_unused(&unused, &column_id("Sales", "Region"));
    // The YTD calculation item keeps its group table, field column, and
    // partition alive (extended resolution of `'Time Intelligence'[YTD]`),
    // and its stale `'Date'[Date]` reference keeps the qualifying Date
    // table — and through it the partition — alive too.
    not_unused(&unused, &calculation_item_id("Time Intelligence", "YTD"));
    not_unused(&unused, &table_id("Time Intelligence"));
    not_unused(
        &unused,
        &column_id("Time Intelligence", "Time Intelligence"),
    );
    not_unused(
        &unused,
        &partition_id("Time Intelligence", "Partition_Time Intelligence"),
    );
    not_unused(&unused, &table_id("Date"));
    not_unused(&unused, &partition_id("Date", "Date"));
    // The M chain keeps Staging Query alive, and with it ServerName — one
    // M-to-M hop past the partition that names the staging query.
    not_unused(&unused, &expression_id("Staging Query"));
    not_unused(&unused, &expression_id("ServerName"));
    // The structural survivors: Amount's sort-by and group-by columns, and
    // the calendar-bound column on the live Date table.
    not_unused(&unused, &column_id("Sales", "Amount Sort"));
    not_unused(&unused, &column_id("Sales", "Bucket"));
    not_unused(&unused, &column_id("Date", "Day"));
    // Relationship-kept key columns: live, but weakly.
    not_unused(&unused, &column_id("Sales", "Key"));
    not_unused(&unused, &column_id("DimOld", "Key"));
    not_unused(
        &unused,
        &ObjectId::Relationship {
            from_table: NameKey::new("Sales"),
            from_column: NameKey::new("Key"),
            to_table: NameKey::new("DimOld"),
            to_column: NameKey::new("Key"),
        },
    );
}

/// The annotations that power the `← only used by X (also unused)` lines.
#[test]
fn the_dead_chain_annotates_its_chains() {
    let (db, report) = dead_chain();
    let graph = DependencyGraph::build(&db, &[&report]);
    let unused = graph.unused_objects();

    // The far table's consumers: its live key column and its dead column.
    let dim_old = find(&unused, &table_id("DimOld"));
    assert_eq!(dim_old.used_by.len(), 2);
    let key = dim_old
        .used_by
        .iter()
        .find(|used| used.id == column_id("DimOld", "Key"))
        .expect("the key column contains the table");
    assert!(!key.also_unused, "the key column is live (weakly)");
    assert!(matches!(
        key.provenance,
        Provenance::Structural {
            role: StructuralEdge::TableMember
        }
    ));

    // `Old Amount` is kept by two dead measures at once: a model measure and
    // an unused report measure.
    let old_amount = find(&unused, &column_id("Sales", "Old Amount"));
    assert_eq!(old_amount.used_by.len(), 2);
    assert!(old_amount.used_by.iter().any(|used| {
        used.id == measure_id("Sales", "Legacy Total")
            && matches!(
                used.provenance,
                Provenance::Dax {
                    kind: DaxExpressionKind::Measure
                }
            )
            && used.also_unused
    }));
    assert!(old_amount.used_by.iter().any(|used| {
        used.id == report_measure_id("Local Total")
            && matches!(
                used.provenance,
                Provenance::Dax {
                    kind: DaxExpressionKind::ReportMeasure
                }
            )
            && used.also_unused
    }));

    // The sort-by chain names its sorted column.
    let month_num = find(&unused, &column_id("Sales", "Month Num"));
    assert_eq!(month_num.used_by.len(), 1);
    assert_eq!(month_num.used_by[0].id, column_id("Sales", "Month Name"));
    assert!(matches!(
        month_num.used_by[0].provenance,
        Provenance::Structural {
            role: StructuralEdge::SortByColumn
        }
    ));

    // The dead partition is the only reference keeping `LegacyParam` alive.
    let legacy_param = find(&unused, &expression_id("LegacyParam"));
    assert_eq!(legacy_param.used_by.len(), 1);
    assert_eq!(legacy_param.used_by[0].id, partition_id("DimOld", "DimOld"));
    assert!(matches!(legacy_param.used_by[0].provenance, Provenance::M));

    // Orphans: the chain root causes.
    for orphan in [
        measure_id("Sales", "Ghost Ref"),
        hierarchy_id("Date", "Calendar"),
        calculation_item_id("Time Intelligence", "Current"),
        report_measure_id("Local Total"),
    ] {
        assert_eq!(
            find(&unused, &orphan).used_by,
            Vec::new(),
            "{orphan} is an orphan"
        );
    }
}

/// The structural survivors are alive through exactly one edge each: the
/// sort-by, group-by, calendar, and M rules. These edges are the whole point
/// of the issue — a column referenced *only* through one of them is not dead.
#[test]
fn the_structural_survivors_are_kept_alive_by_their_own_edges() {
    let (db, report) = dead_chain();
    let graph = DependencyGraph::build(&db, &[&report]);

    let consumers = |id: &ObjectId| graph.consumers_of(id);

    // Amount Sort is kept alive only by the column it sorts.
    assert_eq!(
        consumers(&column_id("Sales", "Amount Sort")),
        [(
            column_id("Sales", "Amount"),
            Provenance::Structural {
                role: StructuralEdge::SortByColumn
            }
        )]
    );

    // Bucket is kept alive only by the column that groups by it.
    assert_eq!(
        consumers(&column_id("Sales", "Bucket")),
        [(
            column_id("Sales", "Amount"),
            Provenance::Structural {
                role: StructuralEdge::GroupByColumn
            }
        )]
    );

    // Day is kept alive only by its table's calendar — engine-managed with
    // the table, never dropped independently of it.
    assert_eq!(
        consumers(&column_id("Date", "Day")),
        [(
            table_id("Date"),
            Provenance::Structural {
                role: StructuralEdge::EngineManaged
            }
        )]
    );

    // Staging Query is kept alive only by the partition whose M names it, and
    // it alone keeps ServerName alive now — the shared-expression-to-
    // shared-expression edge.
    assert_eq!(
        consumers(&expression_id("Staging Query")),
        [(partition_id("Sales", "Sales"), Provenance::M)]
    );
    assert_eq!(
        consumers(&expression_id("ServerName")),
        [(expression_id("Staging Query"), Provenance::M)]
    );
}
