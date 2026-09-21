//! The `deps` presentation layer: the human trees, `--plain` records, and
//! `--json` graph — three renderings of the same slices, with identical
//! selection semantics. Change them together with the orchestration module
//! and `docs/deps.md`.

use std::collections::{BTreeMap, HashSet};
use std::io;

use serde::Serialize;

use ripbi_core::{BindingEdge, BindingSite, DepSlice, ObjectId};

use super::tree::{Node, Orientation, TreeBuilder};
use crate::error::ScanError;
use crate::render::kind_of;
use crate::style::Palette;

/// What `deps` prints: the whole-graph overview, or one object's view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DepsOutput {
    /// The compact overview printed with no object and no selectors — never
    /// the whole graph.
    Overview(OverviewOut),
    /// One object's dependencies and/or impact, as the traversed slices.
    /// Boxed: the slices dwarf the overview's counts.
    Focused(Box<FocusedOut>),
}

/// The overview's counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OverviewOut {
    /// Every model and report object in the graph.
    pub objects: usize,
    /// Every dependency edge between them.
    pub edges: usize,
    /// Every report binding (the graph's roots).
    pub bindings: usize,
    /// Object counts by type bucket.
    pub by_type: ByType,
}

/// The overview's by-type buckets, in the fixed order the human view prints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ByType {
    /// Measures.
    pub measures: usize,
    /// Columns.
    pub columns: usize,
    /// Hierarchies.
    pub hierarchies: usize,
    /// Relationships.
    pub relationships: usize,
    /// Every other kind — tables, partitions, roles, calculation items,
    /// shared expressions, functions, report measures.
    pub other: usize,
}

impl ByType {
    /// `(label, count)` in the fixed human order.
    fn rows(&self) -> [(&'static str, usize); 5] {
        [
            ("Measures", self.measures),
            ("Columns", self.columns),
            ("Hierarchies", self.hierarchies),
            ("Relationships", self.relationships),
            ("Other", self.other),
        ]
    }
}

/// One object's focused view: the traversed slices, not yet rendered. The
/// human trees are projections built at render time; `--plain` and `--json`
/// read the slices directly and never truncate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FocusedOut {
    /// The selected object.
    pub root: ObjectId,
    /// The upstream slice; `None` when only `--impact` was asked for.
    pub dependencies: Option<DepSlice>,
    /// The downstream slice; `None` when only `--dependencies` was asked for.
    pub impact: Option<DepSlice>,
    /// The report bindings riding on the impact slice, as
    /// `(target object, binding)` pairs in slice order.
    pub bindings: Vec<(ObjectId, BindingEdge)>,
}

/// Writes the human-readable output: the object header, then one tree
/// section per active direction.
///
/// # Errors
/// Propagates stream write failures.
pub fn human(out: &mut dyn io::Write, palette: &Palette, output: &DepsOutput) -> io::Result<()> {
    match output {
        DepsOutput::Overview(overview) => write_overview(out, palette, overview),
        DepsOutput::Focused(focused) => write_focused(out, palette, focused),
    }
}

fn write_overview(
    out: &mut dyn io::Write,
    palette: &Palette,
    overview: &OverviewOut,
) -> io::Result<()> {
    writeln!(out, "{}", palette.bold("Dependency graph"))?;
    writeln!(out)?;
    writeln!(out, "{} model objects", overview.objects)?;
    writeln!(out, "{} dependency edges", overview.edges)?;
    writeln!(out, "{} report bindings", overview.bindings)?;
    writeln!(out)?;
    writeln!(out, "By type")?;
    for (label, count) in overview.by_type.rows() {
        writeln!(out, "  {label:<14}{count}")?;
    }
    writeln!(out)?;
    writeln!(out, "Try:")?;
    writeln!(out, "  ripbi deps \"'Table'[Name]\"")?;
    Ok(())
}

fn write_focused(
    out: &mut dyn io::Write,
    palette: &Palette,
    focused: &FocusedOut,
) -> io::Result<()> {
    let root = &focused.root;
    writeln!(out, "{}  {}", root, palette.dim(kind_of(root)))?;
    let label = |id: &ObjectId| format!("{}  {}", id, kind_of(id));

    if let Some(slice) = &focused.dependencies {
        let mut builder = TreeBuilder::new(Orientation::Dependencies);
        let tree = builder.build(slice, root, label);
        write_section(
            out,
            palette,
            "Dependencies",
            &tree,
            builder.suppressed(),
            slice,
        )?;
    }
    if let Some(slice) = &focused.impact {
        writeln!(out)?;
        writeln!(out, "{}", palette.bold("Impact"))?;
        let mut builder = TreeBuilder::new(Orientation::Impact);
        let tree = builder.build(slice, root, label);
        if tree.children.is_empty() && focused.bindings.is_empty() {
            writeln!(out, "└─ nothing")?;
        } else {
            // The Model section says so even when empty — "no model object
            // uses this, but reports do" is exactly the distinction the two
            // sections exist to draw.
            write_section(out, palette, "Model", &tree, builder.suppressed(), slice)?;
            if !focused.bindings.is_empty() {
                writeln!(out)?;
                writeln!(out, "{}", palette.bold("Reports"))?;
                write_children(out, &report_forest(focused), "")?;
            }
        }
    }
    Ok(())
}

/// One titled tree section — its header, its tree, and the truncation footer
/// when the budget cut branches. The footer is the one place that points at
/// the other output modes instead of drowning the terminal.
fn write_section(
    out: &mut dyn io::Write,
    palette: &Palette,
    title: &str,
    tree: &Node,
    suppressed: bool,
    slice: &DepSlice,
) -> io::Result<()> {
    writeln!(out)?;
    writeln!(out, "{}", palette.bold(title))?;
    if tree.children.is_empty() {
        writeln!(out, "└─ nothing")?;
        return Ok(());
    }
    write_children(out, &tree.children, "")?;
    if suppressed {
        writeln!(out)?;
        writeln!(out, "{} nodes are reachable.", slice.nodes.len())?;
        writeln!(out, "{TRUNCATION_FOOTER}")?;
    }
    Ok(())
}

/// The footer a budget-truncated section prints.
const TRUNCATION_FOOTER: &str = "Use --depth 2 for a smaller view, --graph \
 to inspect topology, or --json for the complete graph.";

/// Draws one level of the tree: `├─`/`└─` connectors, `│` continuation
/// lines, and the `… N additional branches` line under every branch the
/// budget cut. Meaning never depends on colour — the markers are characters.
fn write_children(out: &mut dyn io::Write, children: &[Node], prefix: &str) -> io::Result<()> {
    for (index, node) in children.iter().enumerate() {
        let last = index + 1 == children.len();
        let connector = if last { "└─ " } else { "├─ " };
        let note = node
            .note
            .map(|note| format!("  {note}"))
            .unwrap_or_default();
        writeln!(out, "{prefix}{connector}{}{note}", node.label)?;
        let deeper = format!("{prefix}{}", if last { "   " } else { "│  " });
        write_children(out, &node.children, &deeper)?;
        if node.hidden > 0 {
            writeln!(out, "{deeper}└─ … {} additional branches", node.hidden)?;
        }
    }
    Ok(())
}

/// The report bindings of the impact slice as deterministic tries:
/// `report → page → visual → binding site`, bookmarks as their own level,
/// mobile layouts marked. When every binding lands on the selected object
/// the object level is left out — the header already names it; when several
/// objects carry bindings, each gets its own labeled subtree.
fn report_forest(focused: &FocusedOut) -> Vec<Node> {
    let root = &focused.root;
    if focused.bindings.iter().all(|(id, _)| id == root) {
        let edges: Vec<&BindingEdge> = focused.bindings.iter().map(|(_, edge)| edge).collect();
        return binding_trie(&edges);
    }
    // Grouped by the object each binding lands on, in slice (identity) order.
    let mut by_target: Vec<(ObjectId, Vec<&BindingEdge>)> = Vec::new();
    for (id, edge) in &focused.bindings {
        match by_target.last_mut() {
            Some((last, edges)) if *last == *id => edges.push(edge),
            _ => by_target.push((id.clone(), vec![edge])),
        }
    }
    by_target
        .into_iter()
        .map(|(id, edges)| Node {
            label: format!("{}  {}", id, kind_of(&id)),
            note: None,
            children: binding_trie(&edges),
            hidden: 0,
        })
        .collect()
}

/// The `report → page → visual → site` tries of one binding set, one root
/// node per report. Absent levels are transparent: a report-level filter
/// hangs straight off the report (or off the trie root when nothing else is
/// known).
fn binding_trie(edges: &[&BindingEdge]) -> Vec<Node> {
    let mut trie = Trie::default();
    for edge in edges {
        let mut keys: Vec<String> = Vec::new();
        if let Some(report) = &edge.report {
            keys.push(report.as_str().to_string());
        }
        if let Some(bookmark) = &edge.bookmark {
            keys.push(bookmark.as_str().to_string());
        }
        if let Some(page) = &edge.page {
            keys.push(page.as_str().to_string());
        }
        if let Some(visual) = &edge.visual {
            keys.push(format!("{}  visual", visual.as_str()));
        }
        insert(&mut trie, &keys, site_label(edge));
    }
    trie_to_node(trie).children
}

/// The leaf label of one binding: a field well renders its role (`Values`),
/// every other kind its site phrase, and a phone-layout binding says so.
fn site_label(edge: &BindingEdge) -> String {
    let mut label = match &edge.kind {
        BindingSite::FieldWell { role } => role.clone(),
        other => other.to_string(),
    };
    if edge.mobile {
        label.push_str("  (mobile layout)");
    }
    label
}

#[derive(Default)]
struct Trie {
    children: BTreeMap<String, Trie>,
    leaves: Vec<String>,
}

fn insert(trie: &mut Trie, keys: &[String], leaf: String) {
    match keys.split_first() {
        None => trie.leaves.push(leaf),
        Some((key, rest)) => insert(trie.children.entry(key.clone()).or_default(), rest, leaf),
    }
}

fn trie_to_node(trie: Trie) -> Node {
    let mut children: Vec<Node> = trie
        .children
        .into_iter()
        .map(|(key, child)| {
            let mut node = trie_to_node(child);
            node.label = key;
            node
        })
        .collect();
    let mut leaves = trie.leaves;
    leaves.sort();
    leaves.dedup();
    children.extend(leaves.iter().map(|leaf| Node::leaf(leaf.clone())));
    Node {
        label: String::new(),
        note: None,
        children,
        hidden: 0,
    }
}

/// Writes `--plain`: one typed record per line, for grep/cut/awk. Never
/// truncated.
///
/// # Errors
/// Propagates stream write failures.
pub fn plain(out: &mut dyn io::Write, output: &DepsOutput) -> io::Result<()> {
    match output {
        DepsOutput::Overview(overview) => {
            writeln!(out, "objects\t{}", overview.objects)?;
            writeln!(out, "edges\t{}", overview.edges)?;
            writeln!(out, "bindings\t{}", overview.bindings)?;
            for (key, count) in [
                ("measures", overview.by_type.measures),
                ("columns", overview.by_type.columns),
                ("hierarchies", overview.by_type.hierarchies),
                ("relationships", overview.by_type.relationships),
                ("other", overview.by_type.other),
            ] {
                writeln!(out, "{key}\t{count}")?;
            }
            Ok(())
        }
        DepsOutput::Focused(focused) => {
            if let Some(slice) = &focused.dependencies {
                // Graph orientation: the consumer, what it uses, and why.
                for edge in &slice.edges {
                    writeln!(
                        out,
                        "dependency\t{}\t{}\t{}",
                        edge.from,
                        edge.to,
                        edge.provenance.key()
                    )?;
                }
            }
            if let Some(slice) = &focused.impact {
                // Flipped: the queried-object side first, then what uses it.
                for edge in &slice.edges {
                    writeln!(
                        out,
                        "impact\t{}\t{}\t{}",
                        edge.to,
                        edge.from,
                        edge.provenance.key()
                    )?;
                }
                for (id, edge) in &focused.bindings {
                    writeln!(
                        out,
                        "binding\t{}\t{}\t{}\t{}\t{}",
                        id,
                        edge.report.as_ref().map_or("-", |r| r.as_str()),
                        edge.page.as_ref().map_or("-", |p| p.as_str()),
                        edge.visual.as_ref().map_or("-", |v| v.as_str()),
                        site_field(edge)
                    )?;
                }
            }
            Ok(())
        }
    }
}

/// The binding record's sixth field: a field well's role (`Values`), or the
/// machine kind of every other binding.
fn site_field(edge: &BindingEdge) -> &str {
    match &edge.kind {
        BindingSite::FieldWell { role } => role.as_str(),
        other => other.key(),
    }
}

/// Writes `--json`: the graph itself, `schema_version` 1. Never truncated.
///
/// # Errors
/// Propagates serialization or stream write failures.
pub fn json(out: &mut dyn io::Write, output: &DepsOutput) -> Result<(), ScanError> {
    match output {
        DepsOutput::Overview(overview) => write_json(
            out,
            &JsonOverview {
                schema_version: 1,
                objects: overview.objects,
                edges: overview.edges,
                bindings: overview.bindings,
                by_type: overview.by_type,
            },
        ),
        DepsOutput::Focused(focused) => {
            let root = &focused.root;

            let mut node_ids: Vec<&ObjectId> = Vec::new();
            if let Some(slice) = &focused.dependencies {
                node_ids.extend(slice.nodes.iter());
            }
            if let Some(slice) = &focused.impact {
                node_ids.extend(slice.nodes.iter());
            }
            node_ids.sort();
            node_ids.dedup();
            let nodes: Vec<JsonNode> = node_ids
                .into_iter()
                .map(|id| JsonNode {
                    id: id.to_string(),
                    kind: kind_of(id),
                })
                .collect();

            // Dependency edges keep the graph orientation (consumer →
            // producer); edges only the impact slice reached are flipped so
            // `from` is always the queried-object side, and `kind` says
            // which. An edge both slices share is emitted once, as a
            // dependency.
            let mut edges: Vec<JsonEdge> = Vec::new();
            let mut seen: HashSet<(String, String, &'static str)> = HashSet::new();
            if let Some(slice) = &focused.dependencies {
                for edge in &slice.edges {
                    seen.insert((
                        edge.from.to_string(),
                        edge.to.to_string(),
                        edge.provenance.key(),
                    ));
                    edges.push(JsonEdge {
                        from: edge.from.to_string(),
                        to: edge.to.to_string(),
                        kind: "dependency",
                        provenance: edge.provenance.key(),
                    });
                }
            }
            if let Some(slice) = &focused.impact {
                for edge in &slice.edges {
                    let forward = (
                        edge.from.to_string(),
                        edge.to.to_string(),
                        edge.provenance.key(),
                    );
                    if seen.contains(&forward) {
                        continue;
                    }
                    edges.push(JsonEdge {
                        from: edge.to.to_string(),
                        to: edge.from.to_string(),
                        kind: "impact",
                        provenance: edge.provenance.key(),
                    });
                }
            }
            edges.sort_by(|a, b| {
                (&a.from, &a.to, a.kind, a.provenance).cmp(&(&b.from, &b.to, b.kind, b.provenance))
            });

            let bindings: Vec<JsonBinding> = focused
                .bindings
                .iter()
                .map(|(id, edge)| JsonBinding {
                    object: id.to_string(),
                    report: edge.report.as_ref().map(|r| r.as_str().to_string()),
                    page: edge.page.as_ref().map(|p| p.as_str().to_string()),
                    visual: edge.visual.as_ref().map(|v| v.as_str().to_string()),
                    bookmark: edge.bookmark.as_ref().map(|b| b.as_str().to_string()),
                    mobile: edge.mobile,
                    binding: match &edge.kind {
                        BindingSite::FieldWell { role } => Some(role.clone()),
                        _ => None,
                    },
                    kind: edge.kind.key(),
                })
                .collect();

            write_json(
                out,
                &JsonFocused {
                    schema_version: 1,
                    root: JsonRoot {
                        id: root.to_string(),
                        kind: kind_of(root),
                    },
                    nodes,
                    edges,
                    bindings,
                },
            )
        }
    }
}

fn write_json<T: Serialize>(out: &mut dyn io::Write, payload: &T) -> Result<(), ScanError> {
    serde_json::to_writer_pretty(&mut *out, payload)
        .map_err(|error| ScanError::new(format!("cannot serialize JSON: {error}")))?;
    writeln!(out).map_err(ScanError::from)?;
    Ok(())
}

/// The JSON document of one focused view: the graph itself, versioned.
#[derive(Serialize)]
struct JsonFocused {
    schema_version: u8,
    root: JsonRoot,
    nodes: Vec<JsonNode>,
    edges: Vec<JsonEdge>,
    bindings: Vec<JsonBinding>,
}

#[derive(Serialize)]
struct JsonRoot {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct JsonNode {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct JsonEdge {
    from: String,
    to: String,
    /// `dependency` for upstream edges (graph orientation), `impact` for
    /// edges reached only downstream (flipped, so `from` is the
    /// queried-object side).
    kind: &'static str,
    provenance: &'static str,
}

#[derive(Serialize)]
struct JsonBinding {
    object: String,
    report: Option<String>,
    page: Option<String>,
    visual: Option<String>,
    bookmark: Option<String>,
    mobile: bool,
    /// The field-well role (`Values`), when the binding is a field well.
    binding: Option<String>,
    /// The binding kind, snake_case.
    kind: &'static str,
}

/// The JSON document of the overview.
#[derive(Serialize)]
struct JsonOverview {
    schema_version: u8,
    objects: usize,
    edges: usize,
    bindings: usize,
    by_type: ByType,
}
