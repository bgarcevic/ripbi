//! The dependency graph: one [`petgraph`] DAG over the semantic model and the
//! reports that share it, plus the reachability analysis that isolates dead
//! objects.
//!
//! # Shape
//!
//! Nodes are [`ObjectId`]s — every table, column, measure, partition,
//! hierarchy, relationship, role, calculation item, shared expression,
//! user-defined function, and report measure, whether or not anything
//! references them. Edges point from user to used and carry their
//! [`Provenance`] as first-class data, so a reverse query
//! ([`consumers_of`](DependencyGraph::consumers_of)) is a pure read and a
//! second view over the graph (`ripbi deps`) is pure rendering in the CLI.
//! Report sites — visuals, pages, bookmarks — are not model objects, so their
//! bindings live beside the graph as [`roots`](DependencyGraph::roots) with
//! full provenance.
//!
//! # Liveness policy (conservative — what "unused" means)
//!
//! Reachability starts from the roots: every
//! [`ReportModel::bindings`](crate::ReportModel::bindings) target and every
//! role (roles are security configuration, never dead weight; their filter
//! edges keep the referenced columns alive). From there, two passes over the
//! edge catalog decide liveness — the full catalog with the reasoning behind
//! every rule lives in `docs/graph.md` beside this module:
//!
//! - **DAX references** (`dax::bind`, every candidate — an unqualified
//!   `[Name]` keeps the measure *and* the home-table column alive) and their
//!   extended candidates (hierarchies, calculation items, and, for a
//!   reference matching nothing, its qualifying table). A reference that
//!   matches nothing and has no resolvable part keeps nothing alive.
//! - **M references** (`m::bind`, same conservatism). The pipeline is M →
//!   tables/columns → DAX → reports, so M is upstream of everything and
//!   deletion flows one way. A **table or shared expression** named in M — a
//!   merge source, a referenced parameter query — keeps alive: deleting it
//!   deletes the query another partition reads, which breaks refresh. A
//!   **column** named in M is the column's *supply chain*, not a consumer:
//!   the query keeps producing it and the model just stops mapping it, so
//!   unloading cannot break refresh and there is deliberately no edge.
//!   Instead the naming expressions ride along on the finding
//!   ([`UnusedObject::named_by_m`]) — the what-a-full-removal-must-edit
//!   context. Liveness still flows through the owner, so a dead table's
//!   partition keeps nothing alive.
//! - **Report bindings** with their provenance, report measures shadowing
//!   model measures of the same name. An unused report measure is dead like
//!   any other node — its body's references stay alive only through it.
//! - **Containment**: a used member (column, measure, hierarchy, calculation
//!   item) keeps its table alive; a used table keeps its partitions,
//!   relationships, and engine-managed columns (calculated-table columns,
//!   calculation-group columns, calendar columns) alive.
//! - **Relationships**: live if either endpoint table is reachable. An
//!   **active** relationship keeps both key columns alive — but a key column
//!   kept alive *only* as a relationship endpoint does **not** keep its table
//!   alive, so a table referenced by nothing but a relationship is still
//!   unused. An **inactive** relationship is live only when a live DAX
//!   reference (`USERELATIONSHIP`) activates it — switching one on at query
//!   time is DAX's job, and nothing else can. Unactivated, it is a finding
//!   itself, and its key columns are findings chained under it.
//!
//! The conservatism rule from name resolution governs everything: marking an
//! object used too many is harmless; marking one too few tells a user to
//! delete live code. A model scanned with no reports and no roles therefore
//! reports *everything* as unused — callers decide whether that is a finding
//! or a missing report.
//!
//! # Examples
//!
//! ```
//! use ripbi_core::{
//!     Column, FieldTarget, FieldWell, Measure, NameKey, Page, Projection,
//!     ReportModel, Table, TabularDatabase, Visual,
//! };
//! use ripbi_core::graph::DependencyGraph;
//!
//! let db = TabularDatabase {
//!     tables: vec![Table {
//!         name: "Sales".to_string(),
//!         columns: vec![
//!             Column { name: "Amount".to_string(), ..Default::default() },
//!             Column { name: "Legacy".to_string(), ..Default::default() },
//!         ],
//!         measures: vec![Measure {
//!             name: "Total".to_string(),
//!             expression: "SUM('Sales'[Amount])".to_string(),
//!             ..Default::default()
//!         }],
//!         ..Default::default()
//!     }],
//!     ..Default::default()
//! };
//! // One visual projecting the Total measure keeps it — and its column — alive.
//! let report = ReportModel {
//!     pages: vec![Page {
//!         name: NameKey::new("P1"),
//!         display_name: None,
//!         is_hidden: false,
//!         filters: Vec::new(),
//!         binding: None,
//!         visuals: vec![Visual {
//!             name: NameKey::new("V1"),
//!             visual_type: "card".to_string(),
//!             wells: vec![FieldWell {
//!                 role: "Values".to_string(),
//!                 projections: vec![Projection {
//!                     target: FieldTarget::Measure {
//!                         home_table: Some(NameKey::new("Sales")),
//!                         measure: NameKey::new("Total"),
//!                     },
//!                     query_ref: None,
//!                     active: true,
//!                 }],
//!             }],
//!             filters: Vec::new(),
//!             sorts: Vec::new(),
//!             conditional_formatting: Vec::new(),
//!             alt_text: Vec::new(),
//!             tooltip_page: None,
//!         }],
//!     }],
//!     ..Default::default()
//! };
//!
//! let graph = DependencyGraph::build(&db, &[&report]);
//!
//! // The visual well is a root with provenance…
//! assert_eq!(graph.roots().len(), 1);
//! let total = ripbi_core::ObjectId::Measure {
//!     table: NameKey::new("Sales"),
//!     measure: NameKey::new("Total"),
//! };
//! assert_eq!(graph.roots_of(&total).len(), 1);
//! // …and nothing touches `Legacy`, so it is the one unused object.
//! let unused = graph.unused_objects();
//! assert_eq!(unused.len(), 1);
//! assert_eq!(unused[0].id.to_string(), "'Sales'[Legacy]");
//! assert!(unused[0].used_by.is_empty(), "nothing references it at all");
//! ```

use std::collections::{HashMap, HashSet};

use petgraph::Direction;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;

pub mod provenance;

mod builder;
mod reachability;

pub use provenance::{BindingEdge, BindingSite, Provenance, StructuralEdge};
pub use reachability::{UnusedObject, UsedBy};

use crate::identity::ObjectId;
use crate::model::TabularDatabase;
use crate::report::ReportModel;

/// The dependency graph of one semantic model and the reports sharing it.
///
/// Build it once with [`DependencyGraph::build`], then query: who uses an
/// object ([`consumers_of`](DependencyGraph::consumers_of)), what an object
/// uses ([`producers_of`](DependencyGraph::producers_of)), and what nothing
/// reaches ([`unused_objects`](DependencyGraph::unused_objects)).
#[derive(Debug)]
pub struct DependencyGraph {
    /// The object-to-object edges, user → used, weighted by provenance.
    graph: DiGraph<ObjectId, Provenance>,
    /// Node key → petgraph index. Every model and report object has a node.
    nodes: HashMap<ObjectId, NodeIndex>,
    /// The reachability roots: report bindings pointing at model objects, with
    /// their binding provenance, in report order.
    roots: Vec<(ObjectId, Provenance)>,
    /// Columns named by M expressions: the supply chain that is deliberately
    /// *not* edges. Key: the column. Value: the naming expressions, sorted.
    m_named: HashMap<ObjectId, Vec<ObjectId>>,
}

impl DependencyGraph {
    /// Builds the graph for one model and every report that shares it.
    ///
    /// Never fails: resolution misses are data, never errors. Passing no
    /// reports leaves every model object unused unless a role keeps it alive.
    #[must_use]
    pub fn build(db: &TabularDatabase, reports: &[&ReportModel]) -> Self {
        builder::build(db, reports)
    }

    /// Assembles a finished graph from its parts. Only the builder calls this.
    pub(super) fn assemble(
        graph: DiGraph<ObjectId, Provenance>,
        nodes: HashMap<ObjectId, NodeIndex>,
        roots: Vec<(ObjectId, Provenance)>,
        m_named: HashMap<ObjectId, Vec<ObjectId>>,
    ) -> Self {
        Self {
            graph,
            nodes,
            roots,
            m_named,
        }
    }

    /// Every object in the graph, in build order (model order, then
    /// relationships, roles, shared expressions, functions, report measures).
    pub fn object_ids(&self) -> impl Iterator<Item = &ObjectId> {
        self.graph.node_indices().map(|index| &self.graph[index])
    }

    /// The objects that use `id`, with what kind of use each edge records —
    /// the query the `ripbi deps` view is built on. Report bindings are not
    /// object-to-object edges; they are answered by
    /// [`roots_of`](DependencyGraph::roots_of).
    pub fn consumers_of(&self, id: &ObjectId) -> Vec<(ObjectId, Provenance)> {
        self.neighbors(id, Direction::Incoming)
    }

    /// The objects that `id` uses, with what kind of use each edge records.
    pub fn producers_of(&self, id: &ObjectId) -> Vec<(ObjectId, Provenance)> {
        self.neighbors(id, Direction::Outgoing)
    }

    /// Every reachability root: the report bindings, with their targets and
    /// provenance, in report order. Deterministic for a given set of reports.
    pub fn roots(&self) -> &[(ObjectId, Provenance)] {
        &self.roots
    }

    /// The provenance of every report binding that targets `id`.
    pub fn roots_of(&self, id: &ObjectId) -> Vec<&Provenance> {
        self.roots
            .iter()
            .filter(|(target, _)| target == id)
            .map(|(_, provenance)| provenance)
            .collect()
    }

    /// The M expressions that name `id` — its Power Query supply chain. A
    /// name is not a consumer: unloading a column these expressions produce
    /// cannot break refresh. But removing the column *entirely* — model and
    /// script — means editing each of them, which is what this answers.
    /// Non-empty only ever for columns.
    pub fn named_by_m(&self, id: &ObjectId) -> &[ObjectId] {
        self.m_named.get(id).map(Vec::as_slice).unwrap_or_default()
    }

    /// Every object reachability never reached, sorted by object identity:
    /// the `scan` findings. Each finding names who still references it —
    /// empty for a true orphan, and every referencing object is either
    /// itself unused, a key column kept alive only as an active relationship
    /// endpoint, or the table of an inactive relationship it cannot keep
    /// alive.
    pub fn unused_objects(&self) -> Vec<UnusedObject> {
        let reach = reachability::Reachability::compute(self);
        let mut out: Vec<UnusedObject> = self
            .graph
            .node_indices()
            .filter(|index| !reach.is_live(&self.graph[*index]))
            .map(|index| {
                let id = self.graph[index].clone();
                let mut used_by: Vec<UsedBy> = self
                    .graph
                    .edges_directed(index, Direction::Incoming)
                    .map(|edge| UsedBy {
                        id: self.graph[edge.source()].clone(),
                        provenance: edge.weight().clone(),
                        also_unused: !reach.is_live(&self.graph[edge.source()]),
                    })
                    .collect();
                used_by.sort_by(|a, b| a.id.cmp(&b.id));
                let named_by_m = self.named_by_m(&id).to_vec();
                UnusedObject {
                    id,
                    used_by,
                    named_by_m,
                }
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn neighbors(&self, id: &ObjectId, direction: Direction) -> Vec<(ObjectId, Provenance)> {
        let Some(&index) = self.nodes.get(id) else {
            return Vec::new();
        };
        self.graph
            .edges_directed(index, direction)
            .map(|edge| {
                let other = match direction {
                    Direction::Incoming => edge.source(),
                    Direction::Outgoing => edge.target(),
                };
                (self.graph[other].clone(), edge.weight().clone())
            })
            .collect()
    }

    /// The petgraph indices reachability starts from: every root target and
    /// every role.
    pub(super) fn seed_indices(&self) -> Vec<NodeIndex> {
        let mut seeds: Vec<NodeIndex> = self
            .roots
            .iter()
            .filter_map(|(id, _)| self.nodes.get(id).copied())
            .collect();
        seeds.extend(
            self.nodes
                .iter()
                .filter(|(id, _)| matches!(id, ObjectId::Role { .. }))
                .map(|(_, &index)| index),
        );
        seeds
    }

    /// The set of nodes reachable from `seeds` over the edges `allowed`.
    pub(super) fn reach(
        &self,
        seeds: impl IntoIterator<Item = NodeIndex>,
        allowed: fn(&Provenance) -> bool,
    ) -> HashSet<NodeIndex> {
        let mut seen: HashSet<NodeIndex> = seeds.into_iter().collect();
        let mut queue: Vec<NodeIndex> = seen.iter().copied().collect();
        while let Some(index) = queue.pop() {
            for edge in self.graph.edges_directed(index, Direction::Outgoing) {
                if !allowed(edge.weight()) {
                    continue;
                }
                if seen.insert(edge.target()) {
                    queue.push(edge.target());
                }
            }
        }
        seen
    }

    /// The node key at a petgraph index.
    pub(super) fn object_at(&self, index: NodeIndex) -> &ObjectId {
        &self.graph[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NameKey;
    use crate::model::{
        Column, ColumnKind, DaxExpressionKind, Function, Measure, Partition, PartitionSource,
        Relationship, Role, SharedExpression, Table, TablePermission,
    };
    use crate::report::{
        Bookmark, BookmarkSection, BookmarkVisual, FieldTarget, FieldWell, Filter, Page,
        Projection, Visual,
    };

    fn column(name: &str) -> Column {
        Column {
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn measure(name: &str, expression: &str) -> Measure {
        Measure {
            name: name.to_string(),
            expression: expression.to_string(),
            ..Default::default()
        }
    }

    fn m_partition(name: &str, expression: &str) -> Partition {
        Partition {
            name: name.to_string(),
            source: PartitionSource::M {
                expression: expression.to_string(),
            },
        }
    }

    fn table(name: &str) -> Table {
        Table {
            name: name.to_string(),
            ..Default::default()
        }
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

    fn report_measure_id(name: &str) -> ObjectId {
        ObjectId::ReportMeasure {
            measure: NameKey::new(name),
        }
    }

    /// A visual on page `page` projecting `targets` into its Values well.
    fn visual_page(page: &str, visual: &str, targets: &[FieldTarget]) -> ReportModel {
        ReportModel {
            name: Some("Mini".to_string()),
            pages: vec![Page {
                name: NameKey::new(page),
                display_name: None,
                is_hidden: false,
                filters: Vec::new(),
                binding: None,
                visuals: vec![Visual {
                    name: NameKey::new(visual),
                    visual_type: "card".to_string(),
                    wells: vec![FieldWell {
                        role: "Values".to_string(),
                        projections: targets
                            .iter()
                            .map(|target| Projection {
                                target: target.clone(),
                                query_ref: None,
                                active: true,
                            })
                            .collect(),
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

    fn measure_target(table: &str, name: &str) -> FieldTarget {
        FieldTarget::Measure {
            home_table: Some(NameKey::new(table)),
            measure: NameKey::new(name),
        }
    }

    fn column_target(table: &str, column: &str) -> FieldTarget {
        FieldTarget::Column {
            table: NameKey::new(table),
            column: NameKey::new(column),
        }
    }

    /// The finding for `id`, panicking with a readable message when absent.
    fn find<'a>(unused: &'a [UnusedObject], id: &ObjectId) -> &'a UnusedObject {
        unused
            .iter()
            .find(|finding| &finding.id == id)
            .unwrap_or_else(|| panic!("{id} expected in the unused set"))
    }

    fn not_unused(unused: &[UnusedObject], id: &ObjectId) {
        assert!(
            !unused.iter().any(|finding| &finding.id == id),
            "{id} must be live"
        );
    }

    mod construction {
        use super::*;

        #[test]
        fn every_model_object_gets_a_node_even_when_isolated() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    columns: vec![column("Amount")],
                    ..Default::default()
                }],
                functions: vec![Function {
                    name: "MyFunc".to_string(),
                    expression: "1".to_string(),
                    is_hidden: false,
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);

            let ids: Vec<_> = graph.object_ids().cloned().collect();
            assert!(ids.contains(&table_id("Sales")));
            assert!(ids.contains(&column_id("Sales", "Amount")));
            assert!(ids.contains(&ObjectId::Function {
                name: NameKey::new("MyFunc")
            }));
        }

        #[test]
        fn identical_edges_are_deduped_but_distinct_provenance_is_kept() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    columns: vec![column("Amount")],
                    measures: vec![measure(
                        "Total",
                        "SUM('Sales'[Amount]) + SUM('Sales'[Amount])",
                    )],
                    ..Default::default()
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);

            // The measure's outgoing edges: containment in its table, plus
            // exactly ONE DAX edge to the column even though the reference is
            // written twice.
            let producers = graph.producers_of(&measure_id("Sales", "Total"));
            assert_eq!(producers.len(), 2);
            assert_eq!(
                producers
                    .iter()
                    .filter(|(id, _)| *id == column_id("Sales", "Amount"))
                    .count(),
                1,
                "identical (from, to, provenance) triples dedupe"
            );
            // …while the column's only consumer is the measure's DAX edge; its
            // containment edge points the other way, at the table.
            let consumers = graph.consumers_of(&column_id("Sales", "Amount"));
            assert_eq!(consumers.len(), 1);
            assert!(matches!(
                consumers[0].1,
                Provenance::Dax {
                    kind: DaxExpressionKind::Measure
                }
            ));
            assert_eq!(consumers[0].0, measure_id("Sales", "Total"));
            assert!(
                graph
                    .consumers_of(&table_id("Sales"))
                    .iter()
                    .any(|(id, p)| *id == column_id("Sales", "Amount")
                        && matches!(
                            p,
                            Provenance::Structural {
                                role: StructuralEdge::TableMember
                            }
                        ))
            );
        }

        /// A shared expression whose M text names itself keeps nothing alive:
        /// self-references are dropped rather than recorded.
        #[test]
        fn self_references_are_dropped() {
            let db = TabularDatabase {
                expressions: vec![SharedExpression {
                    name: "Recursive".to_string(),
                    expression: "Recursive + 1".to_string(),
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);
            let id = ObjectId::Expression {
                name: NameKey::new("Recursive"),
            };

            assert!(graph.producers_of(&id).is_empty());
            assert!(graph.consumers_of(&id).is_empty());
        }
    }

    mod liveness {
        use super::*;

        /// The far-table policy: a live table keeps its relationship and both
        /// key columns alive, but the far table stays unused — its key column,
        /// alive only as a relationship endpoint, cannot keep it.
        #[test]
        fn a_relationship_does_not_keep_its_far_table_alive() {
            let db = TabularDatabase {
                tables: vec![
                    Table {
                        name: "Sales".to_string(),
                        columns: vec![column("Key")],
                        partitions: vec![m_partition("Sales", "let Source = 1 in Source")],
                        ..Default::default()
                    },
                    Table {
                        name: "DimOld".to_string(),
                        columns: vec![column("Key"), column("Notes")],
                        partitions: vec![m_partition("DimOld", "let Source = 2 in Source")],
                        ..Default::default()
                    },
                ],
                relationships: vec![Relationship {
                    name: None,
                    from_table: "Sales".to_string(),
                    from_column: "Key".to_string(),
                    to_table: "DimOld".to_string(),
                    to_column: "Key".to_string(),
                    is_active: true,
                }],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[column_target("Sales", "Key")]);
            let graph = DependencyGraph::build(&db, &[&report]);
            let unused = graph.unused_objects();

            // The used side is entirely live, weak parts included.
            not_unused(&unused, &table_id("Sales"));
            not_unused(&unused, &column_id("Sales", "Key"));
            not_unused(
                &unused,
                &ObjectId::Relationship {
                    from_table: NameKey::new("Sales"),
                    from_column: NameKey::new("Key"),
                    to_table: NameKey::new("DimOld"),
                    to_column: NameKey::new("Key"),
                },
            );

            // The far table is unused despite its live key column…
            let dim_old = find(&unused, &table_id("DimOld"));
            assert_eq!(dim_old.used_by.len(), 2, "its two columns contain it");
            let by_key = dim_old
                .used_by
                .iter()
                .find(|used| used.id == column_id("DimOld", "Key"))
                .expect("the key column references its table");
            assert!(
                !by_key.also_unused,
                "the key column is live, kept by the relationship endpoint"
            );
            assert!(matches!(
                by_key.provenance,
                Provenance::Structural {
                    role: StructuralEdge::TableMember
                }
            ));

            // …and so are its other column and its partition, annotated.
            let notes = find(&unused, &column_id("DimOld", "Notes"));
            assert!(notes.used_by.is_empty(), "an orphan has no consumers");
            let partition = find(
                &unused,
                &ObjectId::Partition {
                    table: NameKey::new("DimOld"),
                    partition: NameKey::new("DimOld"),
                },
            );
            assert_eq!(partition.used_by.len(), 1);
            assert!(partition.used_by[0].also_unused);
            assert_eq!(partition.used_by[0].id, table_id("DimOld"));
        }

        /// An inactive relationship nothing activates is itself a finding,
        /// and its key columns are findings pointing back at it — the
        /// `only used by … (also unused)` chain shape. Only a live
        /// `USERELATIONSHIP` reference can switch it on at query time.
        #[test]
        fn an_unactivated_inactive_relationship_is_a_finding_with_its_keys() {
            let relationship_id = ObjectId::Relationship {
                from_table: NameKey::new("Sales"),
                from_column: NameKey::new("Key"),
                to_table: NameKey::new("DimOld"),
                to_column: NameKey::new("Key"),
            };
            let db = TabularDatabase {
                tables: vec![
                    Table {
                        name: "Sales".to_string(),
                        columns: vec![column("Amt"), column("Key")],
                        measures: vec![measure("Total", "SUM('Sales'[Amt])")],
                        partitions: vec![m_partition("Sales", "let Source = 1 in Source")],
                        ..Default::default()
                    },
                    Table {
                        name: "DimOld".to_string(),
                        columns: vec![column("Key"), column("Notes")],
                        partitions: vec![m_partition("DimOld", "let Source = 2 in Source")],
                        ..Default::default()
                    },
                ],
                relationships: vec![Relationship {
                    name: None,
                    from_table: "Sales".to_string(),
                    from_column: "Key".to_string(),
                    to_table: "DimOld".to_string(),
                    to_column: "Key".to_string(),
                    is_active: false,
                }],
                ..Default::default()
            };
            // Only `Total` is bound: `Sales` is live, `DimOld` is not, and
            // the inactive relationship must not rescue its keys — or itself.
            let report = visual_page("P1", "V1", &[measure_target("Sales", "Total")]);
            let graph = DependencyGraph::build(&db, &[&report]);
            let unused = graph.unused_objects();

            not_unused(&unused, &table_id("Sales"));

            // The relationship is a finding; its two tables are the recorded
            // consumers that could not keep it alive — `Sales` live, `DimOld`
            // itself unused.
            let relationship = find(&unused, &relationship_id);
            assert_eq!(relationship.used_by.len(), 2);
            assert!(relationship.used_by.iter().all(|used| matches!(
                &used.provenance,
                Provenance::Structural {
                    role: StructuralEdge::InactiveRelationship
                }
            )));
            let sales_side = relationship
                .used_by
                .iter()
                .find(|used| used.id == table_id("Sales"))
                .expect("the from table references the relationship");
            assert!(!sales_side.also_unused);

            // Both keys point back at the unactivated relationship — the
            // `only used by … (also unused)` chain shape.
            for (table_name, column_name) in [("Sales", "Key"), ("DimOld", "Key")] {
                let finding = find(&unused, &column_id(table_name, column_name));
                assert_eq!(
                    finding.used_by.len(),
                    1,
                    "the inactive relationship is the only reference"
                );
                assert!(finding.used_by[0].also_unused);
                assert_eq!(finding.used_by[0].id, relationship_id);
                assert!(matches!(
                    &finding.used_by[0].provenance,
                    Provenance::Structural {
                        role: StructuralEdge::InactiveRelationshipEndpoint
                    }
                ));
            }
            // And `DimOld` is still a finding: a dead key column must not
            // pull its own table along.
            find(&unused, &table_id("DimOld"));
        }

        /// The other half of the rule: a live measure switching the inactive
        /// relationship on with `USERELATIONSHIP` is an ordinary DAX
        /// reference, and it keeps both key columns alive.
        #[test]
        fn a_live_userelationship_measure_keeps_inactive_keys_alive() {
            let db = TabularDatabase {
                tables: vec![
                    Table {
                        name: "Sales".to_string(),
                        columns: vec![column("Amt"), column("Key")],
                        measures: vec![measure(
                            "Old Total",
                            "CALCULATE(SUM('Sales'[Amt]), USERELATIONSHIP('Sales'[Key], 'DimOld'[Key]))",
                        )],
                        partitions: vec![m_partition("Sales", "let Source = 1 in Source")],
                        ..Default::default()
                    },
                    Table {
                        name: "DimOld".to_string(),
                        columns: vec![column("Key"), column("Notes")],
                        partitions: vec![m_partition("DimOld", "let Source = 2 in Source")],
                        ..Default::default()
                    },
                ],
                relationships: vec![Relationship {
                    name: None,
                    from_table: "Sales".to_string(),
                    from_column: "Key".to_string(),
                    to_table: "DimOld".to_string(),
                    to_column: "Key".to_string(),
                    is_active: false,
                }],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[measure_target("Sales", "Old Total")]);
            let graph = DependencyGraph::build(&db, &[&report]);
            let unused = graph.unused_objects();

            not_unused(&unused, &column_id("Sales", "Key"));
            not_unused(&unused, &column_id("DimOld", "Key"));
            // The live measure's call is the activation edge itself: the
            // relationship stays alive even though no table needs it.
            not_unused(
                &unused,
                &ObjectId::Relationship {
                    from_table: NameKey::new("Sales"),
                    from_column: NameKey::new("Key"),
                    to_table: NameKey::new("DimOld"),
                    to_column: NameKey::new("Key"),
                },
            );
            // The measure's `USERELATIONSHIP` arguments are ordinary DAX
            // references, so containment applies on top: `DimOld` stays alive
            // through its live key column, and only `Notes` is left dead.
            not_unused(&unused, &table_id("DimOld"));
            let notes = find(&unused, &column_id("DimOld", "Notes"));
            assert!(notes.used_by.is_empty());
        }

        /// An RLS filter is rooted at its role: the filtered column stays alive
        /// even though no report binding and no DAX references it.
        #[test]
        fn an_rls_filter_keeps_its_column_and_table_alive() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    columns: vec![column("Region")],
                    ..Default::default()
                }],
                roles: vec![Role {
                    name: "Reader".to_string(),
                    table_permissions: vec![TablePermission {
                        table: "Sales".to_string(),
                        filter_expression: Some("'Sales'[Region] = \"West\"".to_string()),
                    }],
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);
            let unused = graph.unused_objects();

            assert!(
                unused.is_empty(),
                "the role seeds the filter, the filter keeps the column, the column keeps the table"
            );
            let consumers = graph.consumers_of(&column_id("Sales", "Region"));
            assert_eq!(consumers.len(), 1);
            assert_eq!(
                consumers[0].0,
                ObjectId::Role {
                    role: NameKey::new("Reader")
                }
            );
            assert!(matches!(
                consumers[0].1,
                Provenance::Dax {
                    kind: DaxExpressionKind::RlsFilter
                }
            ));
        }

        /// A metadata-only role permission keeps the granted table alive.
        #[test]
        fn a_metadata_only_permission_keeps_its_table_alive() {
            let db = TabularDatabase {
                tables: vec![table("Sales")],
                roles: vec![Role {
                    name: "Reader".to_string(),
                    table_permissions: vec![TablePermission {
                        table: "Sales".to_string(),
                        filter_expression: None,
                    }],
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);

            assert!(graph.unused_objects().is_empty());
        }

        /// With no reports and no roles, nothing is reachable: everything is
        /// unused, which is the caller's signal that no roots were found.
        #[test]
        fn a_model_with_no_roots_reports_everything_unused() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    columns: vec![column("Amount")],
                    partitions: vec![m_partition("Sales", "let Source = 1 in Source")],
                    ..Default::default()
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);

            assert_eq!(graph.unused_objects().len(), 3);
            assert!(graph.roots().is_empty());
        }

        /// An unused report measure is dead, and what only it references
        /// carries the "also unused" annotation.
        #[test]
        fn an_unused_report_measure_is_dead_and_annotates_its_chain() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    columns: vec![column("Amount"), column("Old")],
                    measures: vec![measure("Total", "SUM('Sales'[Amount])")],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let mut report = visual_page("P1", "V1", &[measure_target("Sales", "Total")]);
            report.measures.push(crate::report::ReportMeasure {
                name: NameKey::new("Local"),
                expression: "SUM('Sales'[Old])".to_string(),
                format_string: None,
            });

            let graph = DependencyGraph::build(&db, &[&report]);
            let unused = graph.unused_objects();

            let local = find(&unused, &report_measure_id("Local"));
            assert!(local.used_by.is_empty(), "no visual binds it");
            let old = find(&unused, &column_id("Sales", "Old"));
            assert_eq!(old.used_by.len(), 1);
            assert_eq!(old.used_by[0].id, report_measure_id("Local"));
            assert!(old.used_by[0].also_unused);
            not_unused(&unused, &column_id("Sales", "Amount"));
        }

        /// A visual can bind a report measure directly; the report measure
        /// shadows a model measure of the same name, which then reads as
        /// unreferenced from this report.
        #[test]
        fn a_visual_binding_resolves_to_the_shadowing_report_measure() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    measures: vec![measure("Total", "0")],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let mut report = visual_page("P1", "V1", &[measure_target("Sales", "Total")]);
            report.measures.push(crate::report::ReportMeasure {
                name: NameKey::new("Total"),
                expression: "[Model Total]".to_string(),
                format_string: None,
            });

            let graph = DependencyGraph::build(&db, &[&report]);

            // The binding landed on the report measure, not the model measure.
            assert_eq!(graph.roots_of(&report_measure_id("Total")).len(), 1);
            assert!(graph.roots_of(&measure_id("Sales", "Total")).is_empty());
            let unused = graph.unused_objects();
            not_unused(&unused, &report_measure_id("Total"));
            let shadowed = find(&unused, &measure_id("Sales", "Total"));
            assert!(shadowed.used_by.is_empty());
        }

        /// Sort-by chains: an unused sorted column drags its unused sort
        /// column along, with the annotation naming the chain.
        #[test]
        fn a_sort_by_chain_is_annotated() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Date".to_string(),
                    columns: vec![
                        Column {
                            name: "Month Name".to_string(),
                            sort_by_column: Some("Month Num".to_string()),
                            ..Default::default()
                        },
                        column("Month Num"),
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);
            let unused = graph.unused_objects();

            let month_name = find(&unused, &column_id("Date", "Month Name"));
            assert!(month_name.used_by.is_empty());
            let month_num = find(&unused, &column_id("Date", "Month Num"));
            assert_eq!(month_num.used_by.len(), 1);
            assert_eq!(month_num.used_by[0].id, column_id("Date", "Month Name"));
            assert!(month_num.used_by[0].also_unused);
            assert!(matches!(
                month_num.used_by[0].provenance,
                Provenance::Structural {
                    role: StructuralEdge::SortByColumn
                }
            ));
        }

        /// Group-by chains mirror sort-by: an unused grouping column drags
        /// its unused group column along, with the annotation naming the chain.
        #[test]
        fn a_group_by_chain_is_annotated() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    columns: vec![
                        Column {
                            name: "Amount".to_string(),
                            group_by_columns: vec!["Bucket".to_string()],
                            ..Default::default()
                        },
                        column("Bucket"),
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);
            let unused = graph.unused_objects();

            let amount = find(&unused, &column_id("Sales", "Amount"));
            assert!(amount.used_by.is_empty());
            let bucket = find(&unused, &column_id("Sales", "Bucket"));
            assert_eq!(bucket.used_by.len(), 1);
            assert_eq!(bucket.used_by[0].id, column_id("Sales", "Amount"));
            assert!(bucket.used_by[0].also_unused);
            assert!(matches!(
                bucket.used_by[0].provenance,
                Provenance::Structural {
                    role: StructuralEdge::GroupByColumn
                }
            ));
        }

        /// A used column keeps its group-by column alive: grouping is part of
        /// how the engine aggregates the column, so a column referenced only
        /// through a group-by is not dead.
        #[test]
        fn a_used_column_keeps_its_group_by_column_alive() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    columns: vec![
                        Column {
                            name: "Amount".to_string(),
                            group_by_columns: vec!["Bucket".to_string()],
                            ..Default::default()
                        },
                        column("Bucket"),
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[column_target("Sales", "Amount")]);

            let graph = DependencyGraph::build(&db, &[&report]);

            assert!(graph.unused_objects().is_empty());
        }

        /// A dead hierarchy keeps its level columns from being orphans: they
        /// are referenced only by the hierarchy, which is itself unused.
        #[test]
        fn a_dead_hierarchy_annotates_its_level_columns() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Date".to_string(),
                    columns: vec![column("Year")],
                    hierarchies: vec![crate::model::Hierarchy {
                        name: "Calendar".to_string(),
                        levels: vec![crate::model::HierarchyLevel {
                            name: "Year".to_string(),
                            column: "Year".to_string(),
                        }],
                        is_hidden: false,
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);
            let unused = graph.unused_objects();

            let hierarchy = find(
                &unused,
                &ObjectId::Hierarchy {
                    table: NameKey::new("Date"),
                    hierarchy: NameKey::new("Calendar"),
                },
            );
            assert!(hierarchy.used_by.is_empty());
            let year = find(&unused, &column_id("Date", "Year"));
            assert_eq!(year.used_by.len(), 1);
            assert!(matches!(
                year.used_by[0].provenance,
                Provenance::Structural {
                    role: StructuralEdge::HierarchyLevel
                }
            ));
            assert!(year.used_by[0].also_unused);
        }

        /// A hierarchy referenced from DAX (`ISINSCOPE('Date'[Calendar])`) is
        /// an extended-resolution candidate the plain binder does not know.
        #[test]
        fn dax_keeps_a_referenced_hierarchy_alive() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Date".to_string(),
                    columns: vec![column("Year")],
                    hierarchies: vec![crate::model::Hierarchy {
                        name: "Calendar".to_string(),
                        levels: vec![crate::model::HierarchyLevel {
                            name: "Year".to_string(),
                            column: "Year".to_string(),
                        }],
                        is_hidden: false,
                    }],
                    measures: vec![measure("In Scope", "ISINSCOPE('Date'[Calendar])")],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[measure_target("Date", "In Scope")]);

            let graph = DependencyGraph::build(&db, &[&report]);

            assert!(graph.unused_objects().is_empty());
        }

        /// A report binding on a calculation-group column keeps every item of
        /// its group alive: a slicer or filter over the column can select any
        /// item by name at query time. Structural liveness of the group alone
        /// does not: the dead-chain fixture pins an unselected item staying
        /// dead when only another item's explicit DAX use keeps the table up.
        #[test]
        fn a_binding_on_a_calculation_group_column_keeps_its_items_alive() {
            let db = TabularDatabase {
                tables: vec![
                    Table {
                        name: "Sales".to_string(),
                        columns: vec![column("Amount")],
                        measures: vec![measure("Total", "SUM('Sales'[Amount])")],
                        ..Default::default()
                    },
                    Table {
                        name: "Date Role".to_string(),
                        columns: vec![column("Date Role")],
                        calculation_group: Some(crate::model::CalculationGroup {
                            items: vec![
                                crate::model::CalculationItem {
                                    name: "By Ship Date".to_string(),
                                    expression: "SELECTEDMEASURE()".to_string(),
                                    format_string_expression: None,
                                },
                                crate::model::CalculationItem {
                                    name: "By Due Date".to_string(),
                                    expression: "SELECTEDMEASURE()".to_string(),
                                    format_string_expression: None,
                                },
                            ],
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let report = visual_page(
                "P1",
                "Slicer",
                &[
                    measure_target("Sales", "Total"),
                    column_target("Date Role", "Date Role"),
                ],
            );

            let graph = DependencyGraph::build(&db, &[&report]);

            assert!(
                graph.unused_objects().is_empty(),
                "the bound column keeps the group, the group's items, and the model alive"
            );
            let consumers = graph.consumers_of(&ObjectId::CalculationItem {
                table: NameKey::new("Date Role"),
                item: NameKey::new("By Ship Date"),
            });
            assert!(
                consumers.iter().any(|(id, provenance)| {
                    *id == column_id("Date Role", "Date Role")
                        && matches!(provenance, Provenance::Binding(_))
                }),
                "the column's binding edge names the item, with the binding site as provenance"
            );
        }

        /// A qualified reference into a calculation group keeps the named
        /// calculation item alive.
        #[test]
        fn dax_keeps_a_referenced_calculation_item_alive() {
            let db = TabularDatabase {
                tables: vec![
                    Table {
                        name: "Sales".to_string(),
                        measures: vec![measure(
                            "YTD Sales",
                            "CALCULATE(SUM('Sales'[Amount]), 'Time Intelligence'[YTD])",
                        )],
                        ..Default::default()
                    },
                    Table {
                        name: "Time Intelligence".to_string(),
                        calculation_group: Some(crate::model::CalculationGroup {
                            items: vec![
                                crate::model::CalculationItem {
                                    name: "YTD".to_string(),
                                    expression: "SELECTEDMEASURE()".to_string(),
                                    format_string_expression: None,
                                },
                                crate::model::CalculationItem {
                                    name: "MTD".to_string(),
                                    expression: "SELECTEDMEASURE()".to_string(),
                                    format_string_expression: None,
                                },
                            ],
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[measure_target("Sales", "YTD Sales")]);

            let graph = DependencyGraph::build(&db, &[&report]);
            let unused = graph.unused_objects();
            let unused_ids: Vec<&ObjectId> = unused.iter().map(|finding| &finding.id).collect();

            assert_eq!(
                unused_ids,
                [&ObjectId::CalculationItem {
                    table: NameKey::new("Time Intelligence"),
                    item: NameKey::new("MTD"),
                }],
                "only the unselected calculation item is unused"
            );
        }

        /// A qualified reference matching nothing keeps its qualifying table
        /// alive — the nearest resolvable candidate.
        #[test]
        fn an_unresolved_qualified_reference_keeps_its_table_alive() {
            let db = TabularDatabase {
                tables: vec![
                    Table {
                        name: "Sales".to_string(),
                        measures: vec![measure("M", "'Ghost'[Nope]")],
                        ..Default::default()
                    },
                    table("Ghost"),
                ],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[measure_target("Sales", "M")]);

            let graph = DependencyGraph::build(&db, &[&report]);

            assert!(graph.unused_objects().is_empty(), "Ghost stays alive");
        }

        /// A reference whose table does not exist either keeps nothing alive.
        #[test]
        fn an_unresolved_reference_without_a_resolvable_part_keeps_nothing_alive() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    measures: vec![measure("M", "'Ghost'[Nope] + [Also Nope]")],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[measure_target("Sales", "M")]);

            let graph = DependencyGraph::build(&db, &[&report]);

            assert_eq!(graph.unused_objects().len(), 0, "only Sales and M exist");
        }

        /// A shared expression named in an M partition is referenced by it —
        /// and if the partition's table is dead, the annotation says so.
        #[test]
        fn m_references_keep_shared_expressions_alive() {
            let db = TabularDatabase {
                tables: vec![
                    Table {
                        name: "Sales".to_string(),
                        partitions: vec![m_partition(
                            "Sales",
                            "let Source = Sql.Database(ServerName) in Source",
                        )],
                        ..Default::default()
                    },
                    Table {
                        name: "DimOld".to_string(),
                        partitions: vec![m_partition(
                            "DimOld",
                            "let Source = LegacyParam in Source",
                        )],
                        ..Default::default()
                    },
                ],
                expressions: vec![
                    SharedExpression {
                        name: "ServerName".to_string(),
                        expression: "\"localhost\"".to_string(),
                    },
                    SharedExpression {
                        name: "LegacyParam".to_string(),
                        expression: "5".to_string(),
                    },
                ],
                ..Default::default()
            };
            // The visual binds a column that does not exist; the written form
            // still keeps its qualifying table alive.
            let report = visual_page("P1", "V1", &[column_target("Sales", "Anything")]);

            let graph = DependencyGraph::build(&db, &[&report]);
            let unused = graph.unused_objects();

            not_unused(
                &unused,
                &ObjectId::Expression {
                    name: NameKey::new("ServerName"),
                },
            );
            let legacy = find(
                &unused,
                &ObjectId::Expression {
                    name: NameKey::new("LegacyParam"),
                },
            );
            assert_eq!(legacy.used_by.len(), 1);
            assert_eq!(
                legacy.used_by[0].id,
                ObjectId::Partition {
                    table: NameKey::new("DimOld"),
                    partition: NameKey::new("DimOld"),
                }
            );
            assert!(legacy.used_by[0].also_unused);
            assert!(matches!(legacy.used_by[0].provenance, Provenance::M));
        }

        /// Shared expressions reference each other: a partition keeps its
        /// staging query alive, and the staging query keeps the parameter it
        /// names alive — one M edge per hop.
        #[test]
        fn an_m_chain_keeps_shared_expressions_alive() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    partitions: vec![m_partition(
                        "Sales",
                        "let Source = Sql.Database(#\"Staging Query\") in Source",
                    )],
                    ..Default::default()
                }],
                expressions: vec![
                    SharedExpression {
                        name: "Staging Query".to_string(),
                        expression: "ServerName".to_string(),
                    },
                    SharedExpression {
                        name: "ServerName".to_string(),
                        expression: "\"localhost\"".to_string(),
                    },
                ],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[column_target("Sales", "Anything")]);

            let graph = DependencyGraph::build(&db, &[&report]);
            let unused = graph.unused_objects();

            not_unused(
                &unused,
                &ObjectId::Expression {
                    name: NameKey::new("Staging Query"),
                },
            );
            not_unused(
                &unused,
                &ObjectId::Expression {
                    name: NameKey::new("ServerName"),
                },
            );

            // The second hop is the M-to-M edge: the staging query, not the
            // partition, is what names ServerName.
            assert_eq!(
                graph.consumers_of(&ObjectId::Expression {
                    name: NameKey::new("ServerName"),
                }),
                [(
                    ObjectId::Expression {
                        name: NameKey::new("Staging Query"),
                    },
                    Provenance::M
                )]
            );
        }

        /// A column named only inside its own table's Power Query partition
        /// is **not** kept alive. M produces the column and the model maps
        /// onto the query's output, so unloading the column cannot break
        /// refresh — the issue #39 keep was inverted. What the partition's
        /// mention is worth rides on the finding instead
        /// ([`UnusedObject::named_by_m`]): removing the column from the
        /// *script* too means editing those steps.
        #[test]
        fn an_m_partition_names_its_columns_without_keeping_them_alive() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    columns: vec![
                        column("Pk"),
                        column("Amount"),
                        column("Region"),
                        column("Orphaned"),
                    ],
                    partitions: vec![m_partition(
                        "Sales",
                        concat!(
                            "let\n",
                            "    Source = Sql.Database(ServerName, \"db\"),\n",
                            "    Typed = Table.TransformColumnTypes(Source, {{\"Amount\", type text}}),\n",
                            "    Expanded = Table.ExpandTableColumn(Typed, \"Detail\", {\"Region\"}),\n",
                            "    Filtered = Table.SelectRows(Expanded, each [Orphaned] = \"West\")\n",
                            "in\n",
                            "    Filtered",
                        ),
                    )],
                    ..Default::default()
                }],
                expressions: vec![SharedExpression {
                    name: "ServerName".to_string(),
                    expression: "\"localhost\"".to_string(),
                }],
                ..Default::default()
            };
            // The report binds Pk only: that keeps the table (and with it the
            // partition) alive, while Amount, Region, and Orphaned have no
            // DAX or report binding anywhere.
            let report = visual_page("P1", "V1", &[column_target("Sales", "Pk")]);

            let graph = DependencyGraph::build(&db, &[&report]);
            let unused = graph.unused_objects();

            let partition = ObjectId::Partition {
                table: NameKey::new("Sales"),
                partition: NameKey::new("Sales"),
            };
            let expected_named = [partition];
            for name in ["Amount", "Region", "Orphaned"] {
                let finding = find(&unused, &column_id("Sales", name));
                assert!(
                    finding.used_by.is_empty(),
                    "M names are not consumers: no edge points at the column"
                );
                assert_eq!(finding.named_by_m, expected_named);
            }
            // The shared expression the partition's M reads is still kept —
            // the liveness half of the rule.
            not_unused(
                &unused,
                &ObjectId::Expression {
                    name: NameKey::new("ServerName"),
                },
            );
        }

        /// A liveness edge must never outrun its owner: when the table is
        /// dead, its partition is unreachable and keeps nothing alive — the
        /// columns die with the table they belong to.
        #[test]
        fn a_dead_tables_partition_keeps_nothing_alive() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "DimOld".to_string(),
                    columns: vec![column("Key")],
                    partitions: vec![m_partition(
                        "DimOld",
                        "let Source = Table.SelectRows(#\"DimOld\", each [Key] <> null) in Source",
                    )],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let graph = DependencyGraph::build(&db, &[]);
            let unused = graph.unused_objects();

            find(&unused, &column_id("DimOld", "Key"));
            // The partition names DimOld itself and [Key]; the self-table
            // reference is dropped, but nothing else could keep the table
            // alive either.
            find(&unused, &table_id("DimOld"));
        }

        /// A table consumed only as another query's merge source is
        /// refresh-critical: `#"DimOld"` in a NestedJoin deletes the query the
        /// join reads when the table goes, so the table keeps alive. Its
        /// *column* does not — the `{"Key"}` strings merely name it.
        #[test]
        fn an_m_merge_source_keeps_the_joined_table_alive() {
            let db = TabularDatabase {
                tables: vec![
                    Table {
                        name: "Sales".to_string(),
                        columns: vec![column("Key")],
                        partitions: vec![m_partition(
                            "Sales",
                            concat!(
                                "let\n",
                                "    Source = Sql.Database(ServerName, \"db\"),\n",
                                "    Joined = Table.NestedJoin(Source, {\"Key\"}, #\"DimOld\", {\"Key\"}, \"Dim\")\n",
                                "in\n",
                                "    Joined",
                            ),
                        )],
                        ..Default::default()
                    },
                    Table {
                        name: "DimOld".to_string(),
                        columns: vec![column("Key")],
                        partitions: vec![m_partition("DimOld", "let Source = DimOld in Source")],
                        ..Default::default()
                    },
                ],
                expressions: vec![SharedExpression {
                    name: "ServerName".to_string(),
                    expression: "\"localhost\"".to_string(),
                }],
                ..Default::default()
            };
            // Sales is reachable only through a relationship-free report
            // binding on its column; DimOld has no binding anywhere.
            let report = visual_page("P1", "V1", &[column_target("Sales", "Key")]);

            let graph = DependencyGraph::build(&db, &[&report]);
            let unused = graph.unused_objects();

            not_unused(&unused, &table_id("DimOld"));
            // The join keys are named, not kept: Sales' partition rides on
            // DimOld's column finding as supply-chain context.
            let finding = find(&unused, &column_id("DimOld", "Key"));
            assert_eq!(
                finding.named_by_m,
                [ObjectId::Partition {
                    table: NameKey::new("Sales"),
                    partition: NameKey::new("Sales"),
                }]
            );
        }

        /// A qualified field access names the query it reads from:
        /// `#"DimOld"[Key]` keeps the whole DimOld table alive even when no
        /// argument-position mention of the table exists anywhere.
        #[test]
        fn a_qualified_m_field_access_keeps_the_named_table_alive() {
            let db = TabularDatabase {
                tables: vec![
                    Table {
                        name: "Sales".to_string(),
                        columns: vec![column("Key")],
                        partitions: vec![m_partition(
                            "Sales",
                            "let Source = #\"DimOld\"[Key] in Source",
                        )],
                        ..Default::default()
                    },
                    Table {
                        name: "DimOld".to_string(),
                        columns: vec![column("Key")],
                        partitions: vec![m_partition("DimOld", "let Source = DimOld in Source")],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[column_target("Sales", "Key")]);

            let graph = DependencyGraph::build(&db, &[&report]);
            let unused = graph.unused_objects();

            not_unused(&unused, &table_id("DimOld"));
        }

        /// The lexer narrowed the old substring match, deliberately: a shared
        /// expression whose name appears only inside an M comment or an
        /// unrelated string is no longer "referenced".
        #[test]
        fn a_name_inside_an_m_comment_or_string_keeps_nothing_alive() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    partitions: vec![m_partition(
                        "Sales",
                        concat!(
                            "let\n",
                            "    // ServerName was renamed; this step is retired.\n",
                            "    Text = \"ServerName is mentioned here as data\",\n",
                            "    Source = 1\n",
                            "in\n",
                            "    Source",
                        ),
                    )],
                    ..Default::default()
                }],
                expressions: vec![SharedExpression {
                    name: "ServerName".to_string(),
                    expression: "\"localhost\"".to_string(),
                }],
                ..Default::default()
            };
            let graph = DependencyGraph::build(&db, &[]);
            let unused = graph.unused_objects();

            find(
                &unused,
                &ObjectId::Expression {
                    name: NameKey::new("ServerName"),
                },
            );
        }

        /// A bookmark's saved filter is a root like a live one.
        #[test]
        fn a_bookmark_saved_filter_is_a_root() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    columns: vec![column("Region")],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let report = ReportModel {
                bookmarks: vec![Bookmark {
                    name: NameKey::new("B1"),
                    display_name: None,
                    filters: Vec::new(),
                    sections: vec![BookmarkSection {
                        page: NameKey::new("P1"),
                        filters: Vec::new(),
                        visuals: vec![BookmarkVisual {
                            visual: NameKey::new("V1"),
                            wells: Vec::new(),
                            filters: vec![Filter {
                                target: Some(column_target("Sales", "Region")),
                                ..Default::default()
                            }],
                        }],
                    }],
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[&report]);

            assert!(graph.unused_objects().is_empty());
            let roots = graph.roots();
            assert_eq!(roots.len(), 1);
            assert!(matches!(
                &roots[0].1,
                Provenance::Binding(edge) if edge.bookmark.is_some()
            ));
        }

        /// Engine-managed columns ride along with their table: calculated-table
        /// columns cannot be dropped independently.
        #[test]
        fn calculated_table_columns_stay_with_their_table() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Top Products".to_string(),
                    columns: vec![Column {
                        name: "Product".to_string(),
                        kind: ColumnKind::CalculatedTableColumn,
                        ..Default::default()
                    }],
                    partitions: vec![Partition {
                        name: "Top Products".to_string(),
                        source: PartitionSource::Calculated {
                            expression: "TOPN(10, 'Product')".to_string(),
                        },
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[column_target("Top Products", "Product")]);

            let graph = DependencyGraph::build(&db, &[&report]);

            assert!(graph.unused_objects().is_empty());
        }

        /// Calendar-bound columns ride along with their table: the engine
        /// materializes them through the calendar, so a column referenced
        /// only through a calendar is not dead.
        #[test]
        fn calendar_columns_stay_with_their_table() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Date".to_string(),
                    columns: vec![column("Day")],
                    calendars: vec![crate::model::Calendar {
                        name: "Fiscal Calendar".to_string(),
                        columns: vec!["Day".to_string()],
                    }],
                    measures: vec![measure("Rows", "COUNTROWS('Date')")],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let report = visual_page("P1", "V1", &[measure_target("Date", "Rows")]);

            let graph = DependencyGraph::build(&db, &[&report]);

            assert!(graph.unused_objects().is_empty());
        }

        /// A dead table drags its calendar-bound columns along, annotated:
        /// the calendar is the only thing that ever referenced them.
        #[test]
        fn a_dead_table_annotates_its_calendar_columns() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Date".to_string(),
                    columns: vec![column("Day")],
                    calendars: vec![crate::model::Calendar {
                        name: "Fiscal Calendar".to_string(),
                        columns: vec!["Day".to_string()],
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);
            let unused = graph.unused_objects();

            let day = find(&unused, &column_id("Date", "Day"));
            assert_eq!(day.used_by.len(), 1);
            assert_eq!(day.used_by[0].id, table_id("Date"));
            assert!(day.used_by[0].also_unused);
            assert!(matches!(
                day.used_by[0].provenance,
                Provenance::Structural {
                    role: StructuralEdge::EngineManaged
                }
            ));
        }
    }

    mod queries {
        use super::*;

        #[test]
        fn queries_on_an_unknown_object_are_empty() {
            let graph = DependencyGraph::build(&TabularDatabase::default(), &[]);

            assert!(graph.consumers_of(&table_id("Nope")).is_empty());
            assert!(graph.producers_of(&table_id("Nope")).is_empty());
            assert!(graph.roots_of(&table_id("Nope")).is_empty());
        }

        #[test]
        fn unused_objects_are_sorted_by_identity() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    columns: vec![column("B"), column("A")],
                    ..Default::default()
                }],
                ..Default::default()
            };

            let graph = DependencyGraph::build(&db, &[]);
            let unused = graph.unused_objects();
            let ids: Vec<&ObjectId> = unused.iter().map(|finding| &finding.id).collect();
            let mut sorted = ids.clone();
            sorted.sort();

            assert_eq!(ids, sorted);
        }

        #[test]
        fn the_root_carries_the_full_binding_provenance() {
            let db = TabularDatabase {
                tables: vec![Table {
                    name: "Sales".to_string(),
                    measures: vec![measure("Total", "0")],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let report = visual_page("P2", "Card", &[measure_target("Sales", "Total")]);

            let graph = DependencyGraph::build(&db, &[&report]);
            let roots = graph.roots();

            assert_eq!(roots.len(), 1);
            assert_eq!(roots[0].0, measure_id("Sales", "Total"));
            let Provenance::Binding(edge) = &roots[0].1 else {
                panic!("a root carries binding provenance");
            };
            let BindingEdge {
                kind,
                report: report_name,
                page,
                visual,
                bookmark,
            } = edge.as_ref();
            assert!(matches!(kind, BindingSite::FieldWell { role } if role == "Values"));
            assert_eq!(report_name.as_ref().map(NameKey::as_str), Some("Mini"));
            assert_eq!(page.as_ref().map(NameKey::as_str), Some("P2"));
            assert_eq!(visual.as_ref().map(NameKey::as_str), Some("Card"));
            assert!(bookmark.is_none());
        }
    }
}
