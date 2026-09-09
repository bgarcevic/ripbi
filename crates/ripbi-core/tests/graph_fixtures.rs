//! Integration tests for the dependency graph: the golden `Mini` model and
//! report pair must produce exactly the unused set its known bindings imply,
//! with the chain annotations the issue calls for.

use std::path::PathBuf;

use ripbi_core::graph::{DependencyGraph, Provenance, StructuralEdge};
use ripbi_core::identity::{NameKey, ObjectId};
use ripbi_core::ingest::{report, semantic_model};
use ripbi_core::model::TabularDatabase;
use ripbi_core::report::ReportModel;

fn fixture(groups: &[&str]) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    for group in groups {
        path.push(group);
    }
    path
}

fn golden_pair() -> (TabularDatabase, ReportModel) {
    let db = semantic_model(&fixture(&["tmdl", "golden", "Mini.SemanticModel"]))
        .expect("golden model parses")
        .value;
    let report = report(&fixture(&["pbir", "golden", "Mini.Report"]))
        .expect("golden report parses")
        .value;
    (db, report)
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

/// What the golden pair leaves unused, hand-derived from its known bindings:
/// the report resolves the `Sales` measure (the card), falls back to the
/// `Sales` table (the nonexistent `Units` column's qualifier), and the
/// administrator role keeps `Sales Order` alive via its metadata-only
/// permission and `Sales` via its filter. Everything else is dead.
#[test]
fn golden_unused_set_is_exact() {
    let (db, report) = golden_pair();
    let graph = DependencyGraph::build(&db, &[&report]);
    let unused = graph.unused_objects();

    let mut expected = vec![
        // The calculated column nothing references; its own reference target
        // (`Sales Amount`) is live through the measure.
        column_id("Sales", "Margin %"),
        // Hierarchy `Fiscal` is bound by nothing; its level columns follow.
        hierarchy_id("Sales", "Fiscal"),
        column_id("Sales", "YearNum"),
        column_id("Sales", "Month Name"),
        // The quoted-name column no expression or visual reaches.
        column_id("Sales Order", "It's quoted"),
        // The two inactive relationships name these keys but cannot keep
        // them: nothing but a live USERELATIONSHIP reference activates an
        // inactive relationship. `Sales`[SalesOrderLineKey] stays live — it
        // is also the active relationship's key.
        column_id("Sales", "DueDateKey"),
        column_id("Sales Order", "DueDateKey"),
        column_id("Sales Order", "SalesOrder"),
        // The unactivated inactive relationships are findings themselves.
        ObjectId::Relationship {
            from_table: NameKey::new("Sales"),
            from_column: NameKey::new("DueDateKey"),
            to_table: NameKey::new("Sales Order"),
            to_column: NameKey::new("DueDateKey"),
        },
        ObjectId::Relationship {
            from_table: NameKey::new("Sales Order"),
            from_column: NameKey::new("SalesOrder"),
            to_table: NameKey::new("Sales"),
            to_column: NameKey::new("SalesOrderLineKey"),
        },
        // `Growth %` is bound by no visual and named by no other DAX.
        measure_id("Sales", "Growth %"),
        // The report defines `Budget %` but no visual binds it: a dead report
        // measure.
        report_measure_id("Budget %"),
        // Shared expressions no M query mentions.
        expression_id("ServerName"),
        expression_id("Calendar"),
        // The calculation group is bound nowhere: both items, the group's
        // field column, and the table's partition follow the dead table.
        table_id("Time Intelligence"),
        calculation_item_id("Time Intelligence", "Current"),
        calculation_item_id("Time Intelligence", "YoY %"),
        column_id("Time Intelligence", "Time Intelligence"),
        partition_id("Time Intelligence", "Partition_Time Intelligence"),
    ];
    expected.sort();

    let ids: Vec<ObjectId> = unused.iter().map(|finding| finding.id.clone()).collect();
    assert_eq!(ids, expected, "exact unused set for the golden pair");
}

/// The relationships follow the activation rule: the active relationship and
/// its key columns — the hidden key columns on `Sales Order` included — stay
/// live with their tables, while the two inactive ones are unactivated (no
/// `USERELATIONSHIP` anywhere in the golden report) and are findings; see
/// `golden_inactive_relationship_keys_are_findings`.
#[test]
fn golden_relationships_and_key_columns_stay_live() {
    let (db, report) = golden_pair();
    let graph = DependencyGraph::build(&db, &[&report]);
    let unused = graph.unused_objects();

    not_unused(&unused, &table_id("Sales"));
    not_unused(&unused, &table_id("Sales Order"));
    not_unused(&unused, &column_id("Sales", "Sales Amount"));
    not_unused(&unused, &column_id("Sales", "SalesOrderLineKey"));
    not_unused(&unused, &column_id("Sales Order", "SalesOrderLineKey"));

    let relationship_id = |from_table: &str, from_column: &str, to_table: &str, to_column: &str| {
        ObjectId::Relationship {
            from_table: NameKey::new(from_table),
            from_column: NameKey::new(from_column),
            to_table: NameKey::new(to_table),
            to_column: NameKey::new(to_column),
        }
    };
    not_unused(
        &unused,
        &relationship_id(
            "Sales",
            "SalesOrderLineKey",
            "Sales Order",
            "SalesOrderLineKey",
        ),
    );

    not_unused(
        &unused,
        &ObjectId::Role {
            role: NameKey::new("Administrators"),
        },
    );
}

/// The activation rule for inactive relationships: an unactivated one is
/// itself a finding, and its key columns surface as findings pointing back
/// at it — the `only used by X (also unused)` chain shape. Only a live
/// `USERELATIONSHIP` reference can activate one, and the golden report has
/// none. `Sales`[SalesOrderLineKey] is an inactive relationship's key too,
/// but the active relationship keeps it alive.
#[test]
fn golden_inactive_relationship_keys_are_findings() {
    let (db, report) = golden_pair();
    let graph = DependencyGraph::build(&db, &[&report]);
    let unused = graph.unused_objects();

    // The active relationship keeps its key columns alive; one of them is
    // also an inactive relationship's key, which changes nothing.
    not_unused(&unused, &column_id("Sales", "SalesOrderLineKey"));
    not_unused(&unused, &column_id("Sales Order", "SalesOrderLineKey"));

    let expected: [(ObjectId, &[ObjectId]); 2] = [
        (
            ObjectId::Relationship {
                from_table: NameKey::new("Sales"),
                from_column: NameKey::new("DueDateKey"),
                to_table: NameKey::new("Sales Order"),
                to_column: NameKey::new("DueDateKey"),
            },
            &[
                column_id("Sales", "DueDateKey"),
                column_id("Sales Order", "DueDateKey"),
            ],
        ),
        (
            ObjectId::Relationship {
                from_table: NameKey::new("Sales Order"),
                from_column: NameKey::new("SalesOrder"),
                to_table: NameKey::new("Sales"),
                to_column: NameKey::new("SalesOrderLineKey"),
            },
            &[column_id("Sales Order", "SalesOrder")],
        ),
    ];
    for (relationship_id, keys) in expected {
        // The unactivated relationship is a finding; its tables are the
        // recorded consumers that could not keep it alive.
        let relationship = find(&unused, &relationship_id);
        assert!(
            relationship.used_by.iter().all(|used| matches!(
                &used.provenance,
                Provenance::Structural {
                    role: StructuralEdge::InactiveRelationship
                }
            )),
            "{relationship_id}: only its tables record it"
        );
        for key in keys {
            let finding = find(&unused, key);
            assert_eq!(
                finding.used_by.len(),
                1,
                "{key}: the inactive relationship is the only reference"
            );
            assert_eq!(finding.used_by[0].id, relationship_id);
            assert!(
                finding.used_by[0].also_unused,
                "{key}: the relationship naming it is itself unused"
            );
            assert!(matches!(
                &finding.used_by[0].provenance,
                Provenance::Structural {
                    role: StructuralEdge::InactiveRelationshipEndpoint
                }
            ));
        }
    }
}

/// The dead chains carry their annotations: the hierarchy explains its level
/// columns, the calculation group explains its members, and the report measure
/// is an orphan.
#[test]
fn golden_dead_chains_are_annotated() {
    let (db, report) = golden_pair();
    let graph = DependencyGraph::build(&db, &[&report]);
    let unused = graph.unused_objects();

    // `YearNum` is referenced only by the dead hierarchy.
    let year_num = find(&unused, &column_id("Sales", "YearNum"));
    assert_eq!(year_num.used_by.len(), 1);
    assert_eq!(year_num.used_by[0].id, hierarchy_id("Sales", "Fiscal"));
    assert!(matches!(
        year_num.used_by[0].provenance,
        Provenance::Structural {
            role: StructuralEdge::HierarchyLevel
        }
    ));
    assert!(year_num.used_by[0].also_unused);

    // The calculation group table is referenced only by its two dead items and
    // its engine-managed field column.
    let group = find(&unused, &table_id("Time Intelligence"));
    assert_eq!(group.used_by.len(), 3);
    assert!(group.used_by.iter().all(|used| used.also_unused));
    assert!(
        group
            .used_by
            .iter()
            .any(|used| used.id == calculation_item_id("Time Intelligence", "Current"))
    );
    assert!(group.used_by.iter().all(|used| matches!(
        used.provenance,
        Provenance::Structural {
            role: StructuralEdge::TableMember
        }
    )));

    // The group's field column is kept only by the table itself, through the
    // engine-managed edge.
    let group_column = find(
        &unused,
        &column_id("Time Intelligence", "Time Intelligence"),
    );
    assert_eq!(group_column.used_by.len(), 1);
    assert_eq!(group_column.used_by[0].id, table_id("Time Intelligence"));
    assert!(matches!(
        group_column.used_by[0].provenance,
        Provenance::Structural {
            role: StructuralEdge::EngineManaged
        }
    ));

    // Orphans: nothing references them at all.
    for orphan in [
        measure_id("Sales", "Growth %"),
        report_measure_id("Budget %"),
        expression_id("ServerName"),
        column_id("Sales", "Margin %"),
    ] {
        assert_eq!(
            find(&unused, &orphan).used_by,
            Vec::new(),
            "{orphan} is an orphan"
        );
    }
}

/// The visual card's projection is a root with full binding provenance, and
/// the drillthrough/slicer/conditional-formatting sites all surface as roots.
#[test]
fn golden_roots_carry_binding_provenance() {
    let (db, report) = golden_pair();
    let graph = DependencyGraph::build(&db, &[&report]);

    // The card binds the `Sales` measure through its Values well…
    let sales_roots = graph.roots_of(&measure_id("Sales", "Sales"));
    assert_eq!(sales_roots.len(), 1);
    let Provenance::Binding(edge) = sales_roots[0] else {
        panic!("a root carries binding provenance");
    };
    assert!(matches!(
        &edge.kind,
        ripbi_core::BindingSite::FieldWell { role } if role.as_str() == "Values"
    ));
    assert_eq!(edge.page.as_ref().map(NameKey::as_str), Some("P1"));
    assert_eq!(edge.visual.as_ref().map(NameKey::as_str), Some("V2"));
    assert!(edge.bookmark.is_none());

    // …and the stale `Units` aggregation keeps the qualifying table alive —
    // the nearest resolvable candidate of a reference matching nothing. The
    // alt text's `Units` column reference is a second root on the table.
    assert_eq!(graph.roots_of(&table_id("Sales")).len(), 2);

    // The report's measures are nodes, not roots.
    assert!(graph.roots_of(&report_measure_id("Budget %")).is_empty());
}
