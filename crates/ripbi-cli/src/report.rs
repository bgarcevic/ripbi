//! The `report` orchestration (issue #33): resolve a target exactly the way
//! `scan` does, ingest the paired model and reports, and print the report
//! inventory — pages → visuals → fields — as data. The sanity check for "what
//! did ripbi actually see in this report", which is the question to ask when
//! a scan result looks wrong. Informational by design: nothing here gates the
//! exit code, so `scan`'s findings contract stays untouched.

pub(crate) mod render;

use std::collections::HashSet;
use std::path::Path;

use ripbi_core::NameKey;
use ripbi_core::graph::{BindingSite, BrokenBinding, BrokenReason, DependencyGraph};
use ripbi_core::ingest::{self, SkipKind};
use ripbi_core::report::{FieldTarget, Filter, Page, ReportModel, Visual};

use crate::cli::ReportArgs;
use crate::config;
use crate::error::ScanError;
use crate::glob;
use crate::render::SkipNoticeOut;
use crate::report::render::{
    FieldNode, FilterNode, Inventory, PageNode, ReportNode, UnresolvedNode, UsedRow, VisualNode,
};
use crate::scan::Streams;
use crate::scan::{dedupe, discover_target, resolve_explicit, skip_notice_out, write_skip_notices};
use crate::style::Palette;

/// Exit code: the inventory printed (or `--quiet` suppressed it). The command
/// is informational — unresolved bindings are listed, never failed on.
pub const EXIT_OK: i32 = 0;
/// Exit code: the inventory could not be produced.
pub const EXIT_ERROR: i32 = 2;

/// Runs `report` against the process's working directory.
#[must_use = "the return value is the process exit code"]
pub fn run(args: &ReportArgs, streams: &mut Streams<'_>) -> i32 {
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

/// Runs `report` against an explicit working directory (tests pass one).
#[must_use = "the return value is the process exit code"]
pub fn run_in(args: &ReportArgs, cwd: &Path, streams: &mut Streams<'_>) -> i32 {
    let palette_err = Palette::detect(streams.stderr_is_tty, args.no_color);
    match inventory(args, cwd, streams, &palette_err) {
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

fn inventory(
    args: &ReportArgs,
    cwd: &Path,
    streams: &mut Streams<'_>,
    palette_err: &Palette,
) -> Result<i32, ScanError> {
    let palette_out = Palette::detect(streams.stdout_is_tty, args.no_color);
    let loaded = config::find_in(cwd)?;
    let config = loaded.map(|loaded| loaded.config);

    // Target: PATH argument, else config `target`, else folder discovery —
    // the same ladder `scan` climbs, sharing its helpers. The `reports` and
    // `[scan].ignore` keys are scan-specific and deliberately unread here.
    let explicit = args
        .path
        .clone()
        .or_else(|| config.as_ref().and_then(|config| config.target.clone()));
    let paired = match explicit {
        Some(path) => resolve_explicit(&path)?,
        None => discover_target(args.no_input, args.quiet, cwd, streams, palette_err)?.0,
    };
    let report_paths = dedupe(paired.reports);
    if report_paths.is_empty() {
        return Err(ScanError::new(format!(
            "nothing to inventory: {} has no reports",
            paired.model.display()
        ))
        .with_hint("pass a .pbip project, a .Report folder, or a project folder"));
    }

    if !args.quiet {
        writeln!(
            streams.err,
            "Reading {} with {} report(s)",
            paired.model.display(),
            report_paths.len()
        )
        .map_err(ScanError::from)?;
    }

    // Ingest the pair, then build the graph — the same pipeline `scan` runs,
    // because "unresolved" here means exactly what `scan`'s broken bindings
    // mean: a written reference that names nothing in the model.
    let model = ingest::semantic_model(&paired.model).map_err(|error| {
        ScanError::new(format!("cannot ingest {}: {error}", paired.model.display()))
    })?;
    let mut skips: Vec<SkipNoticeOut> = model.skips.iter().map(skip_notice_out).collect();
    let mut reports = Vec::new();
    for path in &report_paths {
        let ingested = ingest::report(path).map_err(|error| {
            ScanError::new(format!("cannot ingest report {}: {error}", path.display()))
        })?;
        skips.extend(ingested.skips.iter().map(skip_notice_out));
        reports.push(ingested.value);
    }
    let report_refs: Vec<&ReportModel> = reports.iter().collect();
    let graph = DependencyGraph::build(&model.value, &report_refs);

    // Issue #60's precision bar, shared with `scan`: model-side
    // `unknown_object` skips can hide the very name a binding wrote, so no
    // unresolved claim is made while any are present.
    let hides_a_name = model
        .skips
        .iter()
        .any(|skip| matches!(skip.kind, SkipKind::UnknownObject));

    let mut nodes = Vec::new();
    for (path, report) in report_paths.iter().zip(&reports) {
        nodes.push(report_node(path, report, &graph, hides_a_name));
    }
    let mut output = Inventory {
        target: paired.model.display().to_string(),
        reports: nodes,
        skips,
        used: Vec::new(),
    };
    apply_filters(&mut output, args);
    if args.used {
        output.used = used_rows(&graph, args);
    }

    if !args.quiet {
        if args.json {
            render::json(streams.out, &output).map_err(ScanError::from)?;
        } else if args.plain {
            render::plain(streams.out, &output).map_err(ScanError::from)?;
        } else {
            let mut table = false;
            if args.pages {
                render::pages_view(streams.out, &output).map_err(ScanError::from)?;
                table = true;
            }
            if args.visuals {
                if table {
                    writeln!(streams.out).map_err(ScanError::from)?;
                }
                render::visuals_view(streams.out, &output).map_err(ScanError::from)?;
                table = true;
            }
            if args.fields {
                if table {
                    writeln!(streams.out).map_err(ScanError::from)?;
                }
                render::fields_view(streams.out, &output).map_err(ScanError::from)?;
                table = true;
            }
            if args.used {
                if table {
                    writeln!(streams.out).map_err(ScanError::from)?;
                }
                render::used_view(streams.out, &output).map_err(ScanError::from)?;
                table = true;
            }
            if !table {
                render::human(streams.out, &palette_out, &output).map_err(ScanError::from)?;
            }
            // The next-command suggestion is stderr's job, and it comes after
            // every stdout line: the streams can buffer independently.
            if args.visuals {
                let unresolved: usize = output.reports.iter().map(|r| r.unresolved_count).sum();
                if unresolved > 0 {
                    writeln!(
                        streams.err,
                        "{}",
                        palette_err.dim(&format!(
                            "{unresolved} unresolved — see: ripbi report --broken"
                        ))
                    )
                    .map_err(ScanError::from)?;
                }
            }
        }
        // Notices and notes are stderr's job in text modes; --json carries
        // the notices itself.
        if !args.json {
            write_skip_notices(streams.err, &output.skips)?;
        }
        write_empty_note(args, &output, streams.err)?;
    }
    Ok(EXIT_OK)
}

/// The stderr note when filters matched nothing — silence reads as breakage.
/// Each requested kind is judged on its own survivors: `--page` on pages,
/// `--visual` on visuals, the row filters on bindings.
fn write_empty_note(
    args: &ReportArgs,
    output: &Inventory,
    err: &mut dyn std::io::Write,
) -> Result<(), ScanError> {
    let rows_requested =
        !args.matches.is_empty() || !args.kind.is_empty() || !args.site.is_empty() || args.broken;
    if !rows_requested && args.page.is_empty() && args.visual.is_empty() {
        return Ok(());
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
    let message = if !args.page.is_empty() && pages == 0 {
        Some(format!("no pages match {}", args.page.join(", ")))
    } else if !args.visual.is_empty() && visuals == 0 {
        Some(format!("no visuals match {}", args.visual.join(", ")))
    } else if args.used && output.used.is_empty() {
        Some("no used objects match the given filters".to_string())
    } else if rows_requested && rows == 0 {
        Some("no bindings match the given filters".to_string())
    } else {
        None
    };
    if let Some(message) = message {
        writeln!(err, "{message}").map_err(ScanError::from)?;
    }
    Ok(())
}

/// Every model object reachability keeps alive — the complement of scan's
/// unused findings, sorted by kind then id. `--kind` and `--match` narrow it
/// by the two columns the view shows; the other filters do not: reachability
/// always describes the whole report.
fn used_rows(graph: &DependencyGraph, args: &ReportArgs) -> Vec<UsedRow> {
    let unused: HashSet<ripbi_core::ObjectId> = graph
        .unused_objects()
        .into_iter()
        .map(|finding| finding.id)
        .collect();
    let mut rows: Vec<UsedRow> = graph
        .object_ids()
        .filter(|id| !unused.contains(*id))
        .map(|id| UsedRow {
            kind: crate::render::kind_of(id),
            id: id.to_string(),
        })
        .filter(|row| {
            (args.kind.is_empty() || args.kind.iter().any(|glob| glob::matches(glob, row.kind)))
                && (args.matches.is_empty()
                    || args.matches.iter().any(|glob| glob::matches(glob, &row.id)))
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
fn apply_filters(inventory: &mut Inventory, args: &ReportArgs) {
    let target_globs: Vec<&str> = args.matches.iter().map(String::as_str).collect();
    let kind_globs: Vec<&str> = args.kind.iter().map(String::as_str).collect();
    let site_globs: Vec<&str> = args.site.iter().map(String::as_str).collect();
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
    let prune_rows =
        !target_globs.is_empty() || !kind_globs.is_empty() || !site_globs.is_empty() || args.broken;
    let filtering = prune_rows || !args.page.is_empty() || !args.visual.is_empty();

    for report in &mut inventory.reports {
        if args.broken {
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
        if !args.page.is_empty() {
            pages.retain(|page| {
                args.page.iter().any(|pattern| {
                    glob::matches(pattern, &page.name)
                        || page
                            .display_name
                            .as_deref()
                            .is_some_and(|display| glob::matches(pattern, display))
                })
            });
        }
        for page in &mut pages {
            if args.broken {
                page.filters.clear();
            } else if prune_rows {
                page.filters
                    .retain(|filter| filter_matches(filter, &row_selected));
            }
            page.unresolved.retain(|unresolved| {
                row_selected(&unresolved.target, "unresolved", &unresolved.binding_site)
            });
            let mut visuals = std::mem::take(&mut page.visuals);
            if !args.visual.is_empty() {
                visuals.retain(|visual| {
                    args.visual.iter().any(|pattern| {
                        glob::matches(pattern, &visual.name)
                            || glob::matches(pattern, &visual.visual_type)
                    })
                });
            }
            for visual in &mut visuals {
                if args.broken {
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

/// The inventory of one ingested report.
fn report_node(
    path: &Path,
    report: &ReportModel,
    graph: &DependencyGraph,
    hides_a_name: bool,
) -> ReportNode {
    let name = report.name.clone().unwrap_or_else(|| fallback_name(path));

    // The report's broken *live* bindings. Attribution joins on the report
    // name the edge recorded — name, not identity, so two same-named reports
    // share attribution; benign for an informational listing. Bookmark-saved
    // state is out of scope for v1 (bookmarks are a count line), so bindings
    // a bookmark carries are skipped.
    let broken: Vec<&BrokenBinding> = if hides_a_name {
        Vec::new()
    } else {
        graph
            .broken_bindings()
            .iter()
            .filter(|binding| binding.edge.bookmark.is_none())
            .filter(|binding| names_match(&binding.edge.report, report.name.as_deref()))
            .collect()
    };

    let mut pages = Vec::new();
    for page in &report.pages {
        pages.push(page_node(page, false, &broken));
    }
    for page in &report.mobile_pages {
        pages.push(page_node(page, true, &broken));
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
        BrokenReason::BoundArtifactBroken { artifact } => {
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
    }
}

/// The binding-site vocabulary of the inventory: `field_well:<role>`, `filter`,
/// `sort`, `drillthrough`, `conditional_formatting`, `alt_text`.
fn site_of(site: &BindingSite) -> String {
    match site {
        BindingSite::FieldWell { role } => format!("field_well:{role}"),
        BindingSite::Filter => "filter".to_string(),
        BindingSite::Sort => "sort".to_string(),
        BindingSite::Drillthrough => "drillthrough".to_string(),
        BindingSite::ConditionalFormatting => "conditional_formatting".to_string(),
        BindingSite::AltText => "alt_text".to_string(),
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
