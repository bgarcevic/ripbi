//! Fixture tests for the M-reference rule (issue #39) against the obscured
//! production-shape model.
//!
//! `fixtures/tmdl/obscured-model/` is a fully synthetic model whose shapes
//! mirror a real production corpus — an inline SQL option record, Changed
//! Type and ReplaceValue column lists, `each [Field]` row filters, a
//! cross-query merge, and non-ASCII column names — scrubbed of every real
//! name. It pins the end-to-end behavior: a column named only inside Power
//! Query is still an unused finding (unloading it cannot break refresh) and
//! carries the naming partition as supply-chain context, merge-source tables
//! stay alive with Power Query provenance, a dead table's partition keeps
//! nothing, and data strings (SQL text, filter values) never count as
//! references.

use std::path::PathBuf;

use ripbi_core::graph::{DependencyGraph, Provenance};
use ripbi_core::ingest::semantic_model;
use ripbi_core::model::TabularDatabase;
use ripbi_core::model::index::ModelIndex;
use ripbi_core::report::{FieldTarget, FieldWell, Page, Projection, ReportModel, Visual};
use ripbi_core::{Column, NameKey, ObjectId, Table};

fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tmdl/obscured-model/Obscured.SemanticModel")
}

fn obscured_model() -> TabularDatabase {
    semantic_model(&model_path())
        .expect("the obscured fixture parses")
        .value
}

/// A visual on one page projecting one column.
fn report_binding(table: &str, column: &str) -> ReportModel {
    ReportModel {
        name: Some("Obscured".to_string()),
        dataset: Default::default(),
        filters: Vec::new(),
        pages: vec![Page {
            name: NameKey::new("P1"),
            display_name: None,
            is_hidden: false,
            filters: Vec::new(),
            binding: None,
            visuals: vec![Visual {
                name: NameKey::new("V1"),
                visual_type: "card".to_string(),
                wells: vec![FieldWell {
                    role: "Values".to_string(),
                    projections: vec![Projection {
                        target: FieldTarget::Column {
                            table: NameKey::new(table),
                            column: NameKey::new(column),
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
        bookmarks: Vec::new(),
        measures: Vec::new(),
    }
}

fn column_id(table: &str, column: &str) -> ObjectId {
    ObjectId::Column {
        table: NameKey::new(table),
        column: NameKey::new(column),
    }
}

fn table_id(table: &str) -> ObjectId {
    ObjectId::Table {
        table: NameKey::new(table),
    }
}

fn partition_id(table: &str, partition: &str) -> ObjectId {
    ObjectId::Partition {
        table: NameKey::new(table),
        partition: NameKey::new(partition),
    }
}

fn expression_id(name: &str) -> ObjectId {
    ObjectId::Expression {
        name: NameKey::new(name),
    }
}

fn not_unused(unused: &[ObjectId], id: &ObjectId) {
    assert!(!unused.contains(id), "{id} must be live");
}

/// The fixture parses with every object present, including the non-ASCII
/// column name and the shared expressions.
#[test]
fn the_obscured_model_ingests() {
    let db = obscured_model();
    assert_eq!(db.name.as_deref(), Some("Obscured"));
    let fact = db.tables.iter().find(|t| t.name == "Fact Events").unwrap();
    let names: Vec<&str> = fact
        .columns
        .iter()
        .map(|c: &Column| c.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["Event Id", "Amount", "Beløb", "Region", "Key", "Notes"]
    );
    assert_eq!(db.expressions.len(), 2);
}

/// The core issue #39 semantics, after the inversion. The pipeline is M →
/// tables/columns → DAX → reports: deletion never breaks upstream, so a
/// column named only inside Power Query is still an unused finding, and the
/// naming partition rides on the finding as supply-chain context (removing
/// the column from the *script* too means editing those steps). Deleting a
/// table is different — it deletes the query another partition's M reads —
/// so a merge source stays alive. Data strings keep nothing, and a dead
/// table's partition keeps nothing.
#[test]
fn m_named_columns_stay_findings_and_merge_sources_stay_alive() {
    let db = obscured_model();
    let report = report_binding("Fact Events", "Event Id");
    let graph = DependencyGraph::build(&db, &[&report]);
    let unused = graph.unused_objects();
    let ids: Vec<ObjectId> = unused.iter().map(|f| f.id.clone()).collect();
    let finding_for = |table: &str, column: &str| -> &ripbi_core::graph::UnusedObject {
        unused
            .iter()
            .find(|f| f.id == column_id(table, column))
            .unwrap_or_else(|| panic!("{table}[{column}] must be a finding"))
    };

    let expected_named = [partition_id("Fact Events", "Fact Events")];

    // Named by the Changed Type / ReplaceValue lists, the `each [Region]`
    // filter, and the join strings — named, not kept: every one is still a
    // finding, with exactly the partition as its supply-chain context.
    for name in ["Amount", "Beløb", "Region", "Key"] {
        let finding = finding_for("Fact Events", name);
        assert!(
            finding.used_by.is_empty(),
            "{name}: M names are not consumers"
        );
        assert_eq!(finding.named_by_m, expected_named, "{name}");
    }
    // Dim Lookup's columns: named by the join/expand strings from the fact
    // partition, never kept by them.
    for name in ["Lookup Key", "Dim Name"] {
        assert_eq!(
            finding_for("Dim Lookup", name).named_by_m,
            expected_named,
            "{name}"
        );
    }

    // The merge-source table itself IS kept: deleting it deletes the query
    // the NestedJoin reads, which breaks refresh.
    not_unused(&ids, &table_id("Dim Lookup"));

    // The one column no M step, DAX, or report references is dead with no
    // context: the SQL text and the "West" filter value are data.
    let notes = finding_for("Fact Events", "Notes");
    assert!(notes.used_by.is_empty());
    assert!(notes.named_by_m.is_empty(), "Notes must have no M context");

    // The dead table's partition keeps nothing — not its own column (whose
    // naming rides only as context), not the shared expression its M reads.
    assert!(ids.contains(&table_id("Staging Events")));
    assert!(ids.contains(&partition_id("Staging Events", "Staging Events")));
    let raw = finding_for("Staging Events", "Raw");
    assert_eq!(
        raw.named_by_m,
        [partition_id("Staging Events", "Staging Events")]
    );
    assert!(ids.contains(&expression_id("Staging Extract")));
    // `"Server Name"` appears only inside string literals — data, never a
    // reference — so the parameter stays unused too.
    assert!(ids.contains(&expression_id("Server Name")));

    // The table keep that survives carries Power Query provenance.
    assert_eq!(Provenance::M.to_string(), "Power Query expression");
}

/// The merge-source keep works one level up: `#"Dim Lookup"` in the fact
/// partition produces a table edge, so the merge-source table would survive
/// a scan even with no report binding and no relationship touching it.
#[test]
fn the_merge_source_table_has_an_m_table_edge() {
    let db = obscured_model();
    let report = report_binding("Fact Events", "Event Id");
    let graph = DependencyGraph::build(&db, &[&report]);

    let consumers = graph.consumers_of(&table_id("Dim Lookup"));
    assert!(
        consumers.iter().any(|(source, provenance)| *source
            == partition_id("Fact Events", "Fact Events")
            && matches!(provenance, Provenance::M)),
        "the fact partition's NestedJoin must keep the merge source alive: {consumers:?}"
    );

    // The self-reference inside Dim Lookup's own partition is dropped.
    assert!(
        !consumers
            .iter()
            .any(|(source, _)| *source == partition_id("Dim Lookup", "Dim Lookup")),
        "a partition naming its own table must not create an edge"
    );
}

/// The binding resolution is reachable through the public surface alone:
/// every harvested column string binds model-wide, so a name matching
/// columns on several tables names all of them.
#[test]
fn an_m_column_string_names_same_named_columns_on_every_table() {
    let db = TabularDatabase {
        tables: vec![
            Table {
                name: "Left".to_string(),
                columns: vec![Column {
                    name: "Join Key".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            Table {
                name: "Right".to_string(),
                columns: vec![Column {
                    name: "Join Key".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let index = ModelIndex::build(&db);
    let bindings = ripbi_core::m::bindings(
        &db,
        &index,
        r#"Table.NestedJoin(Source, {"Join Key"}, Other, {"Join Key"}, "O")"#,
    );
    assert_eq!(
        bindings[0].targets().len(),
        2,
        "both same-named columns are named"
    );
}

/// The graph layers the production rule on top of the conservative binding:
/// an M step can only *name* a column it produces, so an engine-computed
/// column matching an M name — auto date/time columns named like Desktop's
/// date-template query — gets no supply-chain context.
#[test]
fn an_engine_computed_column_matching_an_m_name_gets_no_supply_chain() {
    let db = TabularDatabase {
        tables: vec![
            Table {
                name: "Fact".to_string(),
                columns: vec![Column {
                    name: "Join Key".to_string(),
                    ..Default::default() // Data: produced by the M query
                }],
                partitions: vec![ripbi_core::Partition {
                    name: "Fact".to_string(),
                    source: ripbi_core::PartitionSource::M {
                        expression: r#"Table.SelectRows(Source, each [Join Key] <> null)"#
                            .to_string(),
                    },
                }],
                ..Default::default()
            },
            Table {
                name: "Date Template".to_string(),
                columns: vec![Column {
                    name: "Join Key".to_string(),
                    kind: ripbi_core::ColumnKind::Calculated {
                        expression: "YEAR([Date])".to_string(),
                    },
                    ..Default::default()
                }],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let graph = DependencyGraph::build(&db, &[]);

    let data_column = column_id("Fact", "Join Key");
    let calculated = column_id("Date Template", "Join Key");
    assert_eq!(
        graph.named_by_m(&data_column),
        &[partition_id("Fact", "Fact")],
        "the M-produced column carries its supply chain"
    );
    assert!(
        graph.named_by_m(&calculated).is_empty(),
        "an engine-computed column matching the M name is coincidence, not supply chain"
    );
}
