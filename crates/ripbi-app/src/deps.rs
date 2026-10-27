//! The `deps` exploration: resolve a target the way `scan` does, ingest
//! through `ripbi-core`, resolve the object operand or selectors, and project
//! the graph slices a front end renders. Inspection only — nothing is pruned,
//! persisted, or gated on.

pub mod json;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest;
use ripbi_core::lookup::ReferenceError;
use ripbi_core::{
    BindingEdge, DepSlice, KeptObject, ObjectId, Provenance, QueriedObject, ReportModel,
};

use crate::config;
use crate::error::ScanError;
use crate::output::{KINDS, SkipNoticeOut, kind_of};
use crate::scan::{
    Observer, Stage, Target, TargetInput, dedupe, load_query_log, query_log_item, query_log_path,
    resolve_target, skip_notice_out,
};

/// Everything one exploration is asked to do. `Default` is the overview of
/// the project cwd discovery finds.
#[derive(Debug, Clone, Default)]
pub struct DepsRequest {
    /// The working directory: discovery, `ripbi.toml`, and relative paths
    /// resolve against it.
    pub cwd: PathBuf,
    /// The object operand, e.g. `'Sales'[Total]`.
    pub object: Option<String>,
    /// `--model`.
    pub model: Option<PathBuf>,
    /// `--report` anchors; replace the config's `reports` when non-empty.
    pub reports: Vec<PathBuf>,
    /// `--queries-from`: a workspace-monitoring query log.
    pub queries_from: Option<PathBuf>,
    /// `--queries-item`: the model's name in a multi-model log.
    pub queries_item: Option<String>,
    /// `--dependencies`: the upstream view (neither flag means both).
    pub dependencies: bool,
    /// `--impact`: the downstream view (neither flag means both).
    pub impact: bool,
    /// `--depth`: an edge count or `all`, as written.
    pub depth: Option<String>,
    /// `--table`: start from this table's members.
    pub table: Option<String>,
    /// `--type`: start from objects of these kinds.
    pub types: Vec<String>,
    /// `--consumer`: keep impact branches ending at this kind (`visual` for
    /// report bindings).
    pub consumer: Option<String>,
    /// `--in-report`: keep bindings in this report.
    pub in_report: Option<String>,
    /// `--on-page`: keep bindings on this page.
    pub on_page: Option<String>,
    /// Never prompt; fail where the picker would appear.
    pub no_input: bool,
    /// Suppress the picker along with the output.
    pub quiet: bool,
}

/// What one exploration produced.
#[derive(Debug, Clone)]
pub struct DepsOutcome {
    /// The view.
    pub output: DepsOutput,
    /// Skip notices from ingestion and the bound-report walk. The JSON
    /// documents the graph, not the run, so these are the front end's to show.
    pub skips: Vec<SkipNoticeOut>,
    /// Whether a query log was read — it narrows which consumers stay unseen.
    pub query_logged: bool,
}

/// Runs one exploration. Observer calls, in order: `pick` (discovery),
/// `stage(Model)`, `resolved`, `stage(Reports)`, query-log notes,
/// `stage(Graph)`, `stage(Findings)`.
///
/// # Errors
/// Config, discovery, lookup, and ingestion failures, and contradictory or
/// unknown selectors.
pub fn run(request: &DepsRequest, observer: &mut dyn Observer) -> Result<DepsOutcome, ScanError> {
    let cwd = request.cwd.as_path();
    let loaded = config::find_in(cwd)?;
    let config = loaded.map(|loaded| loaded.config);

    // Report roots: --report flags replace the config's `reports` — the same
    // inputs-are-not-filters rule as scan.
    let extras: Vec<PathBuf> = if request.reports.is_empty() {
        config
            .as_ref()
            .map(|config| config.reports.clone())
            .unwrap_or_default()
    } else {
        request.reports.clone()
    };
    let depth = parse_depth(request.depth.as_deref())?;
    let queries_path = query_log_path(request.queries_from.as_deref(), config.as_ref());
    let query_logged = queries_path.is_some();

    // Target: the shared ladder — --model, else PATH (none here: the
    // positional is an object operand, not a path), else config `target`,
    // else derivation from the --report anchors, else cwd discovery. Unlike
    // scan there is no zero-connected-report refusal: model-only exploration
    // is valid, and the intra-model graph stays fully explorable.
    let resolved = resolve_target(
        TargetInput {
            model: request.model.as_deref(),
            path: None,
            config_target: config.as_ref().and_then(|config| config.target.as_deref()),
            extras: &extras,
            no_input: request.no_input,
            quiet: request.quiet,
            cwd,
        },
        observer,
    )?;
    observer.stage(Stage::Model);
    let paired = resolved.paired;
    let walk = resolved.walk;
    let report_paths = dedupe(paired.reports);
    observer.resolved(&Target {
        model: &paired.model,
        reports: &report_paths,
        bound: walk.as_ref().map(|walk| &walk.bound),
        discovered: resolved.discovered.as_ref(),
    })?;

    // Ingest: the model, then every report as a binding source.
    let model = ingest::semantic_model(&paired.model).map_err(|error| {
        ScanError::new(format!("cannot ingest {}: {error}", paired.model.display()))
    })?;
    observer.stage(Stage::Reports);
    let mut skips: Vec<SkipNoticeOut> = model.skips.iter().map(skip_notice_out).collect();
    let mut reports = Vec::new();
    for path in &report_paths {
        let ingested = ingest::report(path).map_err(|error| {
            ScanError::new(format!("cannot ingest report {}: {error}", path.display()))
        })?;
        skips.extend(ingested.skips.iter().map(skip_notice_out));
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
        skips.extend(
            walk.bound
                .unresolved
                .iter()
                .map(|(path, detail)| SkipNoticeOut {
                    path: path.display().to_string(),
                    location: None,
                    kind: "unresolved_dataset_reference",
                    detail: detail.clone(),
                }),
        );
        skips.extend(walk.bound.malformed.iter().map(|path| SkipNoticeOut {
            path: path.display().to_string(),
            location: None,
            kind: "malformed_report_item",
            detail: "missing report.json".to_string(),
        }));
        skips.extend(walk.bound.parse_skips.iter().map(skip_notice_out));
    }

    let query_log = load_query_log(
        queries_path,
        query_log_item(request.queries_item.as_deref(), config.as_ref()),
        &paired.model,
        observer,
        &mut skips,
    )?;

    // Analysis is core's job; this module only chooses slices and asks for
    // them by name.
    observer.stage(Stage::Graph);
    let report_refs: Vec<&ReportModel> = reports.iter().collect();
    let graph = DependencyGraph::build_with_queries(&model.value, &report_refs, &query_log);
    observer.stage(Stage::Findings);

    let output = if request.object.is_some() || request.table.is_some() || !request.types.is_empty()
    {
        selection_view(&graph, request, depth)?
    } else {
        overview_view(&graph)
    };
    Ok(DepsOutcome {
        output,
        skips,
        query_logged,
    })
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
    request: &DepsRequest,
    depth: Option<usize>,
) -> Result<DepsOutput, ScanError> {
    let roots = match &request.object {
        Some(object) => vec![graph.resolve_reference(object).map_err(reference_error)?],
        None => select_roots(graph, request)?,
    };

    // Neither flag means both; either one alone narrows the view.
    let show_dependencies = request.dependencies || !request.impact;
    let show_impact = request.impact || !request.dependencies;

    // Filters restrict the Impact result, so asking for one while impact is
    // switched off is a contradiction, not a silent no-op.
    let filtering =
        request.consumer.is_some() || request.in_report.is_some() || request.on_page.is_some();
    if filtering && !show_impact {
        return Err(ScanError::new(
            "--consumer, --in-report, and --on-page filter the Impact view",
        )
        .with_hint("drop --dependencies, or add --impact so there is something to filter"));
    }
    if let Some(kind) = request.consumer.as_deref()
        && kind != "visual"
        && !KINDS.contains(&kind)
    {
        let mut message = format!("--consumer {kind} is not a consumer kind");
        message.push_str(&format!(
            "\n\nConsumer kinds:\n  visual\n  {}",
            KINDS.join("\n  ")
        ));
        return Err(ScanError::new(message)
            .with_hint("'visual' selects report bindings; the rest select model objects"));
    }
    let consumer = request.consumer.as_deref();
    let in_report = request.in_report.as_deref().map(str::to_lowercase);
    let on_page = request.on_page.as_deref().map(str::to_lowercase);

    let dependencies = show_dependencies.then(|| {
        roots
            .iter()
            .map(|root| graph.dependencies_of(root, depth))
            .collect::<Vec<_>>()
    });
    let mut impact = None;
    let mut bindings = Vec::new();
    let mut kept = Vec::new();
    let mut queried = Vec::new();
    // A `ripbi_keep` annotation stands in for a consumer ripbi cannot see
    // (issue #151): not a visual, not a model object, in no report — so any
    // consumer or report filter leaves it out.
    let show_kept = !filtering;
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
            kept.push(if show_kept {
                slice
                    .nodes
                    .iter()
                    .filter_map(|id| graph.kept_by(id).map(|by| (id.clone(), by.clone())))
                    .collect::<Vec<(ObjectId, KeptObject)>>()
            } else {
                Vec::new()
            });
            // Logged queries are consumers outside every report and model
            // object, so the same filters leave them out.
            queried.push(if show_kept {
                slice
                    .nodes
                    .iter()
                    .filter_map(|id| graph.queried_by(id).cloned())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            });
            slices.push(slice);
        }
        impact = Some(slices);
    }

    Ok(DepsOutput::Focused(Box::new(FocusedOut {
        roots,
        dependencies,
        impact,
        bindings,
        kept,
        queried,
        model_hidden,
    })))
}

/// The consumers of the asked kind plus every node on a path leading to
/// one — pruning keeps a branch exactly when it ends at a match.
fn consumer_keep_set(slice: &DepSlice, kind: &str) -> HashSet<ObjectId> {
    let mut kept: HashSet<ObjectId> = slice
        .nodes
        .iter()
        .filter(|id| kind_of(id) == kind)
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
fn prune_slice(slice: &DepSlice, kept: &HashSet<ObjectId>) -> DepSlice {
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
fn select_roots(
    graph: &DependencyGraph,
    request: &DepsRequest,
) -> Result<Vec<ObjectId>, ScanError> {
    for kind in &request.types {
        if !KINDS.contains(&kind.as_str()) {
            let mut message = format!("--type {kind} is not an object type");
            message.push_str(&format!("\n\nObject types:\n  {}", KINDS.join("\n  ")));
            return Err(ScanError::new(message)
                .with_hint("run ripbi deps --help for what the selectors explore"));
        }
    }
    let tables: Vec<String> = request
        .table
        .iter()
        .map(|name| name.to_lowercase())
        .collect();
    let roots: Vec<ObjectId> = graph
        .object_ids()
        .filter(|id| {
            let table_match = tables.is_empty()
                || id
                    .owning_table()
                    .is_some_and(|table| tables.iter().any(|t| t == table.folded()));
            let kind_match = request.types.is_empty()
                || request
                    .types
                    .iter()
                    .any(|kind| kind_of(id) == kind.as_str());
            table_match && kind_match
        })
        .cloned()
        .collect();
    if roots.is_empty() {
        let selected = request
            .table
            .as_ref()
            .map(|table| format!("table {table}"))
            .into_iter()
            .chain(request.types.iter().map(|kind| format!("type {kind}")))
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

/// What `deps` prints: the whole-graph overview, or one object's view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepsOutput {
    /// The compact overview printed with no object and no selectors — never
    /// the whole graph.
    Overview(OverviewOut),
    /// One object's dependencies and/or impact, as the traversed slices.
    /// Boxed: the slices dwarf the overview's counts.
    Focused(Box<FocusedOut>),
}

/// The overview's counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewOut {
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
pub struct ByType {
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
    pub fn rows(&self) -> [(&'static str, usize); 5] {
        [
            ("Measures", self.measures),
            ("Columns", self.columns),
            ("Hierarchies", self.hierarchies),
            ("Relationships", self.relationships),
            ("Other", self.other),
        ]
    }
}

/// The selected objects' view: the traversed slices, not yet rendered. The
/// human trees are projections built at render time; `--plain` and `--json`
/// read the slices directly and never truncate. One root for an object
/// operand; one per selector match for `--table`/`--type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedOut {
    /// The selected objects, sorted by identity.
    pub roots: Vec<ObjectId>,
    /// One upstream slice per root; `None` when only `--impact` was asked
    /// for.
    pub dependencies: Option<Vec<DepSlice>>,
    /// One downstream slice per root; `None` when only `--dependencies` was
    /// asked for.
    pub impact: Option<Vec<DepSlice>>,
    /// The report bindings riding on each root's impact slice, as
    /// `(target object, binding)` pairs in slice order, one entry per root.
    pub bindings: Vec<Vec<(ObjectId, BindingEdge)>>,
    /// The objects on each root's impact slice that a `ripbi_keep` model
    /// annotation keeps (issue #151), with the annotation keeping them — the
    /// object's own, or its kept table's. One entry per root, in slice order.
    pub kept: Vec<Vec<(ObjectId, KeptObject)>>,
    /// The objects on each root's impact slice that `--queries-from` logged
    /// queries reference directly. One entry per root, in slice order.
    pub queried: Vec<Vec<QueriedObject>>,
    /// True for a `--consumer visual` run: the Impact view belongs to the
    /// report bindings, so the Model section stays out entirely.
    pub model_hidden: bool,
}

impl DepsOutput {
    /// Whether the view includes an Impact section — the one whose meaning
    /// depends on consumers ripbi cannot see.
    pub fn shows_impact(&self) -> bool {
        matches!(self, Self::Focused(focused) if focused.impact.is_some())
    }
}
