//! The `deps` orchestration: resolve a target the way `scan` does, ingest
//! through `ripbi-core`, resolve the object operand, project the graph
//! slices, and hand them to `deps::render`. Inspection only — nothing is
//! pruned, persisted, or gated on findings, so the exit code is 0 for every
//! produced view (including an empty one) and 2 for errors.

use std::path::Path;

use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest;
use ripbi_core::lookup::ReferenceError;
use ripbi_core::{DepSlice, ObjectId, Provenance, ReportModel};

use crate::cli::DepsArgs;
use crate::config;
use crate::deps::render::{ByType, DepsOutput, FocusedOut, OverviewOut};
use crate::error::ScanError;
use crate::render::kind_of;
use crate::scan;
use crate::style::Palette;

mod graph_view;
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

    // Target: the shared ladder — --model, else PATH (none here: the
    // positional is an object operand, not a path), else config `target`,
    // else derivation from the --report anchors, else cwd discovery. Unlike
    // scan there is no zero-connected-report refusal: model-only exploration
    // is valid, and the intra-model graph stays fully explorable.
    let (paired, walk, mut announce) = scan::resolve_target(
        scan::TargetInput {
            model: args.model.as_deref(),
            path: None,
            config_target: config.as_ref().and_then(|config| config.target.as_deref()),
            extras: &extras,
            no_input: args.no_input,
            quiet: args.quiet,
            cwd,
        },
        streams,
        palette_err,
    )?;
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

    let output = if args.object.is_some() || args.table.is_some() || !args.types.is_empty() {
        selection_view(&graph, args, depth)?
    } else {
        overview_view(&graph)
    };

    if !args.quiet {
        if args.json {
            render::json(streams.out, &output)?;
        } else if args.graph {
            render::graph(streams.out, &palette_out, &output).map_err(ScanError::from)?;
        } else if args.plain {
            render::plain(streams.out, &output).map_err(ScanError::from)?;
        } else {
            render::human(streams.out, &palette_out, &output).map_err(ScanError::from)?;
        }
        // The deps JSON documents the graph, not the run, so skip notices are
        // stderr's job in every mode.
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

/// The selected objects' view: one root (an object operand) or many
/// (selectors), each with an upstream slice, a downstream slice, and the
/// report bindings riding on the downstream one. Traversal is always full;
/// the filters restrict which results are included.
fn selection_view(
    graph: &DependencyGraph,
    args: &DepsArgs,
    depth: Option<usize>,
) -> Result<DepsOutput, ScanError> {
    let roots = match &args.object {
        Some(object) => vec![graph.resolve_reference(object).map_err(reference_error)?],
        None => select_roots(graph, args)?,
    };

    // Neither flag means both; either one alone narrows the view.
    let show_dependencies = args.dependencies || !args.impact;
    let show_impact = args.impact || !args.dependencies;

    // Filters restrict the Impact result, so asking for one while impact is
    // switched off is a contradiction, not a silent no-op.
    let filtering = args.consumer.is_some() || args.in_report.is_some() || args.on_page.is_some();
    if filtering && !show_impact {
        return Err(ScanError::new(
            "--consumer, --in-report, and --on-page filter the Impact view",
        )
        .with_hint("drop --dependencies, or add --impact so there is something to filter"));
    }
    if let Some(kind) = args.consumer.as_deref()
        && kind != "visual"
        && !crate::render::KINDS.contains(&kind)
    {
        let mut message = format!("--consumer {kind} is not a consumer kind");
        message.push_str(&format!(
            "\n\nConsumer kinds:\n  visual\n  {}",
            crate::render::KINDS.join("\n  ")
        ));
        return Err(ScanError::new(message)
            .with_hint("'visual' selects report bindings; the rest select model objects"));
    }
    let consumer = args.consumer.as_deref();
    let in_report = args.in_report.as_deref().map(str::to_lowercase);
    let on_page = args.on_page.as_deref().map(str::to_lowercase);

    let dependencies = show_dependencies.then(|| {
        roots
            .iter()
            .map(|root| graph.dependencies_of(root, depth))
            .collect::<Vec<_>>()
    });
    let mut impact = None;
    let mut bindings = Vec::new();
    let mut model_hidden = false;
    if show_impact {
        let mut slices = Vec::new();
        for root in &roots {
            let slice = graph.impact_of(root, depth);
            // --consumer keeps the branches that lead to a consumer of the
            // named kind; 'visual' hands the whole view to report bindings.
            let slice = match consumer {
                None => slice,
                Some("visual") => {
                    model_hidden = true;
                    empty_slice(&slice)
                }
                Some(kind) => {
                    let kept = consumer_keep_set(&slice, kind);
                    prune_slice(&slice, &kept)
                }
            };
            let mut per_root = Vec::new();
            // Bindings are usages by visuals: they answer a `--consumer
            // visual` filter (where the model section steps aside) and are
            // otherwise shown unfiltered — but a model-kind consumer filter
            // asks for consumers of that kind, which a binding is not.
            if consumer.is_none_or(|kind| kind == "visual") {
                for id in &slice.nodes {
                    for provenance in graph.roots_of(id) {
                        if let Provenance::Binding(edge) = provenance {
                            per_root.push((id.clone(), (**edge).clone()));
                        }
                    }
                }
            }
            // --in-report / --on-page keep only bindings that belong to the
            // named report and page. An unnamed site can never match: a
            // filter that cannot check its claim must not pass it.
            per_root.retain(|(_, edge)| {
                in_report
                    .as_ref()
                    .is_none_or(|name| edge.report.as_ref().is_some_and(|r| r.folded() == name))
                    && on_page
                        .as_ref()
                        .is_none_or(|name| edge.page.as_ref().is_some_and(|p| p.folded() == name))
            });
            bindings.push(per_root);
            slices.push(slice);
        }
        impact = Some(slices);
    }

    Ok(DepsOutput::Focused(Box::new(FocusedOut {
        roots,
        dependencies,
        impact,
        bindings,
        model_hidden,
    })))
}

/// The consumers of the asked kind plus every node on a path leading to
/// one — pruning keeps a branch exactly when it ends at a match.
fn consumer_keep_set(slice: &DepSlice, kind: &str) -> std::collections::HashSet<ObjectId> {
    let mut kept: std::collections::HashSet<ObjectId> = slice
        .nodes
        .iter()
        .filter(|id| crate::render::kind_of(id) == kind)
        .cloned()
        .collect();
    loop {
        let mut grew = false;
        for id in &slice.nodes {
            if kept.contains(id) {
                continue;
            }
            if slice
                .consumers_of(id)
                .iter()
                .any(|(neighbor, _)| kept.contains(neighbor))
            {
                kept.insert(id.clone());
                grew = true;
            }
        }
        if !grew {
            return kept;
        }
    }
}

/// The slice restricted to the kept nodes: the root always stays, edges
/// survive when both endpoints do.
fn prune_slice(slice: &DepSlice, kept: &std::collections::HashSet<ObjectId>) -> DepSlice {
    let nodes: Vec<ObjectId> = slice
        .nodes
        .iter()
        .filter(|id| kept.contains(id))
        .cloned()
        .collect();
    let edges = slice
        .edges
        .iter()
        .filter(|edge| kept.contains(&edge.from) && kept.contains(&edge.to))
        .cloned()
        .collect();
    DepSlice {
        root: slice.root.clone(),
        depth: slice.depth,
        nodes,
        edges,
    }
}

/// A slice with nothing but its root — the shape of "no model usage".
fn empty_slice(slice: &DepSlice) -> DepSlice {
    DepSlice {
        root: slice.root.clone(),
        depth: slice.depth,
        nodes: vec![slice.root.clone()],
        edges: Vec::new(),
    }
}

/// The roots the `--table`/`--type` selectors choose. Selectors decide where
/// exploration starts — they never restrict what traversal may reach. Both
/// selectors together intersect: the members of the table that also have the
/// type.
fn select_roots(graph: &DependencyGraph, args: &DepsArgs) -> Result<Vec<ObjectId>, ScanError> {
    for kind in &args.types {
        if !crate::render::KINDS.contains(&kind.as_str()) {
            let mut message = format!("--type {kind} is not an object type");
            message.push_str(&format!(
                "\n\nObject types:\n  {}",
                crate::render::KINDS.join("\n  ")
            ));
            return Err(ScanError::new(message)
                .with_hint("run ripbi deps --help for what the selectors explore"));
        }
    }
    let tables: Vec<String> = args.table.iter().map(|name| name.to_lowercase()).collect();
    let roots: Vec<ObjectId> = graph
        .object_ids()
        .filter(|id| {
            let table_match = tables.is_empty()
                || id
                    .owning_table()
                    .is_some_and(|table| tables.iter().any(|t| t == table.folded()));
            let kind_match =
                args.types.is_empty() || args.types.iter().any(|kind| kind_of(id) == kind.as_str());
            table_match && kind_match
        })
        .cloned()
        .collect();
    if roots.is_empty() {
        let selected = args
            .table
            .as_ref()
            .map(|table| format!("table {table}"))
            .into_iter()
            .chain(args.types.iter().map(|kind| format!("type {kind}")))
            .collect::<Vec<_>>()
            .join(" and ");
        return Err(
            ScanError::new(format!("no objects match the {selected}")).with_hint(
                "selectors choose where exploration starts — the names come from the model",
            ),
        );
    }
    let mut roots = roots;
    roots.sort();
    Ok(roots)
}

/// The overview the bare command prints: counts, never the whole graph.
fn overview_view(graph: &DependencyGraph) -> DepsOutput {
    let mut by_type = ByType::default();
    for id in graph.object_ids() {
        match kind_of(id) {
            "measure" => by_type.measures += 1,
            "column" => by_type.columns += 1,
            "hierarchy" => by_type.hierarchies += 1,
            "relationship" => by_type.relationships += 1,
            _ => by_type.other += 1,
        }
    }
    DepsOutput::Overview(OverviewOut {
        objects: graph.object_ids().count(),
        edges: graph.edge_count(),
        bindings: graph.roots().len(),
        by_type,
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
