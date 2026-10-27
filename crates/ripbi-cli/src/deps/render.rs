//! The `deps` presentation layer: the human trees, `--plain` records, and
//! `--json` graph — three renderings of the same slices, with identical
//! selection semantics. Change them together with the orchestration module
//! and `docs/deps.md`.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::time::Duration;

use ripbi_core::{BindingEdge, BindingSite, DepSlice, KeptObject, ObjectId, QueriedObject};

use super::tree::{Node, Orientation, TreeBuilder};

use crate::render::kind_of;
use crate::style::Palette;
pub(crate) use ripbi_app::deps::json::write as json;
pub(crate) use ripbi_app::deps::{DepsOutput, FocusedOut, OverviewOut};

/// The closing line of the human and `--graph` views, last because a
/// terminal leaves it in view: what the selection uses and what uses it,
/// e.g. `'Sales'[Profit %]   uses 12 objects · used by 0 objects, 3 report
/// bindings`. The overview is its own summary and ends with its `Try:` list;
/// it gets only the wall time, on a terminal.
///
/// # Errors
/// Propagates stream write failures.
pub fn closing(
    out: &mut dyn io::Write,
    palette: &Palette,
    output: &DepsOutput,
    elapsed: Option<Duration>,
) -> io::Result<()> {
    let elapsed = elapsed.map(|elapsed| palette.dim(&crate::render::format_elapsed(elapsed)));
    let focused = match output {
        DepsOutput::Overview(_) => {
            if let Some(elapsed) = elapsed {
                writeln!(out, "{elapsed}")?;
            }
            return Ok(());
        }
        DepsOutput::Focused(focused) => focused,
    };
    let subject = match focused.roots.as_slice() {
        [only] => only.to_string(),
        roots => count(roots.len(), "object"),
    };
    // A slice holds its root; everything else in it is the answer.
    let reached = |slices: &[DepSlice]| -> usize {
        slices
            .iter()
            .map(|slice| slice.nodes.len().saturating_sub(1))
            .sum()
    };
    let mut parts = Vec::new();
    if let Some(slices) = &focused.dependencies {
        parts.push(format!("uses {}", count(reached(slices), "object")));
    }
    if let Some(slices) = &focused.impact {
        let bindings: usize = focused.bindings.iter().map(Vec::len).sum();
        let mut used_by = Vec::new();
        if !focused.model_hidden {
            used_by.push(count(reached(slices), "object"));
        }
        used_by.push(count(bindings, "report binding"));
        parts.push(format!("used by {}", used_by.join(", ")));
    }
    let elapsed = elapsed
        .map(|elapsed| format!("  {elapsed}"))
        .unwrap_or_default();
    writeln!(
        out,
        "\n{}   {}{elapsed}",
        palette.bold(&subject),
        parts.join(" · ")
    )
}

/// `1 object`, `3 objects`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
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

/// Writes the `--graph` view: the overview unchanged, or each root's slices
/// as layered diagrams.
///
/// # Errors
/// Propagates stream write failures.
pub fn graph(out: &mut dyn io::Write, palette: &Palette, output: &DepsOutput) -> io::Result<()> {
    let focused = match output {
        DepsOutput::Overview(overview) => return write_overview(out, palette, overview),
        DepsOutput::Focused(focused) => focused,
    };
    for (index, root) in focused.roots.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        writeln!(out, "{}  {}", root, palette.dim(kind_of(root)))?;
        if let Some(slice) = focused.dependencies.as_ref().and_then(|s| s.get(index)) {
            writeln!(out)?;
            writeln!(out, "{}", palette.bold("Dependencies"))?;
            write!(
                out,
                "{}",
                super::graph_view::diagram(slice, super::graph_view::Arrow::Right)
            )?;
        }
        if let Some(slice) = focused.impact.as_ref().and_then(|s| s.get(index)) {
            writeln!(out)?;
            writeln!(out, "{}", palette.bold("Impact"))?;
            write!(
                out,
                "{}",
                super::graph_view::diagram(slice, super::graph_view::Arrow::Left)
            )?;
        }
    }
    Ok(())
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
    writeln!(out, "  ripbi deps --table Sales")?;
    writeln!(out, "  ripbi deps --type measure")?;
    Ok(())
}

fn write_focused(
    out: &mut dyn io::Write,
    palette: &Palette,
    focused: &FocusedOut,
) -> io::Result<()> {
    let label = |id: &ObjectId| format!("{}  {}", id, kind_of(id));
    for (index, root) in focused.roots.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        writeln!(out, "{}  {}", root, palette.dim(kind_of(root)))?;

        if let Some(slices) = &focused.dependencies {
            let slice = &slices[index];
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
        if let Some(slices) = &focused.impact {
            let slice = &slices[index];
            let bindings = &focused.bindings[index];
            let kept = &focused.kept[index];
            let queried = &focused.queried[index];
            writeln!(out)?;
            writeln!(out, "{}", palette.bold("Impact"))?;
            let mut builder = TreeBuilder::new(Orientation::Impact);
            let tree = builder.build(slice, root, label);
            let model_hidden = focused.model_hidden || tree.children.is_empty();
            if model_hidden && bindings.is_empty() && kept.is_empty() && queried.is_empty() {
                writeln!(out, "└─ nothing")?;
            } else {
                if !focused.model_hidden {
                    // The Model section says so even when empty — "no model
                    // object uses this, but reports do" is exactly the
                    // distinction the two sections exist to draw.
                    write_section(out, palette, "Model", &tree, builder.suppressed(), slice)?;
                }
                if !bindings.is_empty() {
                    writeln!(out)?;
                    writeln!(out, "{}", palette.bold("Reports"))?;
                    write_children(out, &report_forest(root, bindings), "")?;
                }
                if !kept.is_empty() {
                    writeln!(out)?;
                    writeln!(out, "{}", palette.bold("Kept"))?;
                    write_children(out, &kept_nodes(root, kept), "")?;
                }
                if !queried.is_empty() {
                    writeln!(out)?;
                    writeln!(out, "{}", palette.bold("Queried"))?;
                    write_children(out, &queried_nodes(root, queried), "")?;
                }
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

/// The kept objects of one root's impact slice, one leaf each:
/// `kept: <reason>` — prefixed with the object when it is not the root, and
/// naming the kept table when the annotation sits on the table instead.
fn kept_nodes(root: &ObjectId, kept: &[(ObjectId, KeptObject)]) -> Vec<Node> {
    kept.iter()
        .map(|(id, by)| {
            let mut label = String::new();
            if id != root {
                label.push_str(&format!("{id}  "));
            }
            label.push_str(&kept_phrase(id, by));
            Node::leaf(label)
        })
        .collect()
}

/// The queried objects of one root's impact slice, one leaf each — prefixed
/// with the object when it is not the root.
fn queried_nodes(root: &ObjectId, queried: &[QueriedObject]) -> Vec<Node> {
    queried
        .iter()
        .map(|by| {
            let mut label = String::new();
            if &by.id != root {
                label.push_str(&format!("{}  ", by.id));
            }
            label.push_str(&queried_phrase(by));
            Node::leaf(label)
        })
        .collect()
}

/// `queried 3× by 2 users · last 2026-09-30 · PowerBI · 1 report`: how often,
/// by how many people (never who), when last, from which clients and how
/// many distinct reports. Absent facts are left out.
fn queried_phrase(by: &QueriedObject) -> String {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut phrase = format!("queried {}×", by.count);
    if by.users > 0 {
        phrase.push_str(&format!(" by {}", plural(by.users, "user", "users")));
    }
    if let Some(last) = &by.last_seen {
        let day = last.split([' ', 'T']).next().unwrap_or(last);
        phrase.push_str(&format!(" · last {day}"));
    }
    if !by.applications.is_empty() {
        phrase.push_str(&format!(" · {}", by.applications.join(", ")));
    }
    if !by.reports.is_empty() {
        phrase.push_str(&format!(
            " · {}",
            plural(by.reports.len(), "report", "reports")
        ));
    }
    phrase
}

/// `kept: <reason>`, or `kept by table 'T': <reason>` for a member a kept
/// table carries; an empty reason reads `kept (no reason given)`.
fn kept_phrase(id: &ObjectId, by: &KeptObject) -> String {
    let mut phrase = String::from("kept");
    if &by.id != id {
        phrase.push_str(&format!(" by {}", by.id));
    }
    if by.reason.is_empty() {
        phrase.push_str(" (no reason given)");
    } else {
        phrase.push_str(&format!(": {}", by.reason));
    }
    phrase
}

/// The report bindings of one root's impact slice as deterministic tries:
/// `report → page → visual → binding site`, bookmarks as their own level,
/// mobile layouts marked. When every binding lands on the selected object
/// the object level is left out — the header already names it; when several
/// objects carry bindings, each gets its own labeled subtree.
fn report_forest(root: &ObjectId, bindings: &[(ObjectId, BindingEdge)]) -> Vec<Node> {
    if bindings.is_empty() {
        return Vec::new();
    }
    if bindings.iter().all(|(id, _)| id == root) {
        let edges: Vec<&BindingEdge> = bindings.iter().map(|(_, edge)| edge).collect();
        return binding_trie(&edges);
    }
    // Grouped by the object each binding lands on, in slice (identity) order.
    let mut by_target: Vec<(ObjectId, Vec<&BindingEdge>)> = Vec::new();
    for (id, edge) in bindings {
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
            if let Some(slices) = &focused.dependencies {
                // Graph orientation: the consumer, what it uses, and why.
                // Roots can share reached edges; one record per edge.
                let mut seen: HashSet<(&ObjectId, &ObjectId, &'static str)> = HashSet::new();
                for slice in slices {
                    for edge in &slice.edges {
                        let key = (&edge.from, &edge.to, edge.provenance.key());
                        if seen.insert(key) {
                            writeln!(
                                out,
                                "dependency\t{}\t{}\t{}",
                                edge.from,
                                edge.to,
                                edge.provenance.key()
                            )?;
                        }
                    }
                }
            }
            if let Some(slices) = &focused.impact {
                // Flipped: the queried-object side first, then what uses it.
                let mut seen: HashSet<(&ObjectId, &ObjectId, &'static str)> = HashSet::new();
                for slice in slices {
                    for edge in &slice.edges {
                        let key = (&edge.from, &edge.to, edge.provenance.key());
                        if seen.insert(key) {
                            writeln!(
                                out,
                                "impact\t{}\t{}\t{}",
                                edge.to,
                                edge.from,
                                edge.provenance.key()
                            )?;
                        }
                    }
                }
                let mut seen: HashSet<&ObjectId> = HashSet::new();
                for (id, by) in focused.kept.iter().flatten() {
                    if seen.insert(id) {
                        // The reason is free text: one record per line, so
                        // tabs and line breaks inside it become spaces.
                        let reason = by.reason.replace(['\t', '\n', '\r'], " ");
                        writeln!(out, "kept\t{}\t{}\t{reason}", id, by.id)?;
                    }
                }
                let mut seen: HashSet<&ObjectId> = HashSet::new();
                for by in focused.queried.iter().flatten() {
                    if seen.insert(&by.id) {
                        let applications = if by.applications.is_empty() {
                            "-".to_string()
                        } else {
                            by.applications.join(",").replace(['\t', '\n', '\r'], " ")
                        };
                        writeln!(
                            out,
                            "queried\t{}\t{}\t{}\t{}\t{applications}",
                            by.id,
                            by.count,
                            by.last_seen.as_deref().unwrap_or("-"),
                            by.users,
                        )?;
                    }
                }
                for bindings in &focused.bindings {
                    for (id, edge) in bindings {
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
