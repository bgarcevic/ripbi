//! The `report` inventory (issue #33): resolve a target exactly the way
//! `scan` does, ingest the paired model and reports, and list what the
//! reports bind — pages → visuals → fields — as data. The sanity check for
//! "what did ripbi actually see in this report", which is the question to ask
//! when a scan result looks wrong. Informational by design: nothing here
//! gates, so `scan`'s findings contract stays untouched.

pub mod json;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ripbi_core::NameKey;
use ripbi_core::graph::{BindingSite, BrokenBinding, BrokenReason, DependencyGraph};
use ripbi_core::ingest::{self, SkipKind};
use ripbi_core::report::{FieldTarget, Filter, Page, ReportModel, Visual};

use crate::config;
use crate::discover;
use crate::error::ScanError;
use crate::glob;
use crate::output::SkipNoticeOut;
use crate::scan::{
    Note, Observer, Stage, Target, TargetInput, dedupe, resolve_target, skip_notice_out,
};

/// Everything one inventory is asked to do. `Default` inventories the
/// project cwd discovery finds.
#[derive(Debug, Clone, Default)]
pub struct ReportRequest {
    /// The working directory: discovery, `ripbi.toml`, and relative paths
    /// resolve against it.
    pub cwd: PathBuf,
    /// The positional PATH.
    pub path: Option<PathBuf>,
    /// `--model`: pair the reports with this model.
    pub model: Option<PathBuf>,
    /// `--report` anchors; replace the config's `reports` when non-empty.
    pub reports: Vec<PathBuf>,
    /// Never prompt; fail where the picker would appear.
    pub no_input: bool,
    /// Suppress the picker along with the output.
    pub quiet: bool,
    /// `--allow-no-model`: list the reports as written when no model pairs.
    pub allow_no_model: bool,
    /// `--used`: list every model object reachability keeps alive.
    pub used: bool,
    /// `--broken`: keep only the unresolved rows.
    pub broken: bool,
    /// `--page` globs (page name or display name).
    pub page: Vec<String>,
    /// `--visual` globs (visual name or type).
    pub visual: Vec<String>,
    /// `--site` globs over binding sites.
    pub site: Vec<String>,
    /// `--kind` globs over row kinds.
    pub kind: Vec<String>,
    /// `--match` globs over row targets.
    pub matches: Vec<String>,
}

/// What one inventory produced.
#[derive(Debug, Clone)]
pub struct ReportOutcome {
    /// The inventory, narrowed by the request's filters.
    pub inventory: Inventory,
    /// Whether a model paired; without one nothing was resolved.
    pub paired: bool,
}

/// Runs one inventory. Observer calls, in order: `pick` (discovery),
/// `stage(Model)`, then `resolved` (a model paired) or `unpaired` plus a
/// warning note (`allow_no_model`), then `stage(Reports)`, `stage(Graph)`,
/// `stage(Findings)`.
///
/// # Errors
/// Config, discovery, and ingestion failures; `used`/`broken` without a
/// model; nothing to inventory.
pub fn run(
    request: &ReportRequest,
    observer: &mut dyn Observer,
) -> Result<ReportOutcome, ScanError> {
    let cwd = request.cwd.as_path();
    let loaded = config::find_in(cwd)?;
    let config = loaded.map(|loaded| loaded.config);

    // Report anchors: --report flags replace the config's `reports` — the
    // same inputs-are-not-filters rule as scan and deps. `[scan].ignore`
    // stays scan-specific and deliberately unread here.
    let extras: Vec<PathBuf> = if request.reports.is_empty() {
        config
            .as_ref()
            .map(|config| config.reports.clone())
            .unwrap_or_default()
    } else {
        request.reports.clone()
    };

    // Target: the shared ladder — with one report-side escape hatch. When no
    // model pairs and `allow_no_model` says so, the named reports are listed
    // as written, with nothing to resolve them against; every other failure
    // (a mistyped path, an unreadable model) stays an error.
    let resolved = resolve_target(
        TargetInput {
            model: request.model.as_deref(),
            path: request.path.as_deref(),
            config_target: config.as_ref().and_then(|config| config.target.as_deref()),
            extras: &extras,
            no_input: request.no_input,
            quiet: request.quiet,
            cwd,
        },
        observer,
    );
    observer.stage(Stage::Model);
    let (model_item, report_paths) = match resolved {
        Ok(resolved) => {
            let report_paths = dedupe(resolved.paired.reports);
            if report_paths.is_empty() {
                return Err(ScanError::new(format!(
                    "nothing to inventory: {} has no reports",
                    resolved.paired.model.display()
                ))
                .with_hint("pass a .pbip project, a .Report folder, or a project folder"));
            }
            observer.resolved(&Target {
                model: &resolved.paired.model,
                reports: &report_paths,
                bound: resolved.walk.as_ref().map(|walk| &walk.bound),
                discovered: resolved.discovered.as_ref(),
            })?;
            (Some(resolved.paired.model), report_paths)
        }
        Err(error) if request.allow_no_model && error.is_no_model() => {
            let explicit = request
                .path
                .as_deref()
                .or_else(|| config.as_ref().and_then(|config| config.target.as_deref()));
            let mut items = Vec::new();
            match explicit {
                Some(path) => items.extend(discover::report_items_without_model(path)?),
                None => {
                    for extra in &extras {
                        items.extend(discover::report_items_without_model(extra)?);
                    }
                }
            }
            let report_paths = dedupe(items);
            if report_paths.is_empty() {
                return Err(
                    ScanError::new("nothing to inventory: no report items to list")
                        .with_hint("pass a .Report folder or a .pbip project"),
                );
            }
            // The resolution views need the model: without one, nothing can
            // be resolved against anything, so nothing may claim to be used
            // or broken. Refused before anything is announced.
            if request.used || request.broken {
                let flags: Vec<&str> = [("--used", request.used), ("--broken", request.broken)]
                    .into_iter()
                    .filter(|(_, on)| *on)
                    .map(|(flag, _)| flag)
                    .collect();
                let verb = if flags.len() == 1 { "needs" } else { "need" };
                return Err(ScanError::new(format!(
                    "{} {verb} a paired semantic model",
                    flags.join(" and ")
                ))
                .with_hint(
                    "pair a model (a .SemanticModel beside the report, PATH, or --model), \
                     or drop the flag",
                ));
            }
            observer.unpaired(&report_paths)?;
            observer.note(Note::warn(
                "no semantic model paired; references are listed as written, \
                 not checked against a model",
            ))?;
            (None, report_paths)
        }
        Err(error) => return Err(error),
    };

    // Ingest the reports; the model, when there is one, turns the written
    // references into resolutions — the same pipeline `scan` runs, because
    // "unresolved" here means exactly what `scan`'s broken bindings mean: a
    // written reference that names nothing in the model.
    let model = match &model_item {
        Some(model_item) => Some(ingest::semantic_model(model_item).map_err(|error| {
            ScanError::new(format!("cannot ingest {}: {error}", model_item.display()))
        })?),
        None => None,
    };
    observer.stage(Stage::Reports);
    let mut skips: Vec<SkipNoticeOut> = Vec::new();
    if let Some(ingested) = &model {
        skips.extend(ingested.skips.iter().map(skip_notice_out));
    }
    let mut reports = Vec::new();
    for path in &report_paths {
        let ingested = ingest::report(path).map_err(|error| {
            ScanError::new(format!("cannot ingest report {}: {error}", path.display()))
        })?;
        skips.extend(ingested.skips.iter().map(skip_notice_out));
        reports.push(ingested.value);
    }

    observer.stage(Stage::Graph);
    let (graph, hides_a_name) = match &model {
        Some(ingested) => {
            // Issue #60's precision bar, shared with `scan`: model-side
            // `unknown_object` skips can hide the very name a binding wrote,
            // so no unresolved claim is made while any are present.
            let hides_a_name = ingested
                .skips
                .iter()
                .any(|skip| matches!(skip.kind, SkipKind::UnknownObject));
            let report_refs: Vec<&ReportModel> = reports.iter().collect();
            (
                Some(DependencyGraph::build(&ingested.value, &report_refs)),
                hides_a_name,
            )
        }
        None => (None, false),
    };

    observer.stage(Stage::Findings);
    let mut nodes = Vec::new();
    for (path, report) in report_paths.iter().zip(&reports) {
        // The report's broken *live* bindings. Attribution joins on the
        // report name the edge recorded — name, not identity, so two
        // same-named reports share attribution; benign for an informational
        // listing. Bookmark-saved state is out of scope for v1 (bookmarks
        // are a count line), so bindings a bookmark carries are skipped.
        // Model-less there is nothing to resolve against: no claim is made.
        let broken: Vec<&BrokenBinding> = match &graph {
            Some(graph) if !hides_a_name => graph
                .broken_bindings()
                .iter()
                .filter(|binding| binding.edge.bookmark.is_none())
                .filter(|binding| names_match(&binding.edge.report, report.name.as_deref()))
                .collect(),
            _ => Vec::new(),
        };
        nodes.push(report_node(path, report, &broken));
    }
    let mut output = Inventory {
        target: model_item
            .as_deref()
            .map(|model| model.display().to_string())
            .unwrap_or_else(|| NO_MODEL_TARGET.to_string()),
        reports: nodes,
        skips,
        used: Vec::new(),
    };
    apply_filters(&mut output, request);
    if request.used {
        let graph = graph.as_ref().expect("--used is refused without a model");
        output.used = used_rows(graph, request);
    }

    Ok(ReportOutcome {
        inventory: output,
        paired: model_item.is_some(),
    })
}

/// The note to show when the request's filters matched nothing — silence
/// reads as breakage. Each requested kind is judged on its own survivors:
/// `page` on pages, `visual` on visuals, the row filters on bindings.
#[must_use]
pub fn empty_note(request: &ReportRequest, output: &Inventory) -> Option<String> {
    let rows_requested = !request.matches.is_empty()
        || !request.kind.is_empty()
        || !request.site.is_empty()
        || request.broken;
    if !rows_requested && request.page.is_empty() && request.visual.is_empty() {
        return None;
    }
    let pages: usize = output.reports.iter().map(|report| report.pages.len()).sum();
    let visuals: usize = output
        .reports
        .iter()
        .map(|report| {
            report
                .pages
                .iter()
                .map(|page| page.visuals.len())
                .sum::<usize>()
        })
        .sum();
    let rows: usize = output
        .reports
        .iter()
        .map(|report| {
            report.unresolved_count
                + report.filters.len()
                + report
                    .pages
                    .iter()
                    .map(|page| {
                        page.filters.len()
                            + page
                                .visuals
                                .iter()
                                .map(|visual| visual.fields.len() + visual.filters.len())
                                .sum::<usize>()
                    })
                    .sum::<usize>()
        })
        .sum();
    if !request.page.is_empty() && pages == 0 {
        Some(format!("no pages match {}", request.page.join(", ")))
    } else if !request.visual.is_empty() && visuals == 0 {
        Some(format!("no visuals match {}", request.visual.join(", ")))
    } else if request.used && output.used.is_empty() {
        Some("no used objects match the given filters".to_string())
    } else if rows_requested && rows == 0 {
        Some("no bindings match the given filters".to_string())
    } else {
        None
    }
}

/// Every model object reachability keeps alive — the complement of scan's
/// unused findings, sorted by kind then id. `--kind` and `--match` narrow it
/// by the two columns the view shows; the other filters do not: reachability
/// always describes the whole report.
fn used_rows(graph: &DependencyGraph, request: &ReportRequest) -> Vec<UsedRow> {
    let unused: HashSet<ripbi_core::ObjectId> = graph
        .unused_objects()
        .into_iter()
        .map(|finding| finding.id)
        .collect();
    let mut rows: Vec<UsedRow> = graph
        .object_ids()
        .filter(|id| !unused.contains(*id))
        .map(|id| UsedRow {
            kind: crate::output::kind_of(id),
            id: id.to_string(),
        })
        .filter(|row| {
            (request.kind.is_empty()
                || request
                    .kind
                    .iter()
                    .any(|glob| glob::matches(glob, row.kind)))
                && (request.matches.is_empty()
                    || request
                        .matches
                        .iter()
                        .any(|glob| glob::matches(glob, &row.id)))
        })
        .collect();
    rows.sort_by(|a, b| a.kind.cmp(b.kind).then(a.id.cmp(&b.id)));
    rows
}

/// Narrows the inventory in place: `--page`/`--visual` select containers,
/// the row filters (`--match`/`--kind`/`--site`/`--broken`) select rows — a
/// visual or page with no surviving rows is pruned. Repeats of one flag
/// union; different flags intersect. Counts recompute from what survives, so
/// every number describes the visible tree. The binding-tree filters do not
/// shrink the `--used` rows — reachability always describes the whole report
/// — but `--kind`/`--match` filter those rows afterwards, in [`used_rows`].
fn apply_filters(inventory: &mut Inventory, request: &ReportRequest) {
    let target_globs: Vec<&str> = request.matches.iter().map(String::as_str).collect();
    let kind_globs: Vec<&str> = request.kind.iter().map(String::as_str).collect();
    let site_globs: Vec<&str> = request.site.iter().map(String::as_str).collect();
    // One predicate over a binding row — target, kind, and site each must
    // pass their flag's globs (absent flags pass everything). Filter blocks
    // present rows as kind `filter` (the declared target) and `filter_ref`
    // (condition-tree references), both at site `filter`; unresolved rows
    // carry kind `unresolved` and their real site.
    let row_selected = |target: &str, kind: &str, site: &str| {
        (target_globs.is_empty() || target_globs.iter().any(|glob| glob::matches(glob, target)))
            && (kind_globs.is_empty() || kind_globs.iter().any(|glob| glob::matches(glob, kind)))
            && (site_globs.is_empty() || site_globs.iter().any(|glob| glob::matches(glob, site)))
    };
    let prune_rows = !target_globs.is_empty()
        || !kind_globs.is_empty()
        || !site_globs.is_empty()
        || request.broken;
    let filtering = prune_rows || !request.page.is_empty() || !request.visual.is_empty();

    for report in &mut inventory.reports {
        if request.broken {
            report.filters.clear();
        } else if prune_rows {
            report
                .filters
                .retain(|filter| filter_matches(filter, &row_selected));
        }
        report.unresolved.retain(|unresolved| {
            row_selected(&unresolved.target, "unresolved", &unresolved.binding_site)
        });
        let mut pages = std::mem::take(&mut report.pages);
        if !request.page.is_empty() {
            pages.retain(|page| {
                request.page.iter().any(|pattern| {
                    glob::matches(pattern, &page.name)
                        || page
                            .display_name
                            .as_deref()
                            .is_some_and(|display| glob::matches(pattern, display))
                })
            });
        }
        for page in &mut pages {
            if request.broken {
                page.filters.clear();
            } else if prune_rows {
                page.filters
                    .retain(|filter| filter_matches(filter, &row_selected));
            }
            page.unresolved.retain(|unresolved| {
                row_selected(&unresolved.target, "unresolved", &unresolved.binding_site)
            });
            let mut visuals = std::mem::take(&mut page.visuals);
            if !request.visual.is_empty() {
                visuals.retain(|visual| {
                    request.visual.iter().any(|pattern| {
                        glob::matches(pattern, &visual.name)
                            || glob::matches(pattern, &visual.visual_type)
                    })
                });
            }
            for visual in &mut visuals {
                if request.broken {
                    visual.fields.clear();
                    visual.filters.clear();
                } else if prune_rows {
                    visual.fields.retain(|field| {
                        row_selected(&field.target, field.kind, &field.binding_site)
                    });
                    visual
                        .filters
                        .retain(|filter| filter_matches(filter, &row_selected));
                }
                visual.unresolved.retain(|unresolved| {
                    row_selected(&unresolved.target, "unresolved", &unresolved.binding_site)
                });
            }
            if prune_rows {
                visuals.retain(|visual| {
                    !visual.fields.is_empty()
                        || !visual.filters.is_empty()
                        || !visual.unresolved.is_empty()
                });
            }
            page.visuals = visuals;
        }
        if prune_rows {
            report.pages.retain(|page| {
                !page.visuals.is_empty() || !page.filters.is_empty() || !page.unresolved.is_empty()
            });
        }
        report.pages = pages;
        report.unresolved_count = report.unresolved.len()
            + report
                .pages
                .iter()
                .map(|page| {
                    page.unresolved.len()
                        + page
                            .visuals
                            .iter()
                            .map(|visual| visual.unresolved.len())
                            .sum::<usize>()
                })
                .sum::<usize>();
    }
    // A filtered run describes the surviving tree only: a report left with
    // nothing is dropped entirely — the stderr note names the empty result.
    if filtering {
        inventory.reports.retain(|report| {
            !report.pages.is_empty() || !report.filters.is_empty() || !report.unresolved.is_empty()
        });
    }
}

/// Whether a filter block survives a row selection: its declared target row
/// (kind `filter`) or any condition-tree reference row (kind `filter_ref`)
/// passes — the whole block stays, the same rule `--match` follows.
fn filter_matches(filter: &FilterNode, row_selected: &impl Fn(&str, &str, &str) -> bool) -> bool {
    filter
        .target
        .as_deref()
        .is_some_and(|target| row_selected(target, "filter", "filter"))
        || filter
            .references
            .iter()
            .any(|reference| row_selected(reference, "filter_ref", "filter"))
}

/// The JSON `target` value when no model paired: the inventory describes the
/// reports alone.
const NO_MODEL_TARGET: &str = "(no semantic model)";

/// The inventory of one ingested report.
fn report_node(path: &Path, report: &ReportModel, broken: &[&BrokenBinding]) -> ReportNode {
    let name = report.name.clone().unwrap_or_else(|| fallback_name(path));

    let mut pages = Vec::new();
    for page in &report.pages {
        pages.push(page_node(page, false, broken));
    }
    for page in &report.mobile_pages {
        pages.push(page_node(page, true, broken));
    }
    // Report-level: a broken binding on no page and no visual (a report
    // filter, a report-measure reference).
    let report_unresolved: Vec<UnresolvedNode> = broken
        .iter()
        .filter(|binding| binding.edge.page.is_none() && binding.edge.visual.is_none())
        .map(|binding| unresolved_node(binding))
        .collect();
    let unresolved_count = report_unresolved.len()
        + pages
            .iter()
            .map(|page| {
                page.unresolved.len()
                    + page
                        .visuals
                        .iter()
                        .map(|visual| visual.unresolved.len())
                        .sum::<usize>()
            })
            .sum::<usize>();

    ReportNode {
        path: path.display().to_string(),
        name,
        filters: report.filters.iter().map(filter_node).collect(),
        unresolved: report_unresolved,
        pages,
        bookmarks: report.bookmarks.len(),
        report_measures: report
            .measures
            .iter()
            .map(|measure| measure.name.as_str().to_string())
            .collect(),
        unresolved_count,
    }
}

/// One page, desktop or phone layout.
fn page_node(page: &Page, mobile: bool, broken: &[&BrokenBinding]) -> PageNode {
    // Page-level: on this page but in no visual — a page filter or a
    // drillthrough parameter.
    let unresolved: Vec<UnresolvedNode> = broken
        .iter()
        .filter(|binding| {
            binding.edge.mobile == mobile
                && binding.edge.page.as_ref() == Some(&page.name)
                && binding.edge.visual.is_none()
        })
        .map(|binding| unresolved_node(binding))
        .collect();
    PageNode {
        name: page.name.as_str().to_string(),
        display_name: page.display_name.clone(),
        hidden: page.is_hidden,
        mobile,
        filters: page.filters.iter().map(filter_node).collect(),
        unresolved,
        visuals: page
            .visuals
            .iter()
            .map(|visual| visual_node(visual, page, mobile, broken))
            .collect(),
    }
}

/// One visual: every model object it references, where each reference binds,
/// its filters, and the references that resolve to nothing.
fn visual_node(
    visual: &Visual,
    page: &Page,
    mobile: bool,
    broken: &[&BrokenBinding],
) -> VisualNode {
    let mut fields = Vec::new();
    for well in &visual.wells {
        for projection in &well.projections {
            fields.push(FieldNode {
                kind: kind_of(&projection.target),
                target: projection.target.to_string(),
                binding_site: format!("field_well:{}", well.role),
                active: Some(projection.active),
            });
        }
    }
    for (targets, site) in [
        (&visual.sorts, "sort"),
        (&visual.conditional_formatting, "conditional_formatting"),
        (&visual.alt_text, "alt_text"),
    ] {
        for target in targets {
            fields.push(FieldNode {
                kind: kind_of(target),
                target: target.to_string(),
                binding_site: site.to_string(),
                active: None,
            });
        }
    }

    let unresolved = broken
        .iter()
        .filter(|binding| {
            binding.edge.mobile == mobile
                && binding.edge.page.as_ref() == Some(&page.name)
                && binding.edge.visual.as_ref() == Some(&visual.name)
        })
        .map(|binding| unresolved_node(binding))
        .collect();

    VisualNode {
        name: visual.name.as_str().to_string(),
        visual_type: visual.visual_type.clone(),
        fields,
        filters: visual.filters.iter().map(filter_node).collect(),
        unresolved,
    }
}

/// One filter, metadata and references separated as the AST keeps them.
fn filter_node(filter: &Filter) -> FilterNode {
    FilterNode {
        name: filter.name.as_ref().map(|name| name.as_str().to_string()),
        display_name: filter.display_name.clone(),
        filter_type: filter.filter_type.clone(),
        target: filter.target.as_ref().map(ToString::to_string),
        references: filter.references.iter().map(ToString::to_string).collect(),
    }
}

/// One unresolved binding, in `scan`'s broken-binding vocabulary.
fn unresolved_node(binding: &BrokenBinding) -> UnresolvedNode {
    let (reason, artifact) = match &binding.reason {
        BrokenReason::BoundArtifactBroken { artifact, .. } => {
            ("bound_artifact_broken", Some(artifact.to_string()))
        }
        BrokenReason::TableNotFound => ("table_not_found", None),
        BrokenReason::FieldNotFound => ("field_not_found", None),
        BrokenReason::MeasureNotFound => ("measure_not_found", None),
        BrokenReason::HierarchyNotFound => ("hierarchy_not_found", None),
        BrokenReason::LevelNotFound => ("level_not_found", None),
    };
    UnresolvedNode {
        target: binding.target.to_string(),
        reason,
        artifact,
        binding_site: site_of(&binding.edge.kind),
    }
}

/// The field kind a `FieldTarget` states outright — the binding's own claim,
/// not a resolution verdict.
fn kind_of(target: &FieldTarget) -> &'static str {
    match target {
        FieldTarget::Column { .. } => "column",
        FieldTarget::Measure { .. } => "measure",
        FieldTarget::HierarchyLevel { .. } => "hierarchy_level",
        FieldTarget::Aggregation { .. } => "aggregation",
        FieldTarget::Written(_) => "written",
        FieldTarget::Mdx { .. } => "mdx",
    }
}

/// The binding-site vocabulary of the inventory: `field_well:<role>`, `filter`,
/// `sort`, `drillthrough`, `conditional_formatting`, `alt_text`,
/// `app_code:<file>:<line>`, `query:<connection>`.
fn site_of(site: &BindingSite) -> String {
    match site {
        BindingSite::FieldWell { role } => format!("field_well:{role}"),
        BindingSite::Filter => "filter".to_string(),
        BindingSite::Sort => "sort".to_string(),
        BindingSite::Drillthrough => "drillthrough".to_string(),
        BindingSite::ConditionalFormatting => "conditional_formatting".to_string(),
        BindingSite::AltText => "alt_text".to_string(),
        BindingSite::Code { file, line } => format!("app_code:{file}:{line}"),
        BindingSite::Query { connection } => format!("query:{connection}"),
    }
}

/// Whether an edge's recorded report name is this report's — `None` matches
/// `None` (the edge recorded no name; the report carries none).
fn names_match(edge_report: &Option<NameKey>, report_name: Option<&str>) -> bool {
    match (edge_report, report_name) {
        (Some(edge), Some(name)) => *edge == NameKey::new(name),
        (None, None) => true,
        _ => false,
    }
}

/// The display fallback when a report carries no name: its final folder
/// component.
fn fallback_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Everything the `report` command wants to say on stdout, as
/// presentation-ready data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inventory {
    /// The model the reports were paired with, as given.
    pub target: String,
    /// One node per ingested report, in ingestion order.
    pub reports: Vec<ReportNode>,
    /// Every skip notice ingestion recorded.
    pub skips: Vec<SkipNoticeOut>,
    /// Every model object reachability keeps alive, sorted by kind then id.
    /// Populated only when the `--used` view runs.
    pub used: Vec<UsedRow>,
}

/// One model object the report keeps alive — the complement of scan's unused
/// findings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedRow {
    /// The object kind, in scan's vocabulary (`measure`, `column`, …).
    pub kind: &'static str,
    /// The display id, e.g. `'Sales'[Total]`.
    pub id: String,
}

/// One report's inventory: its filters, pages, bookmarks, and report measures.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReportNode {
    /// The report item's path, as given.
    pub path: String,
    /// The report's display name (`ReportModel::name`), or its folder name.
    pub name: String,
    /// Report-level filters.
    pub filters: Vec<FilterNode>,
    /// Broken live bindings at report level (report filters, report-measure
    /// references). Bookmark-saved breakage is out of scope.
    pub unresolved: Vec<UnresolvedNode>,
    /// Desktop pages first, then phone-layout pages tagged `mobile`.
    pub pages: Vec<PageNode>,
    /// How many bookmarks the report carries (v1 lists the count only).
    pub bookmarks: usize,
    /// Report-level measure names (`reportExtensions.json`).
    pub report_measures: Vec<String>,
    /// The itemized unresolved bindings: every visual-, page-, and
    /// report-level row this report carries.
    pub unresolved_count: usize,
}

/// One page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageNode {
    /// The page's object name (the PBIR folder name).
    pub name: String,
    /// The author-facing display name, when the page carries one.
    pub display_name: Option<String>,
    /// Display-only, never liveness.
    pub hidden: bool,
    /// True for phone-layout pages (`definition.mobile/`, issue #49).
    pub mobile: bool,
    /// Page-level filters.
    pub filters: Vec<FilterNode>,
    /// Broken live bindings at page level (page filters, drillthrough
    /// parameters).
    pub unresolved: Vec<UnresolvedNode>,
    /// The page's visuals, in source order.
    pub visuals: Vec<VisualNode>,
}

/// One visual: every model object it references, and where each binds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VisualNode {
    /// The visual's object name (the PBIR folder name).
    pub name: String,
    /// The visual type as written, e.g. `"card"`, `"donutChart"`.
    pub visual_type: String,
    /// Field wells, sorts, conditional formatting, and alt text — the visual's
    /// references outside its filters.
    pub fields: Vec<FieldNode>,
    /// Visual-level filters, persisted automatic filters included.
    pub filters: Vec<FilterNode>,
    /// References that resolve to nothing in the model, in `scan`'s
    /// broken-binding vocabulary.
    pub unresolved: Vec<UnresolvedNode>,
}

/// One field reference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldNode {
    /// `column`, `measure`, `hierarchy_level`, `aggregation`, or `written` —
    /// the binding's own claim, not a resolution verdict.
    pub kind: &'static str,
    /// The written reference, e.g. `'Sales'[Total]`.
    pub target: String,
    /// Where the reference binds: `field_well:<role>`, `sort`,
    /// `conditional_formatting`, or `alt_text`.
    pub binding_site: String,
    /// Whether a field-well projection is active. Inactive projections still
    /// bind. `None` outside field wells.
    pub active: Option<bool>,
}

/// One filter at report, page, or visual level.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterNode {
    /// The filter's in-scope name, e.g. `Filter5`.
    pub name: Option<String>,
    /// The author-facing name, when the filter carries one.
    pub display_name: Option<String>,
    /// The filter kind as written, e.g. `Categorical` or `Advanced`.
    pub filter_type: Option<String>,
    /// The declared filtered field, when the filter states one.
    pub target: Option<String>,
    /// Further fields the condition tree references.
    pub references: Vec<String>,
}

/// One reference that resolves to nothing in the model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnresolvedNode {
    /// The written reference, e.g. `'Sales'[Color]`.
    pub target: String,
    /// Why it does not resolve — snake_case, the same vocabulary as `scan`'s
    /// broken bindings.
    pub reason: &'static str,
    /// The broken artifact the binding lands on, for the
    /// `bound_artifact_broken` reason; `None` otherwise.
    pub artifact: Option<String>,
    /// Where the reference binds — the same vocabulary as
    /// [`FieldNode::binding_site`], plus `filter` and `drillthrough`.
    pub binding_site: String,
}
