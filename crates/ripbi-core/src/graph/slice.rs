//! Multi-hop traversal over the dependency graph: the per-object slices the
//! `ripbi deps` view renders.
//!
//! Liveness (`reachability`) asks a global question — what does
//! *nothing* reach? — and filters edges by what confers liveness. A slice asks
//! a local one: starting from one object, what lies within `depth` edges
//! upstream (what it uses) or downstream (what uses it)? Exploration wants the
//! truth, not the liveness policy, so a slice follows **every** edge and keeps
//! its [`Provenance`] — the strong/weak-pass predicates stay private to
//! reachability.

use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::Direction;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::DependencyGraph;
use super::provenance::Provenance;
use crate::identity::ObjectId;

/// One dependency edge of a slice, in graph orientation: `from` uses `to`, and
/// [`provenance`](DepEdge::provenance) says how. A view that wants the other
/// orientation (impact trees read `to → from`) flips it at render time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepEdge {
    /// The consuming side of the edge.
    pub from: ObjectId,
    /// The used side of the edge.
    pub to: ObjectId,
    /// What kind of use the edge records.
    pub provenance: Provenance,
}

/// The slice of the dependency graph around one root: every object within a
/// depth cutoff, plus every edge between them (the induced subgraph, so cross
/// links and back edges a pure traversal tree would hide are kept — a cycle
/// between two reached objects is data, and `deps` renders it as one).
///
/// Deterministic for a given graph: `nodes` are sorted by object identity,
/// `edges` by `(from, to, provenance key)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepSlice {
    /// The object the traversal started from, as given.
    pub root: ObjectId,
    /// The depth cutoff the traversal ran with. `None` means unrestricted;
    /// `Some(n)` means every node is within `n` edges of the root.
    pub depth: Option<usize>,
    /// Every node of the slice, root included, sorted by object identity.
    /// Empty only when the root is not in the graph at all.
    pub nodes: Vec<ObjectId>,
    /// Every edge between two nodes of the slice, in graph orientation,
    /// sorted by `(from, to, provenance key)`.
    pub edges: Vec<DepEdge>,
}

impl DepSlice {
    /// The distinct objects `id` uses within the slice, with one provenance
    /// each, sorted by object identity. Empty for a leaf.
    pub fn producers_of(&self, id: &ObjectId) -> Vec<(&ObjectId, &Provenance)> {
        self.neighbors(|edge| edge.from == *id, |edge| &edge.to)
    }

    /// The distinct objects that use `id` within the slice, with one
    /// provenance each, sorted by object identity. Empty for a root with no
    /// consumers.
    pub fn consumers_of(&self, id: &ObjectId) -> Vec<(&ObjectId, &Provenance)> {
        self.neighbors(|edge| edge.to == *id, |edge| &edge.from)
    }

    fn neighbors(
        &self,
        keep: impl Fn(&DepEdge) -> bool,
        other: impl Fn(&DepEdge) -> &ObjectId,
    ) -> Vec<(&ObjectId, &Provenance)> {
        let mut out: Vec<(&ObjectId, &Provenance)> = self
            .edges
            .iter()
            .filter(|edge| keep(edge))
            .map(|edge| (other(edge), &edge.provenance))
            .collect();
        out.sort_by(|a, b| a.0.cmp(b.0));
        out.dedup_by(|a, b| a.0 == b.0);
        out
    }
}

impl DependencyGraph {
    /// Everything within `depth` edges upstream of `root`: the objects it uses,
    /// transitively. A `depth` of `None` traverses to the leaves; `Some(n)`
    /// keeps every node within `n` edges (minimum edge distance, so a node
    /// reachable by a short and a long path stays). An unknown root yields an
    /// empty slice — resolution is the caller's job.
    pub fn dependencies_of(&self, root: &ObjectId, depth: Option<usize>) -> DepSlice {
        self.slice(root, depth, Direction::Outgoing)
    }

    /// Everything within `depth` edges downstream of `root`: the objects that
    /// use it, transitively. Report bindings are not edges — they ride along
    /// through [`DependencyGraph::roots_of`] on the slice's objects.
    pub fn impact_of(&self, root: &ObjectId, depth: Option<usize>) -> DepSlice {
        self.slice(root, depth, Direction::Incoming)
    }

    /// The number of dependency edges in the whole graph — one of the
    /// `ripbi deps` overview's counts.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// One BFS over `direction` edges from `root`, then the induced edge set
    /// over the visited nodes in both directions.
    fn slice(&self, root: &ObjectId, depth: Option<usize>, direction: Direction) -> DepSlice {
        let Some(&start) = self.nodes.get(root) else {
            return DepSlice {
                root: root.clone(),
                depth,
                nodes: Vec::new(),
                edges: Vec::new(),
            };
        };
        let max = depth.unwrap_or(usize::MAX);

        // Breadth-first with minimum edge distance, so a node reached at two
        // depths stays at the shorter one. The visited set is the cycle break.
        let mut level: HashMap<NodeIndex, usize> = HashMap::from([(start, 0)]);
        let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::from([(start, 0)]);
        while let Some((index, distance)) = queue.pop_front() {
            if distance == max {
                continue;
            }
            for edge in self.graph.edges_directed(index, direction) {
                let other = match direction {
                    Direction::Outgoing => edge.target(),
                    Direction::Incoming => edge.source(),
                };
                if level.insert(other, distance + 1).is_none() {
                    queue.push_back((other, distance + 1));
                }
            }
        }

        let mut nodes: Vec<ObjectId> = level
            .keys()
            .map(|index| self.graph[*index].clone())
            .collect();
        nodes.sort();

        let visited: HashSet<NodeIndex> = level.keys().copied().collect();
        let mut edges: Vec<DepEdge> = level
            .keys()
            .flat_map(|index| self.graph.edges_directed(*index, Direction::Outgoing))
            .filter(|edge| visited.contains(&edge.target()))
            .map(|edge| DepEdge {
                from: self.graph[edge.source()].clone(),
                to: self.graph[edge.target()].clone(),
                provenance: edge.weight().clone(),
            })
            .collect();
        edges.sort_by(|a, b| {
            (&a.from, &a.to, a.provenance.key()).cmp(&(&b.from, &b.to, b.provenance.key()))
        });

        DepSlice {
            root: root.clone(),
            depth,
            nodes,
            edges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::StructuralEdge;
    use crate::identity::NameKey;
    use crate::model::{Column, Measure, Table, TabularDatabase};
    use crate::report::{FieldTarget, FieldWell, Page, Projection, ReportModel, Visual};

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

    fn table(name: &str, columns: Vec<Column>, measures: Vec<Measure>) -> Table {
        Table {
            name: name.to_string(),
            columns,
            measures,
            ..Default::default()
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

    fn table_id(name: &str) -> ObjectId {
        ObjectId::Table {
            table: NameKey::new(name),
        }
    }

    fn measure_target(table: &str, name: &str) -> FieldTarget {
        FieldTarget::Measure {
            home_table: Some(NameKey::new(table)),
            measure: NameKey::new(name),
        }
    }

    /// One visual on page `P1` projecting `targets` into its Values well.
    fn bound_report(targets: &[FieldTarget]) -> ReportModel {
        ReportModel {
            name: Some("Mini".to_string()),
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

    /// `Margin %` → (`Margin`, `Revenue`); `Margin` → (`Revenue`, `Cost`);
    /// `Revenue` → `Amount`; `Cost` → `Cost Amount`; every member into
    /// `Sales`; the visual binds `Margin %`.
    fn chain_model() -> (DependencyGraph, ObjectId) {
        let db = TabularDatabase {
            tables: vec![table(
                "Sales",
                vec![column("Amount"), column("Cost Amount")],
                vec![
                    measure("Revenue", "SUM('Sales'[Amount])"),
                    measure("Cost", "SUM('Sales'[Cost Amount])"),
                    measure("Margin", "[Revenue] - [Cost]"),
                    measure("Margin %", "DIVIDE([Margin], [Revenue])"),
                ],
            )],
            ..Default::default()
        };
        let report = bound_report(&[measure_target("Sales", "Margin %")]);
        let graph = DependencyGraph::build(&db, &[&report]);
        (graph, measure_id("Sales", "Margin %"))
    }

    mod dependencies {
        use super::*;

        #[test]
        fn traverses_dax_references_transitively() {
            let (graph, root) = chain_model();

            let slice = graph.dependencies_of(&root, None);

            for id in [
                &root,
                &measure_id("Sales", "Margin"),
                &measure_id("Sales", "Revenue"),
                &measure_id("Sales", "Cost"),
                &column_id("Sales", "Amount"),
                &column_id("Sales", "Cost Amount"),
            ] {
                assert!(slice.nodes.contains(id), "{id} must be in the slice");
            }
        }

        #[test]
        fn depth_one_keeps_direct_producers_only() {
            let (graph, root) = chain_model();

            let slice = graph.dependencies_of(&root, Some(1));

            assert!(slice.nodes.contains(&measure_id("Sales", "Margin")));
            assert!(
                !slice.nodes.contains(&measure_id("Sales", "Amount")),
                "depth 1 must not reach past the direct producers"
            );
        }

        #[test]
        fn a_node_reached_at_two_depths_stays_at_the_shorter_one() {
            // `Total` uses `Amount` directly and via `Sub`: at depth 1 both
            // columns may appear, and the direct edge must win over the longer
            // path.
            let db = TabularDatabase {
                tables: vec![table(
                    "Sales",
                    vec![column("Amount")],
                    vec![
                        measure("Sub", "[Amount] + 0"),
                        measure("Total", "[Amount] + [Sub]"),
                    ],
                )],
                ..Default::default()
            };
            let graph = DependencyGraph::build(&db, &[]);

            let slice = graph.dependencies_of(&measure_id("Sales", "Total"), Some(1));

            assert_eq!(
                slice.nodes,
                vec![
                    table_id("Sales"),
                    column_id("Sales", "Amount"),
                    measure_id("Sales", "Sub"),
                    measure_id("Sales", "Total"),
                ]
            );
        }

        #[test]
        fn containment_edges_reach_the_table() {
            let (graph, root) = chain_model();

            let slice = graph.dependencies_of(&root, None);

            assert!(
                slice
                    .edges
                    .iter()
                    .any(|edge| edge.from == column_id("Sales", "Amount")
                        && edge.to == table_id("Sales")
                        && matches!(
                            edge.provenance,
                            Provenance::Structural {
                                role: StructuralEdge::TableMember
                            }
                        )),
                "a column's table member edge is part of its dependency slice"
            );
        }

        #[test]
        fn an_unknown_root_yields_an_empty_slice() {
            let (graph, _root) = chain_model();

            let slice = graph.dependencies_of(&measure_id("Nope", "Nothing"), None);

            assert!(slice.nodes.is_empty());
            assert!(slice.edges.is_empty());
        }

        #[test]
        fn output_is_deterministically_sorted() {
            let (graph, root) = chain_model();

            let slice = graph.dependencies_of(&root, None);

            let mut sorted = slice.nodes.clone();
            sorted.sort();
            assert_eq!(slice.nodes, sorted);
            let keys: Vec<_> = slice
                .edges
                .iter()
                .map(|edge| (&edge.from, &edge.to, edge.provenance.key()))
                .collect();
            let mut sorted_keys = keys.clone();
            sorted_keys.sort();
            assert_eq!(keys, sorted_keys);
        }
    }

    mod impact {
        use super::*;

        #[test]
        fn traverses_consumers_upstream_of_the_root() {
            let (graph, root) = chain_model();

            let slice = graph.impact_of(&column_id("Sales", "Amount"), None);

            assert!(slice.nodes.contains(&measure_id("Sales", "Revenue")));
            assert!(slice.nodes.contains(&measure_id("Sales", "Margin")));
            assert!(slice.nodes.contains(&root), "Margin % consumes Amount");
        }

        #[test]
        fn depth_one_keeps_direct_consumers_only() {
            let (graph, root) = chain_model();

            let slice = graph.impact_of(&column_id("Sales", "Amount"), Some(1));

            assert!(slice.nodes.contains(&measure_id("Sales", "Revenue")));
            assert!(
                !slice.nodes.contains(&root),
                "Margin % is two edges from Amount"
            );
        }

        #[test]
        fn the_induced_edge_set_keeps_cross_links_between_reached_nodes() {
            // Impacting `Amount` walks incoming edges: `Revenue` is reached
            // directly, `Margin %` two hops later. The edge `Margin % →
            // Revenue` is outgoing from the reached side, so the BFS never
            // traverses it — but both endpoints are in the slice, so the
            // induced subgraph keeps it. At depth 1 `Margin %` is out, and
            // with it the edge.
            let (graph, root) = chain_model();
            let amount = column_id("Sales", "Amount");
            let revenue = measure_id("Sales", "Revenue");
            let margin_pct_to_revenue = |edges: &[DepEdge]| {
                edges
                    .iter()
                    .any(|edge| edge.from == root && edge.to == revenue)
            };

            let full = graph.impact_of(&amount, None);
            assert!(full.nodes.contains(&root));
            assert!(
                margin_pct_to_revenue(&full.edges),
                "an edge between two reached nodes is induced even though the BFS never traversed it"
            );

            let short = graph.impact_of(&amount, Some(1));
            assert!(!short.nodes.contains(&root));
            assert!(
                !margin_pct_to_revenue(&short.edges),
                "an edge to an unreached node is not in the induced subgraph"
            );
        }

        #[test]
        fn bindings_are_not_edges_but_stay_queryable_per_node() {
            let (graph, root) = chain_model();

            let slice = graph.impact_of(&root, None);

            assert!(
                !slice
                    .edges
                    .iter()
                    .any(|edge| matches!(edge.provenance, Provenance::Binding(_))),
                "report bindings live beside the graph"
            );
            assert_eq!(graph.roots_of(&root).len(), 1);
        }
    }

    mod cycles {
        use super::*;

        /// Two measures referencing each other are a modeling error the engine
        /// would reject, but the lexer cannot know that — the graph records the
        /// cycle, and traversal must terminate on it.
        fn cyclic_model() -> DependencyGraph {
            let db = TabularDatabase {
                tables: vec![table(
                    "Sales",
                    vec![],
                    vec![measure("A", "[B] + 1"), measure("B", "[A] + 1")],
                )],
                ..Default::default()
            };
            DependencyGraph::build(&db, &[])
        }

        #[test]
        fn traversal_terminates_and_reports_the_cycle() {
            let graph = cyclic_model();
            let root = measure_id("Sales", "A");

            let slice = graph.dependencies_of(&root, None);

            assert_eq!(
                slice.nodes,
                vec![
                    table_id("Sales"),
                    measure_id("Sales", "A"),
                    measure_id("Sales", "B"),
                ]
            );
            // The back edge survives: `B` uses `A`, and `A` is in the slice.
            assert!(
                slice
                    .edges
                    .iter()
                    .any(|edge| edge.from == measure_id("Sales", "B")
                        && edge.to == measure_id("Sales", "A"))
            );
        }

        #[test]
        fn depth_cutoffs_hold_inside_a_cycle() {
            let graph = cyclic_model();

            // One edge from `A` reaches `B` and the table; the back edge from
            // `B` to `A` must not drag anything new in at depth 1.
            let slice = graph.dependencies_of(&measure_id("Sales", "A"), Some(1));

            assert!(slice.nodes.contains(&measure_id("Sales", "B")));
            assert_eq!(slice.nodes.len(), 3);
        }
    }

    mod neighbors {
        use super::*;

        #[test]
        fn slice_neighbors_mirror_the_edges_in_both_directions() {
            let (graph, root) = chain_model();

            let slice = graph.dependencies_of(&root, None);

            let producers = slice.producers_of(&root);
            assert!(
                producers
                    .iter()
                    .any(|(id, _)| *id == &measure_id("Sales", "Margin"))
            );
            let consumers = slice.consumers_of(&measure_id("Sales", "Margin"));
            assert_eq!(consumers.len(), 1);
            assert_eq!(*consumers[0].0, root);
        }
    }

    mod overview {
        use super::*;

        #[test]
        fn edge_count_counts_the_whole_graph() {
            let (graph, _root) = chain_model();

            // Six member edges (four measures + two columns into `Sales`) and
            // six DAX reference edges (Revenue→Amount, Cost→Cost Amount,
            // Margin→Revenue, Margin→Cost, Margin %→Margin, Margin %→Revenue).
            assert_eq!(graph.edge_count(), 12);
        }
    }
}
