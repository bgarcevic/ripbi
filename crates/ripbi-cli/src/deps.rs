//! The `deps` orchestration: resolve a target the way `scan` does, ingest
//! through `ripbi-core`, resolve the object operand, project the graph
//! slices, and hand them to `deps::render`. Inspection only — nothing is
//! pruned, persisted, or gated on findings, so the exit code is 0 for every
//! produced view (including an empty one) and 2 for errors.

use std::path::Path;

use ripbi_core::graph::{BindingEdge, DependencyGraph};
use ripbi_core::ingest;
use ripbi_core::lookup::ReferenceError;
use ripbi_core::{DepSlice, ObjectId, Provenance, ReportModel};

use crate::cli::DepsArgs;
use crate::config;
use crate::deps::render::{DepsOutput, FocusedOut, ImpactOut, OverviewOut, Section};
use crate::deps::tree::{Node, Orientation, TreeBuilder};
use crate::error::ScanError;
use crate::render::kind_of;
use crate::scan;
use crate::style::Palette;

mod render;
mod tree;

/// Exit code: the requested view was produced, empty or not — inspection has
/// no findings code.
pub const EXIT_OK: i32 = 0;
/// Exit code: usage, discovery, lookup, or analysis error.
pub const EXIT_ERROR: i32 = 2;

/// Runs `deps` against the process's working directory.
#[must_use = "the return value is the process exit code"]
pub fn run(args: &DepsArgs, streams: &mut crate::scan::Streams<'_>) -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let _ = writeln!(
                streams.err,
                "error: cannot determine the working directory: {error}"
            );
            return EXIT_ERROR;
        }
    };
    run_in(args, &cwd, streams)
}

/// Runs `deps` against an explicit working directory (tests pass one).
#[must_use = "the return value is the process exit code"]
pub fn run_in(args: &DepsArgs, cwd: &Path, streams: &mut crate::scan::Streams<'_>) -> i32 {
    let palette_err = Palette::detect(streams.stderr_is_tty, args.no_color);
    match explore(args, cwd, streams, &palette_err) {
        Ok(code) => code,
        Err(error) => {
            if !args.quiet {
                let _ = writeln!(
                    streams.err,
                    "{} {}",
                    palette_err.red("error:"),
                    error.message
                );
                if let Some(hint) = &error.hint {
                    let _ = writeln!(streams.err, "hint: {hint}");
                }
            }
            EXIT_ERROR
        }
    }
}

fn explore(
    args: &DepsArgs,
    cwd: &Path,
    streams: &mut crate::scan::Streams<'_>,
    palette_err: &Palette,
) -> Result<i32, ScanError> {
    let palette_out = Palette::detect(streams.stdout_is_tty, args.no_color);
    let loaded = config::find_in(cwd)?;
    let config = loaded.map(|loaded| loaded.config);

    // Report roots: --report flags replace the config's `reports` — the same
    // inputs-are-not-filters rule as scan.
    let extras: Vec<std::path::PathBuf> = if args.reports.is_empty() {
        config
            .as_ref()
            .map(|config| config.reports.clone())
            .unwrap_or_default()
    } else {
        args.reports.clone()
    };
    let depth = parse_depth(args.depth.as_deref())?;

    // Target ladder: --model, else config `target`, else cwd discovery.
    // Unlike scan there is no zero-connected-report refusal: model-only
    // exploration is valid, and the intra-model graph stays fully explorable.
    let (paired, walk, mut announce) = if let Some(model_path) = &args.model {
        let (paired, walk) = scan::resolve_model_mode(model_path, &extras)?;
        (paired, Some(walk), Vec::new())
    } else {
        match config.as_ref().and_then(|config| config.target.clone()) {
            Some(path) => {
                let model_named = scan::names_semantic_model(&path);
                let paired = scan::resolve_explicit(&path)?;
                let (paired, walk) = scan::attach_extras(paired, &extras, model_named)?;
                (paired, walk, Vec::new())
            }
            None => {
                let (paired, announce) =
                    scan::discover_target(args.no_input, args.quiet, cwd, streams, palette_err)?;
                let (paired, walk) = scan::attach_extras(paired, &extras, false)?;
                (paired, walk, announce)
            }
        }
    };
    let report_paths = scan::dedupe(paired.reports);

    if !args.quiet {
        announce.push(if report_paths.is_empty() {
            format!("Exploring {} with no reports", paired.model.display())
        } else {
            format!(
                "Exploring {} with {} report(s)",
                paired.model.display(),
                report_paths.len()
            )
        });
        if let Some(walk) = &walk {
            if !walk.bound.name_matched.is_empty() {
                if args.verbose {
                    let names: Vec<String> = walk
                        .bound
                        .name_matched
                        .iter()
                        .map(|(path, _)| scan::report_name(path))
                        .collect();
                    announce.push(format!(
                        "Note: matched by dataset name only: {}",
                        names.join(", ")
                    ));
                } else {
                    announce.extend(scan::collapse_name_matched(&walk.bound.name_matched));
                }
            }
            if !walk.bound.ignored_elsewhere.is_empty() {
                let count = walk.bound.ignored_elsewhere.len();
                let names: Vec<String> = walk
                    .bound
                    .ignored_elsewhere
                    .iter()
                    .map(|path| scan::report_name(path))
                    .collect();
                announce.push(format!(
                    "Ignored {count} report(s) bound to other models: {}",
                    scan::capped_names(&names)
                ));
            }
        }
        for line in &announce {
            writeln!(streams.err, "{line}").map_err(ScanError::from)?;
        }
        writeln!(
            streams.err,
            "Note: analysis covers only the ingested reports; \
             external consumers (thin reports, Excel, XMLA) are invisible."
        )
        .map_err(ScanError::from)?;
    }

    // Ingest: the model, then every report as a binding source.
    let model = ingest::semantic_model(&paired.model).map_err(|error| {
        ScanError::new(format!("cannot ingest {}: {error}", paired.model.display()))
    })?;
    let mut skips: Vec<crate::render::SkipNoticeOut> =
        model.skips.iter().map(scan::skip_notice_out).collect();
    let mut reports = Vec::new();
    for path in &report_paths {
        let ingested = ingest::report(path).map_err(|error| {
            ScanError::new(format!("cannot ingest report {}: {error}", path.display()))
        })?;
        skips.extend(ingested.skips.iter().map(scan::skip_notice_out));
        let mut report = ingested.value;
        if report.name.is_none() {
            // No `.platform` display name: a user calls the report by its
            // item folder's name, and the binding provenance should say the
            // same thing the report tree will show.
            report.name = Some(item_name(path));
        }
        reports.push(report);
    }
    if let Some(walk) = &walk {
        skips.extend(walk.bound.unresolved.iter().map(|(path, detail)| {
            crate::render::SkipNoticeOut {
                path: path.display().to_string(),
                location: None,
                kind: "unresolved_dataset_reference",
                detail: detail.clone(),
            }
        }));
        skips.extend(
            walk.bound
                .malformed
                .iter()
                .map(|path| crate::render::SkipNoticeOut {
                    path: path.display().to_string(),
                    location: None,
                    kind: "malformed_report_item",
                    detail: "missing report.json".to_string(),
                }),
        );
        skips.extend(walk.bound.parse_skips.iter().map(scan::skip_notice_out));
    }

    // Analysis is core's job; this module only chooses slices and asks for
    // them by name.
    let report_refs: Vec<&ReportModel> = reports.iter().collect();
    let graph = DependencyGraph::build(&model.value, &report_refs);

    let output = match &args.object {
        Some(object) => focused_view(&graph, object, args, depth)?,
        None => overview_view(&graph),
    };

    if !args.quiet {
        render::human(streams.out, &palette_out, &output).map_err(ScanError::from)?;
        scan::write_skip_notices(streams.err, &skips)?;
    }
    Ok(EXIT_OK)
}

/// The default traversal is unrestricted; `--depth N` cuts the BFS at N
/// edges. Zero would make "direct neighbors only" mean "nothing", so it is a
/// usage error, not a silent empty view.
fn parse_depth(value: Option<&str>) -> Result<Option<usize>, ScanError> {
    match value {
        None | Some("all") => Ok(None),
        Some(other) => match other.parse::<usize>() {
            Ok(0) => Err(ScanError::new("--depth must be at least 1")
                .with_hint("use --depth 1 for direct neighbors, or --depth all for the leaves")),
            Ok(n) => Ok(Some(n)),
            Err(_) => Err(
                ScanError::new(format!("--depth {other} is not a number or 'all'"))
                    .with_hint("pass an edge count like --depth 2, or --depth all"),
            ),
        },
    }
}

/// The selected object's view: an upstream slice, a downstream slice, and the
/// report bindings riding on the downstream one.
fn focused_view(
    graph: &DependencyGraph,
    object: &str,
    args: &DepsArgs,
    depth: Option<usize>,
) -> Result<DepsOutput, ScanError> {
    let root = graph.resolve_reference(object).map_err(reference_error)?;
    let label = |id: &ObjectId| format!("{}  {}", id, kind_of(id));

    // Neither flag means both; either one alone narrows the view.
    let show_dependencies = args.dependencies || !args.impact;
    let show_impact = args.impact || !args.dependencies;

    let dependencies = if show_dependencies {
        let slice = graph.dependencies_of(&root, depth);
        let mut builder = TreeBuilder::new(Orientation::Dependencies);
        let tree = builder.build(&slice, &root, label);
        Some(Section {
            empty: tree.children.is_empty(),
            tree,
            suppressed: builder.suppressed(),
            reachable: slice.nodes.len(),
        })
    } else {
        None
    };

    let impact = if show_impact {
        let slice = graph.impact_of(&root, depth);
        let mut builder = TreeBuilder::new(Orientation::Impact);
        let tree = builder.build(&slice, &root, label);
        let model = Some(Section {
            empty: tree.children.is_empty(),
            tree,
            suppressed: builder.suppressed(),
            reachable: slice.nodes.len(),
        });
        let reports = report_forest(graph, &slice, &root);
        Some(ImpactOut { model, reports })
    } else {
        None
    };

    Ok(DepsOutput::Focused(FocusedOut {
        root: root.to_string(),
        kind: kind_of(&root),
        dependencies,
        impact,
    }))
}

/// The overview the bare command prints: counts, never the whole graph.
fn overview_view(graph: &DependencyGraph) -> DepsOutput {
    let mut measures = 0;
    let mut columns = 0;
    let mut hierarchies = 0;
    let mut relationships = 0;
    let mut other = 0;
    for id in graph.object_ids() {
        match kind_of(id) {
            "measure" => measures += 1,
            "column" => columns += 1,
            "hierarchy" => hierarchies += 1,
            "relationship" => relationships += 1,
            _ => other += 1,
        }
    }
    DepsOutput::Overview(OverviewOut {
        objects: graph.object_ids().count(),
        edges: graph.edge_count(),
        bindings: graph.roots().len(),
        by_type: vec![
            ("Measures", measures),
            ("Columns", columns),
            ("Hierarchies", hierarchies),
            ("Relationships", relationships),
            ("Other", other),
        ],
    })
}

/// The report item's display name: its folder name, `.Report` suffix dropped.
fn item_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let byte_count = name.len();
    if byte_count > ".Report".len() && name[byte_count - 7..].eq_ignore_ascii_case(".Report") {
        let stem = name[..byte_count - 7].to_string();
        if !stem.is_empty() {
            return stem;
        }
    }
    name
}

/// The report bindings of every object in the impact slice, as deterministic
/// tries: `report → page → visual → binding site`, bookmarks as their own
/// level, mobile layouts marked. When every binding lands on the selected
/// object the object level is left out — the header already names it; when
/// several objects carry bindings, each gets its own labeled subtree.
fn report_forest(graph: &DependencyGraph, slice: &DepSlice, root: &ObjectId) -> Vec<Node> {
    let mut bindings: Vec<(&ObjectId, &BindingEdge)> = Vec::new();
    for id in &slice.nodes {
        for provenance in graph.roots_of(id) {
            if let Provenance::Binding(edge) = provenance {
                bindings.push((id, edge));
            }
        }
    }
    if bindings.is_empty() {
        return Vec::new();
    }
    if bindings.iter().all(|(id, _)| *id == root) {
        let edges: Vec<&BindingEdge> = bindings.into_iter().map(|(_, edge)| edge).collect();
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
    let root = trie_to_node(trie);
    root.children
}

/// The leaf label of one binding: a field well renders its role (`Values`),
/// every other kind its site phrase, and a phone-layout binding says so.
fn site_label(edge: &BindingEdge) -> String {
    let mut label = match &edge.kind {
        ripbi_core::BindingSite::FieldWell { role } => role.clone(),
        other => other.to_string(),
    };
    if edge.mobile {
        label.push_str("  (mobile layout)");
    }
    label
}

#[derive(Default)]
struct Trie {
    children: std::collections::BTreeMap<String, Trie>,
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

/// Rewrites a lookup miss for a human: candidates listed with their kinds,
/// the nearest match offered on a typo.
fn reference_error(error: ReferenceError) -> ScanError {
    match error {
        ReferenceError::NotFound { input, suggestion } => {
            let mut message = format!("object \"{input}\" was not found");
            if let Some(suggestion) = suggestion {
                message.push_str("\n\nDid you mean:");
                message.push_str(&format!("\n  {}", *suggestion));
            }
            ScanError::new(message)
                .with_hint("object names are case-insensitive; quote the table: 'Table'[Name]")
        }
        ReferenceError::Ambiguous { input, candidates } => {
            let width = candidates
                .iter()
                .map(|id| kind_of(id).len())
                .max()
                .unwrap_or(0);
            let mut message = format!("\"{input}\" matches multiple objects\n");
            for id in &candidates {
                message.push_str(&format!(
                    "\n  {:width$}  {}",
                    kind_of(id),
                    id,
                    width = width
                ));
            }
            ScanError::new(message)
                .with_hint("use a fully-qualified object reference, e.g. 'Table'[Name]")
        }
    }
}
