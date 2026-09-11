//! PBIR report parsing: a `.Report` `definition/` folder into a
//! [`ReportModel`].
//!
//! PBIR is a folder of small JSON documents, one per report object —
//! `report.json`, `pages/*/page.json`, `pages/*/visuals/*/visual.json`,
//! `bookmarks/*.bookmark.json` — each carrying a `$schema` URL that drifts
//! across Power BI versions. Parsing is therefore one pass per file against
//! per-file key policies ([`Keys`]): keys the AST models are parsed, keys
//! deliberately unmodeled are skipped silently, and anything else is reported
//! as a [`SkipNotice`] (see `docs/formats.md`). Only the anchor `report.json`
//! can fail the run; every other file, object, or field that cannot be read is
//! a notice, because a single drifted visual must never abort an analysis.
//!
//! Every model reference that keeps an object alive lands in the AST, whatever
//! its visual type: field wells (`queryState`), report/page/visual filters and
//! the fields inside their condition trees, sort definitions, conditional
//! formatting (`FillRule` and friends), drillthrough parameters, bookmarks'
//! saved filters and projections (sections whose page the report no longer
//! defines are skipped as stale, issue #48), tooltip pages, and the columns
//! behind field parameters. Filter *values* (slicer selections, comparison
//! literals) are data, never references, and are ignored.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::Result;
use crate::dax;
use crate::identity::{FieldRef, NameKey, fold_name};
use crate::ingest::{SkipKind, SkipNotice};
use crate::report::{
    Bookmark, BookmarkSection, BookmarkVisual, DatasetReference, DrillthroughParameter,
    FieldTarget, FieldWell, Filter, Page, PageBinding, PageBindingKind, Projection, ReportMeasure,
    ReportModel, Visual,
};

/// Aggregation function names by PBIR `Function` code
/// (`semanticQuery` `QueryAggregateFunction`). An unknown code keeps the inner
/// reference with `function: None` — the code is diagnostics only.
const AGGREGATION_FUNCTIONS: [&str; 9] = [
    "Sum",
    "Average",
    "DistinctCount",
    "Min",
    "Max",
    "Count",
    "Median",
    "StdDev",
    "Variance",
];

/// Container keys whose value is a field-shaped expression
/// (`semanticQuery` `QueryExpressionContainer` variants this crate reads).
const FIELD_VARIANTS: &[&str] = &[
    "Column",
    "Measure",
    "Aggregation",
    "HierarchyLevel",
    "Hierarchy",
    "Min",
    "Max",
    "Percentile",
];

/// Parses a report's `definition/` folder into a [`ReportModel`].
///
/// Files are processed in a fixed order (the anchor `report.json`, the
/// dataset reference, `reportExtensions.json`, then pages and their visuals in
/// folder-name order, then bookmarks in file-name order) so notices come out
/// deterministic. Pages are discovered by walking `pages/` subdirectories;
/// `pages/pages.json` is read only for `pageOrder`, one of the two authorities
/// on which pages exist (with the folders themselves) that a bookmark section
/// must clear to bind — page order and the active page stay unmodeled.
pub(super) fn load_report(
    definition: &Path,
    name: Option<String>,
    skips: &mut Vec<SkipNotice>,
) -> Result<ReportModel> {
    // The anchor: `locate_report_definition` guarantees report.json exists, so
    // a failure here is a real error, never drift.
    let report_path = definition.join("report.json");
    let report = read_json(&report_path)?;

    let mut model = ReportModel {
        name,
        dataset: dataset_reference(definition.parent().unwrap_or(definition), skips),
        ..Default::default()
    };

    {
        let mut ctx = Ctx {
            path: &report_path,
            skips,
        };
        check_keys(&report, &REPORT_KEYS, &mut ctx, "");
        model.filters = filter_config(report.get("filterConfig"), &mut ctx, "/filterConfig");
    }
    model.measures = report_extensions(definition, skips);
    model.pages = pages(definition, skips);
    let live = live_sections(definition, &model.pages, skips);
    model.bookmarks = bookmarks(definition, &live, skips);
    Ok(model)
}

// --- File-level parsers ------------------------------------------------------

/// Parses `reportExtensions.json`: report-level measures, grouped by extension
/// entity and flattened in file order.
fn report_extensions(definition: &Path, skips: &mut Vec<SkipNotice>) -> Vec<ReportMeasure> {
    let path = definition.join("reportExtensions.json");
    let Some(value) = read_optional(&path, skips) else {
        return Vec::new();
    };
    let mut ctx = Ctx { path: &path, skips };
    let mut measures = Vec::new();
    let Some(entities) = value.get("entities").and_then(Value::as_array) else {
        return measures;
    };
    for (entity_index, entity) in entities.iter().enumerate() {
        let Some(list) = entity.get("measures").and_then(Value::as_array) else {
            continue;
        };
        for (measure_index, measure) in list.iter().enumerate() {
            let location = format!("/entities/{entity_index}/measures/{measure_index}");
            check_keys(measure, &EXTENSION_MEASURE_KEYS, &mut ctx, &location);
            let (Some(measure_name), Some(expression)) = (
                measure.get("name").and_then(Value::as_str),
                measure.get("expression").and_then(Value::as_str),
            ) else {
                ctx.notice(
                    &location,
                    SkipKind::MalformedValue,
                    "report measure is missing its name or expression",
                );
                continue;
            };
            measures.push(ReportMeasure {
                name: NameKey::new(measure_name),
                expression: expression.to_string(),
                format_string: measure
                    .get("formatString")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
    }
    measures
}

/// Parses `definition.pbir` (beside `definition/`): which semantic model the
/// report connects to. The file is absent in some standalone layouts and is
/// provenance only, so any absence or drift yields [`DatasetReference::Unresolved`].
pub(super) fn dataset_reference(item_root: &Path, skips: &mut Vec<SkipNotice>) -> DatasetReference {
    let path = item_root.join("definition.pbir");
    let Some(value) = read_optional(&path, skips) else {
        return DatasetReference::Unresolved;
    };
    let unresolved = |skips: &mut Vec<SkipNotice>, detail: String| {
        skips.push(SkipNotice {
            path: path.clone(),
            location: Some("/datasetReference".to_string()),
            kind: SkipKind::MalformedValue,
            detail,
        });
        DatasetReference::Unresolved
    };
    let Some(reference) = value.get("datasetReference") else {
        return unresolved(
            skips,
            "definition.pbir carries no datasetReference".to_string(),
        );
    };
    match (
        reference.get("byPath").and_then(|by| by.get("path")),
        reference
            .get("byConnection")
            .and_then(|by| by.get("connectionString")),
    ) {
        (Some(path_value), None) => match path_value.as_str() {
            Some(path) => DatasetReference::ByPath {
                path: path.to_string(),
            },
            None => unresolved(skips, "byPath carries no path".to_string()),
        },
        (None, Some(connection)) => match connection.as_str() {
            Some(connection) => DatasetReference::ByConnection {
                connection_string: connection.to_string(),
            },
            None => unresolved(
                skips,
                "byConnection carries no connectionString".to_string(),
            ),
        },
        // The schema demands exactly one of the two.
        (Some(_), Some(_)) => unresolved(
            skips,
            "datasetReference carries both byPath and byConnection".to_string(),
        ),
        (None, None) => unresolved(
            skips,
            "datasetReference carries neither byPath nor byConnection".to_string(),
        ),
    }
}

/// Parses every page folder under `pages/`, in folder-name order.
fn pages(definition: &Path, skips: &mut Vec<SkipNotice>) -> Vec<Page> {
    let mut folders = child_folders(&definition.join("pages"))
        .into_iter()
        .filter(|folder| folder.join("page.json").is_file())
        .collect::<Vec<_>>();
    folders.sort_by_key(|folder| folder.file_name().unwrap_or_default().to_os_string());

    let mut out = Vec::new();
    for folder in folders {
        let page_path = folder.join("page.json");
        let value = match read_json(&page_path) {
            Ok(value) => value,
            Err(error) => {
                skips.push(SkipNotice {
                    path: page_path.clone(),
                    location: None,
                    kind: SkipKind::MalformedValue,
                    detail: format!("page.json could not be read: {error}"),
                });
                continue;
            }
        };
        let mut ctx = Ctx {
            path: &page_path,
            skips,
        };
        let mut page = page(&value, folder_name(&folder), &mut ctx);
        page.visuals = visuals(&folder, ctx.skips);
        out.push(page);
    }
    out
}

/// The folded object names of every page the report defines — the authority a
/// bookmark section must clear to count as a binding (issue #48): Power BI
/// leaves deleted pages' sections inside bookmarks forever, and saved filters
/// on a page nobody can navigate to would keep their columns "live" with no
/// way to ever re-apply them.
///
/// The set is the *union* of `pages.json` `pageOrder` and the parsed
/// `pages/` folders, compared case-insensitively like every object name. A
/// page named by one source only is still live — its folder or listing is
/// proof enough, and stripping a real page's bookmark bindings would
/// under-count roots — but the disagreement is reported, because healthy
/// reports list every page in both.
fn live_sections(
    definition: &Path,
    pages: &[Page],
    skips: &mut Vec<SkipNotice>,
) -> HashSet<String> {
    let pages_path = definition.join("pages").join("pages.json");
    let mut page_order = Vec::new();
    // Disagreement is only reportable against a `pageOrder` that was actually
    // read: without the file (or with an unreadable one, already noticed)
    // there is nothing to disagree with, and the folders stand alone.
    let mut order_known = false;
    if let Some(value) = read_optional(&pages_path, skips) {
        let mut ctx = Ctx {
            path: &pages_path,
            skips,
        };
        check_keys(&value, &PAGES_KEYS, &mut ctx, "");
        match value.get("pageOrder").and_then(Value::as_array) {
            Some(list) => {
                order_known = true;
                for entry in list {
                    if let Some(name) = entry.as_str() {
                        page_order.push(name.to_string());
                    }
                }
            }
            None => ctx.notice(
                "/pageOrder",
                SkipKind::MalformedValue,
                "pages.json carries no readable pageOrder",
            ),
        }
    }
    // `pages()` walks folders in name order, so both inputs are deterministic.
    let folder_pages: Vec<String> = pages
        .iter()
        .map(|page| page.name.as_str().to_string())
        .collect();
    let mut ctx = Ctx {
        path: &pages_path,
        skips,
    };
    reconcile_pages(&page_order, &folder_pages, order_known, &mut ctx)
}

/// Unions the two page-name sources into the folded live-section set. When
/// `order_known`, every disagreement is reported through `ctx` (the pages.json
/// side, since `pageOrder` is what disagrees with the folders on disk). Both
/// inputs are taken in their given order, so notices are deterministic.
fn reconcile_pages(
    page_order: &[String],
    folder_pages: &[String],
    order_known: bool,
    ctx: &mut Ctx,
) -> HashSet<String> {
    let order_set: HashSet<String> = page_order.iter().map(|name| fold_name(name)).collect();
    let folder_set: HashSet<String> = folder_pages.iter().map(|name| fold_name(name)).collect();

    if order_known {
        for (index, name) in page_order.iter().enumerate() {
            if !folder_set.contains(&fold_name(name)) {
                ctx.notice(
                    format!("/pageOrder/{index}"),
                    SkipKind::StaleState,
                    format!("pageOrder lists '{name}', which has no page folder under pages/"),
                );
            }
        }
        for name in folder_pages {
            if !order_set.contains(&fold_name(name)) {
                ctx.notice(
                    "/pageOrder",
                    SkipKind::StaleState,
                    format!(
                        "page '{name}' has a folder under pages/, which pageOrder does not list"
                    ),
                );
            }
        }
    }

    order_set
        .union(&folder_set)
        .cloned()
        .collect::<HashSet<String>>()
}

/// Parses one `page.json`.
fn page(value: &Value, folder: &str, ctx: &mut Ctx) -> Page {
    check_keys(value, &PAGE_KEYS, ctx, "");
    let Some(name) = value.get("name").and_then(Value::as_str) else {
        ctx.notice(
            "/name",
            SkipKind::MalformedValue,
            "page.json carries no page name; using the folder name",
        );
        return Page {
            name: NameKey::new(folder),
            display_name: None,
            is_hidden: false,
            filters: Vec::new(),
            binding: None,
            visuals: Vec::new(),
        };
    };
    Page {
        name: NameKey::new(name),
        display_name: value
            .get("displayName")
            .and_then(Value::as_str)
            .map(str::to_string),
        // Hidden pages still bind — the flag is display-only, never liveness.
        is_hidden: value.get("visibility").and_then(Value::as_str) == Some("HiddenInViewMode"),
        filters: filter_config(value.get("filterConfig"), ctx, "/filterConfig"),
        binding: value
            .get("pageBinding")
            .map(|binding| page_binding(binding, ctx, "/pageBinding")),
        visuals: Vec::new(),
    }
}

/// Parses a page's `pageBinding`: its drillthrough/tooltip role and the fields
/// its parameters bind.
fn page_binding(value: &Value, ctx: &mut Ctx, location: &str) -> PageBinding {
    check_keys(value, &PAGE_BINDING_KEYS, ctx, location);
    let kind = match value.get("type").and_then(Value::as_str) {
        Some("Drillthrough") => PageBindingKind::Drillthrough,
        Some("Tooltip") => PageBindingKind::Tooltip,
        _ => PageBindingKind::Default,
    };
    let mut parameters = Vec::new();
    if let Some(list) = value.get("parameters").and_then(Value::as_array) {
        for (index, parameter) in list.iter().enumerate() {
            let location = format!("{location}/parameters/{index}");
            check_keys(parameter, &PARAMETER_KEYS, ctx, &location);
            // A parameter without a readable field keeps no slot: the AST
            // models only parameters that bind, and the loss is noticed.
            if let Some(target) = required_field(
                parameter.get("fieldExpr"),
                &Aliases::new(),
                ctx,
                &format!("{location}/fieldExpr"),
            ) {
                parameters.push(DrillthroughParameter {
                    name: parameter
                        .get("name")
                        .and_then(Value::as_str)
                        .map(NameKey::new),
                    target,
                });
            }
        }
    }
    PageBinding { kind, parameters }
}

/// Parses every visual folder of a page, in folder-name order. Group
/// containers (`visualGroup`, which carry no query) are skipped.
fn visuals(folder: &Path, skips: &mut Vec<SkipNotice>) -> Vec<Visual> {
    let mut folders = child_folders(&folder.join("visuals"))
        .into_iter()
        .filter(|folder| folder.join("visual.json").is_file())
        .collect::<Vec<_>>();
    folders.sort_by_key(|folder| folder.file_name().unwrap_or_default().to_os_string());

    let mut out = Vec::new();
    for visual_folder in folders {
        let visual_path = visual_folder.join("visual.json");
        let value = match read_json(&visual_path) {
            Ok(value) => value,
            Err(error) => {
                skips.push(SkipNotice {
                    path: visual_path.clone(),
                    location: None,
                    kind: SkipKind::MalformedValue,
                    detail: format!("visual.json could not be read: {error}"),
                });
                continue;
            }
        };
        let mut ctx = Ctx {
            path: &visual_path,
            skips,
        };
        if let Some(visual) = visual(&value, folder_name(&visual_folder), &mut ctx) {
            out.push(visual);
        }
    }
    out
}

/// Parses one visual container (`visual.json`).
fn visual(value: &Value, folder: &str, ctx: &mut Ctx) -> Option<Visual> {
    check_keys(value, &VISUAL_KEYS, ctx, "");
    let inner = value.get("visual")?;
    check_keys(inner, &VISUAL_KEYS_INNER, ctx, "/visual");
    let query = inner.get("query");
    if let Some(query) = query {
        check_keys(query, &QUERY_KEYS, ctx, "/visual/query");
    }
    let Some(visual_type) = inner.get("visualType").and_then(Value::as_str) else {
        ctx.notice(
            "/visual/visualType",
            SkipKind::MalformedValue,
            "visual carries no visualType",
        );
        return None;
    };
    // A visual container's own filters sit beside `visual`, not under it; the
    // formatting objects contribute more still — a persisted automatic filter
    // — and their remaining fields are conditional formatting.
    let (mut filters, conditional_formatting) =
        visual_objects(inner.get("objects"), ctx, "/visual/objects");
    filters.extend(filter_config(
        value.get("filterConfig"),
        ctx,
        "/filterConfig",
    ));
    let alt_text = alt_text_targets(
        inner.get("visualContainerObjects"),
        ctx,
        "/visual/visualContainerObjects",
    );
    Some(Visual {
        name: NameKey::new(value.get("name").and_then(Value::as_str).unwrap_or(folder)),
        visual_type: visual_type.to_string(),
        wells: wells(
            query.and_then(|query| query.get("queryState")),
            ctx,
            "/visual/query/queryState",
        ),
        filters,
        sorts: query
            .map(|query| sorts(query, ctx, "/visual/query"))
            .unwrap_or_default(),
        conditional_formatting,
        alt_text,
        tooltip_page: tooltip_page(
            inner.get("visualContainerObjects"),
            ctx,
            "/visual/visualContainerObjects",
        ),
    })
}

/// Parses every `*.bookmark.json` under `bookmarks/`, in file-name order.
/// `live` is the set of section IDs (folded) whose pages the report defines.
fn bookmarks(
    definition: &Path,
    live: &HashSet<String>,
    skips: &mut Vec<SkipNotice>,
) -> Vec<Bookmark> {
    let Ok(entries) = fs::read_dir(definition.join("bookmarks")) else {
        return Vec::new();
    };
    let mut files = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".bookmark.json"))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|path| path.file_name().unwrap_or_default().to_os_string());

    let mut out = Vec::new();
    for file in files {
        let value = match read_json(&file) {
            Ok(value) => value,
            Err(error) => {
                skips.push(SkipNotice {
                    path: file.clone(),
                    location: None,
                    kind: SkipKind::MalformedValue,
                    detail: format!("bookmark could not be read: {error}"),
                });
                continue;
            }
        };
        let mut ctx = Ctx { path: &file, skips };
        out.push(bookmark_file(&value, bookmark_name(&file), live, &mut ctx));
    }
    out
}

/// Parses one bookmark file: saved filters (report level, per section, per
/// visual) and saved projections. Sections whose page the report no longer
/// defines are skipped whole with a [`SkipKind::StaleState`] notice (issue
/// #48) — Power BI leaves them behind when a page is deleted, and counting
/// their saved filters as bindings would keep those columns "live" forever.
fn bookmark_file(value: &Value, fallback: &str, live: &HashSet<String>, ctx: &mut Ctx) -> Bookmark {
    check_keys(value, &BOOKMARK_KEYS, ctx, "");
    let Some(name) = value.get("name").and_then(Value::as_str) else {
        ctx.notice(
            "/name",
            SkipKind::MalformedValue,
            "bookmark carries no name; using the file name",
        );
        return Bookmark {
            name: NameKey::new(fallback),
            display_name: None,
            filters: Vec::new(),
            sections: Vec::new(),
        };
    };
    let state = value.get("explorationState");
    if let Some(state) = state {
        check_keys(state, &EXPLORATION_KEYS, ctx, "/explorationState");
    }
    let active_section = state
        .and_then(|state| state.get("activeSection"))
        .and_then(Value::as_str);
    let active_folded = active_section.map(fold_name);
    // Sections this pass skipped as stale; if the active section is among
    // them it is already covered by that section's notice.
    let mut stale_sections: Vec<String> = Vec::new();

    let mut sections = Vec::new();
    if let Some(map) = state
        .and_then(|state| state.get("sections"))
        .and_then(Value::as_object)
    {
        for (page_name, section_state) in map {
            let folded = fold_name(page_name);
            if !live.contains(&folded) {
                let mut detail = format!(
                    "bookmark '{name}' saves state for page '{page_name}', which the report \
                     no longer defines; its saved filters bind nothing"
                );
                if active_folded.as_deref() == Some(folded.as_str()) {
                    detail.push_str(" (it is also the bookmark's active section)");
                }
                ctx.notice(
                    format!("/explorationState/sections/{page_name}"),
                    SkipKind::StaleState,
                    detail,
                );
                stale_sections.push(folded);
                continue;
            }
            let location = format!("/explorationState/sections/{page_name}");
            check_keys(section_state, &SECTION_KEYS, ctx, &location);
            let mut visuals = Vec::new();
            if let Some(containers) = section_state
                .get("visualContainers")
                .and_then(Value::as_object)
            {
                for (visual_name, container) in containers {
                    let location = format!("{location}/visualContainers/{visual_name}");
                    visuals.push(bookmark_visual(visual_name, container, ctx, &location));
                }
            }
            sections.push(BookmarkSection {
                page: NameKey::new(page_name),
                filters: filters_state(
                    section_state.get("filters"),
                    ctx,
                    &format!("{location}/filters"),
                ),
                visuals,
            });
        }
    }

    // The active section names where the bookmark was captured; a stale one
    // is the same deleted page seen from the bookmark's anchor, worth its own
    // notice only when no section notice already covered it.
    if let (Some(active), Some(folded_active)) = (active_section, active_folded)
        && !live.contains(&folded_active)
        && !stale_sections.contains(&folded_active)
    {
        ctx.notice(
            "/explorationState/activeSection",
            SkipKind::StaleState,
            format!(
                "bookmark '{name}' is anchored on page '{active}', which the report \
                 no longer defines"
            ),
        );
    }

    Bookmark {
        name: NameKey::new(name),
        display_name: value
            .get("displayName")
            .and_then(Value::as_str)
            .map(str::to_string),
        filters: filters_state(
            state.and_then(|state| state.get("filters")),
            ctx,
            "/explorationState/filters",
        ),
        sections,
    }
}

/// Parses one saved visual state of a bookmark section.
fn bookmark_visual(name: &str, container: &Value, ctx: &mut Ctx, location: &str) -> BookmarkVisual {
    check_keys(container, &BOOKMARK_VISUAL_KEYS, ctx, location);
    let mut wells = Vec::new();
    if let Some(single) = container.get("singleVisual") {
        let location = format!("{location}/singleVisual");
        check_keys(single, &SINGLE_VISUAL_KEYS, ctx, &location);
        for key in ["projections", "activeProjections"] {
            let Some(map) = single.get(key).and_then(Value::as_object) else {
                continue;
            };
            for (role, items) in map {
                let mut targets = Vec::new();
                collect_fields(
                    items,
                    &Aliases::new(),
                    ctx,
                    &format!("{location}/{key}/{role}"),
                    &mut targets,
                );
                if !targets.is_empty() {
                    wells.push(FieldWell {
                        role: role.clone(),
                        projections: targets
                            .into_iter()
                            .map(|target| Projection {
                                target,
                                query_ref: None,
                                active: false,
                            })
                            .collect(),
                    });
                }
            }
        }
    }
    BookmarkVisual {
        visual: NameKey::new(name),
        wells,
        filters: filters_state(
            container.get("filters"),
            ctx,
            &format!("{location}/filters"),
        ),
    }
}

// --- Filters -----------------------------------------------------------------

/// Parses a `filterConfig` object (report, page, and visual level all share
/// the shape): a `filters` array of filter entries.
fn filter_config(config: Option<&Value>, ctx: &mut Ctx, location: &str) -> Vec<Filter> {
    let Some(config) = config else {
        return Vec::new();
    };
    check_keys(config, &FILTER_CONFIG_KEYS, ctx, location);
    let Some(list) = config.get("filters").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .enumerate()
        .map(|(index, entry)| filter_entry(entry, ctx, &format!("{location}/filters/{index}")))
        .collect()
}

/// Parses a bookmark `FiltersState`: `byName` (an object) plus the `byExpr`,
/// `byType`, and `byTransientState` arrays.
fn filters_state(state: Option<&Value>, ctx: &mut Ctx, location: &str) -> Vec<Filter> {
    let Some(state) = state else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(by_name) = state.get("byName").and_then(Value::as_object) {
        for (name, entry) in by_name {
            out.push(filter_entry(
                entry,
                ctx,
                &format!("{location}/byName/{name}"),
            ));
        }
    }
    for key in ["byExpr", "byType", "byTransientState"] {
        if let Some(list) = state.get(key).and_then(Value::as_array) {
            for (index, entry) in list.iter().enumerate() {
                out.push(filter_entry(
                    entry,
                    ctx,
                    &format!("{location}/{key}/{index}"),
                ));
            }
        }
    }
    out
}

/// Parses one filter entry — the same shape at every scope and in bookmarks,
/// except that `filterConfig` names the filtered field under `field` and
/// bookmark states under `expression`.
fn filter_entry(entry: &Value, ctx: &mut Ctx, location: &str) -> Filter {
    check_keys(entry, &FILTER_ENTRY_KEYS, ctx, location);
    let aliases = entry.get("filter").map(query_aliases).unwrap_or_default();
    let target = required_field(
        entry.get("field").or_else(|| entry.get("expression")),
        &aliases,
        ctx,
        &format!("{location}/field"),
    );

    let mut references = Vec::new();
    if let Some(definition) = entry.get("filter") {
        let definition_location = format!("{location}/filter");
        check_keys(
            definition,
            &FILTER_DEFINITION_KEYS,
            ctx,
            &definition_location,
        );
        if let Some(clauses) = definition.get("Where").and_then(Value::as_array) {
            for (index, clause) in clauses.iter().enumerate() {
                let clause_location = format!("{definition_location}/Where/{index}");
                check_keys(clause, &QUERY_FILTER_KEYS, ctx, &clause_location);
                if let Some(condition) = clause.get("Condition") {
                    collect_fields(
                        condition,
                        &aliases,
                        ctx,
                        &format!("{clause_location}/Condition"),
                        &mut references,
                    );
                }
                if let Some(targets) = clause.get("Target").and_then(Value::as_array) {
                    for (index, target_value) in targets.iter().enumerate() {
                        if let FieldParse::Target(target) = parse_field(
                            target_value,
                            &aliases,
                            ctx,
                            &format!("{clause_location}/Target/{index}"),
                        ) {
                            references.push(*target);
                        }
                    }
                }
            }
        }
    }

    Filter {
        name: entry.get("name").and_then(Value::as_str).map(NameKey::new),
        target,
        references,
    }
}

/// Reads a filter definition's `From` array into alias → entity (table name).
/// Aliases compare case-insensitively, like every other name in this crate.
fn query_aliases(definition: &Value) -> Aliases {
    definition
        .get("From")
        .and_then(Value::as_array)
        .map(|from| {
            from.iter()
                .filter_map(|source| {
                    let name = source.get("Name").and_then(Value::as_str)?;
                    let entity = source.get("Entity").and_then(Value::as_str)?;
                    Some((fold_name(name), entity.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

// --- Field trees -------------------------------------------------------------

type Aliases = HashMap<String, String>;

/// What a field container parsed into.
#[derive(Debug, PartialEq, Eq)]
enum FieldParse {
    /// A structured reference. Boxed: the other variants are unit-sized and
    /// this enum comes back from every field parse, so the payload's size
    /// would otherwise be paid on every return.
    Target(Box<FieldTarget>),
    /// JSON, but no field shape this crate reads. Not noticed here: whether
    /// that is drift depends on the slot — a wrapper key inside a condition
    /// tree is expected, a projection's `field` is not.
    NotAField,
    /// A field shape whose required parts were missing or unreadable. Already
    /// noticed; the slot must stay empty.
    Malformed,
}

/// Sees through a `ScopedEval` wrapper (`{Expression, Scope}`) to the field it
/// evaluates; anything else passes through unchanged.
fn unwrap_scoped_eval(container: &Value) -> &Value {
    container
        .get("ScopedEval")
        .and_then(|scoped| scoped.get("Expression"))
        .unwrap_or(container)
}

/// Whether a field's `Expression` names a visual-calculation local — a
/// subquery output, a subquery transform table, or a named select expression —
/// rather than a model table. Such a field is no model reference: what keeps
/// model objects alive are the subquery's own `Select` columns, which the
/// structural walk binds, and the local name (e.g. a visual calculation) is
/// not a model object at all. Neither is drift, so neither may notice.
fn is_visual_calculation_source(expression: &Value) -> bool {
    ["Subquery", "SelectRef", "TransformTableRef"]
        .iter()
        .any(|key| expression.get(key).is_some())
}

/// Parses one field container into a [`FieldTarget`], resolving `SourceRef`
/// aliases against the enclosing query's `From` entries.
fn parse_field(container: &Value, aliases: &Aliases, ctx: &mut Ctx, location: &str) -> FieldParse {
    let Some(object) = container.as_object() else {
        return FieldParse::NotAField;
    };
    let Some(variant) = FIELD_VARIANTS.iter().find(|key| object.contains_key(**key)) else {
        return FieldParse::NotAField;
    };
    // Some shapes double the variant spelling — sparkline data wraps its
    // measure as `Measure: {Measure: {…}}` — so unwrap repeated wrappers
    // until the object stops being a lone field variant.
    let mut inner = &object[*variant];
    while let Some(wrapped) = inner.as_object() {
        let mut keys = wrapped.keys();
        match (keys.next(), keys.next()) {
            (Some(single), None) if FIELD_VARIANTS.contains(&single.as_str()) => {
                inner = &wrapped[single];
            }
            _ => break,
        }
    }
    match *variant {
        "Column" | "Measure" => column_or_measure(variant, inner, aliases, ctx, location),
        "Aggregation" => {
            // `Expression` is required by the schema; its absence means this
            // is not the semanticQuery shape at all (formatting wrappers reuse
            // these keys), so yield to the structural walk instead.
            let Some(expression) = inner.get("Expression") else {
                return FieldParse::NotAField;
            };
            let function = inner
                .get("Function")
                .and_then(Value::as_u64)
                .and_then(|code| AGGREGATION_FUNCTIONS.get(code as usize).copied());
            // Percent-range bounds in conditional formatting aggregate over
            // scoped evals of fields; the scope is evaluation detail, not a
            // reference, so the wrapper unwraps transparently.
            let expression = unwrap_scoped_eval(expression);
            match parse_field(
                expression,
                aliases,
                ctx,
                &format!("{location}/Aggregation/Expression"),
            ) {
                FieldParse::Target(inner) => {
                    FieldParse::Target(Box::new(FieldTarget::Aggregation {
                        function: function.map(str::to_string),
                        inner: Box::new(*inner),
                    }))
                }
                // A malformed inner was noticed while parsing it. An inner
                // that is no field shape at all is by design here — percent
                // ranges and visual-calculation sources end here — and the
                // structural walk has already seen everything inside it.
                FieldParse::Malformed => FieldParse::Malformed,
                FieldParse::NotAField => FieldParse::NotAField,
            }
        }
        "HierarchyLevel" => hierarchy_level(inner, aliases, ctx, location),
        // A whole hierarchy projected into a well has no dedicated
        // FieldTarget variant (only levels do); its table-qualified name is
        // kept as written so resolution still sees it.
        "Hierarchy" => {
            let Some(hierarchy) = inner.get("Hierarchy").and_then(Value::as_str) else {
                ctx.notice(
                    location,
                    SkipKind::MalformedValue,
                    "Hierarchy carries no hierarchy name",
                );
                return FieldParse::Malformed;
            };
            let table = inner.get("Expression").and_then(|source| {
                resolve_source(
                    source,
                    aliases,
                    ctx,
                    &format!("{location}/Hierarchy/Expression"),
                )
            });
            FieldParse::Target(Box::new(FieldTarget::Written(FieldRef {
                table,
                name: NameKey::new(hierarchy),
            })))
        }
        "Min" | "Max" | "Percentile" => {
            let Some(expression) = inner.get("Expression") else {
                // Not the semanticQuery shape (e.g. a `RangePercent` bound in
                // a conditional-formatting rule) — yield to the walk.
                return FieldParse::NotAField;
            };
            let expression = unwrap_scoped_eval(expression);
            match parse_field(
                expression,
                aliases,
                ctx,
                &format!("{location}/{variant}/Expression"),
            ) {
                FieldParse::Target(inner) => {
                    FieldParse::Target(Box::new(FieldTarget::Aggregation {
                        function: Some(variant.to_string()),
                        inner: Box::new(*inner),
                    }))
                }
                FieldParse::Malformed => FieldParse::Malformed,
                FieldParse::NotAField => FieldParse::NotAField,
            }
        }
        _ => FieldParse::NotAField,
    }
}

/// Parses a `Column` or `Measure` reference: a `Property` over a table source.
fn column_or_measure(
    variant: &str,
    inner: &Value,
    aliases: &Aliases,
    ctx: &mut Ctx,
    location: &str,
) -> FieldParse {
    let Some(property) = inner.get("Property").and_then(Value::as_str) else {
        ctx.notice(
            location,
            SkipKind::MalformedValue,
            format!("{variant} reference carries no Property"),
        );
        return FieldParse::Malformed;
    };
    let name = NameKey::new(property);
    match inner.get("Expression") {
        // A visual-calculation source is understood and no model reference.
        Some(expression) if is_visual_calculation_source(expression) => {
            return FieldParse::NotAField;
        }
        None => ctx.notice(
            location,
            SkipKind::MalformedValue,
            format!("{variant} reference carries no Expression"),
        ),
        _ => {}
    }
    match variant {
        // Measures are model-global, so the entity beside them is provenance:
        // an unreadable source still leaves a resolvable, table-less name.
        "Measure" => FieldParse::Target(Box::new(FieldTarget::Measure {
            home_table: inner.get("Expression").and_then(|source| {
                resolve_source(source, aliases, ctx, &format!("{location}/Expression"))
            }),
            measure: name,
        })),
        // A column without its table keeps the reference as written — a
        // binding we cannot fully read still binds.
        _ => {
            let table = inner.get("Expression").and_then(|source| {
                resolve_source(source, aliases, ctx, &format!("{location}/Expression"))
            });
            match table {
                Some(table) => FieldParse::Target(Box::new(FieldTarget::Column {
                    table,
                    column: name,
                })),
                None => FieldParse::Target(Box::new(FieldTarget::Written(FieldRef {
                    table: None,
                    name,
                }))),
            }
        }
    }
}

/// Parses a `HierarchyLevel` reference, which nests a whole `Hierarchy`
/// (`Expression` → `Hierarchy` container → table source) under its `Level`.
fn hierarchy_level(inner: &Value, aliases: &Aliases, ctx: &mut Ctx, location: &str) -> FieldParse {
    let Some(level) = inner.get("Level").and_then(Value::as_str) else {
        ctx.notice(
            location,
            SkipKind::MalformedValue,
            "HierarchyLevel carries no Level",
        );
        return FieldParse::Malformed;
    };
    let Some(hierarchy) = inner
        .get("Expression")
        .and_then(|expression| expression.get("Hierarchy"))
    else {
        ctx.notice(
            location,
            SkipKind::MalformedValue,
            "HierarchyLevel expression is not a Hierarchy",
        );
        return FieldParse::Malformed;
    };
    let Some(hierarchy_name) = hierarchy.get("Hierarchy").and_then(Value::as_str) else {
        ctx.notice(
            format!("{location}/Hierarchy"),
            SkipKind::MalformedValue,
            "Hierarchy carries no hierarchy name",
        );
        return FieldParse::Malformed;
    };
    let name = NameKey::new(hierarchy_name);
    // A hierarchy over a date variation sources the *varied* table through a
    // `PropertyVariationSource`: its `Property` names the varied column and its
    // `Name` the variation. Both are carried so resolution can join the
    // model-side `variation` declaration instead of guessing.
    let variation_source = hierarchy
        .get("Expression")
        .and_then(|expression| expression.get("PropertyVariationSource"));
    let via_column = variation_source
        .and_then(|variation| variation.get("Property"))
        .and_then(Value::as_str)
        .map(NameKey::new);
    let via_variation = variation_source
        .and_then(|variation| variation.get("Name"))
        .and_then(Value::as_str)
        .map(NameKey::new);
    let table = hierarchy.get("Expression").and_then(|source| {
        resolve_source(
            source,
            aliases,
            ctx,
            &format!("{location}/Hierarchy/Expression"),
        )
    });
    match table {
        Some(table) => FieldParse::Target(Box::new(FieldTarget::HierarchyLevel {
            table,
            hierarchy: name,
            level: NameKey::new(level),
            via_column,
            via_variation,
        })),
        // Losing the table would lose the level too, so fall back to the
        // hierarchy's name as written.
        None => FieldParse::Target(Box::new(FieldTarget::Written(FieldRef {
            table: None,
            name,
        }))),
    }
}

/// Resolves a field expression's table source: `SourceRef.Source` through the
/// query's `From` aliases, or `SourceRef.Entity` directly. `None` means the
/// source is unreadable — noticed before returning; callers keep what they
/// still can.
fn resolve_source(
    expression: &Value,
    aliases: &Aliases,
    ctx: &mut Ctx,
    location: &str,
) -> Option<NameKey> {
    let Some(source) = expression.get("SourceRef") else {
        // A hierarchy on a date variation sources its table through a
        // `PropertyVariationSource`, whose `Expression` is the base table.
        if let Some(variation) = expression.get("PropertyVariationSource") {
            return match variation.get("Expression") {
                Some(source) => resolve_source(source, aliases, ctx, location),
                None => {
                    ctx.notice(
                        location,
                        SkipKind::MalformedValue,
                        "PropertyVariationSource carries no Expression",
                    );
                    None
                }
            };
        }
        ctx.notice(
            location,
            SkipKind::MalformedValue,
            "field expression is not a table (SourceRef) reference",
        );
        return None;
    };
    if let Some(alias) = source.get("Source").and_then(Value::as_str) {
        return match aliases.get(&fold_name(alias)) {
            Some(entity) => Some(NameKey::new(entity)),
            None => {
                ctx.notice(
                    location,
                    SkipKind::UnresolvedAlias,
                    format!("query alias '{alias}' matches no From entry"),
                );
                None
            }
        };
    }
    match source.get("Entity").and_then(Value::as_str) {
        Some(entity) => Some(NameKey::new(entity)),
        None => {
            ctx.notice(
                location,
                SkipKind::MalformedValue,
                "SourceRef names neither a query alias nor an entity",
            );
            None
        }
    }
}

/// Parses a field container a modeled slot requires (a projection's `field`, a
/// filter's target, a drillthrough parameter's `fieldExpr`). JSON that is no
/// field shape at all is drift in such slots: noticed, and the slot stays
/// empty — a reference we cannot read must not silently vanish.
fn required_field(
    container: Option<&Value>,
    aliases: &Aliases,
    ctx: &mut Ctx,
    location: &str,
) -> Option<FieldTarget> {
    let container = container?;
    match parse_field(container, aliases, ctx, location) {
        FieldParse::Target(target) => Some(*target),
        FieldParse::Malformed => None,
        FieldParse::NotAField => {
            ctx.notice(
                location,
                SkipKind::MalformedValue,
                "value is not a readable field reference",
            );
            None
        }
    }
}

/// Collects every field reference inside an arbitrary JSON tree — filter
/// condition trees, conditional-formatting rules — wherever it appears.
///
/// The walk is structural, not schema-driven: known field containers are
/// extracted from any nesting (the condition wrappers are many — `In`,
/// `Comparison`, `And`, `Not`, `Between`, … — and grow across schema
/// versions), while `Literal` values are data and only ever end up recursed
/// into, never read as references.
fn collect_fields(
    value: &Value,
    aliases: &Aliases,
    ctx: &mut Ctx,
    location: &str,
    out: &mut Vec<FieldTarget>,
) {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_fields(item, aliases, ctx, &format!("{location}/{index}"), out);
            }
        }
        Value::Object(object) => {
            if let Some(query) = object.get("Subquery").and_then(|sub| sub.get("Query")) {
                // A visual-calculation subquery defines its own alias scope;
                // its Select, Where, and Transform trees carry the model
                // references — an AI narrative's Transform steps consume whole
                // tables of aliased fields.
                let aliases = query_aliases(query);
                let location = format!("{location}/Subquery/Query");
                for key in ["Select", "Where", "Transform"] {
                    if let Some(part) = query.get(key) {
                        collect_fields(part, &aliases, ctx, &format!("{location}/{key}"), out);
                    }
                }
                return;
            }
            if FIELD_VARIANTS.iter().any(|key| object.contains_key(*key)) {
                match parse_field(value, aliases, ctx, location) {
                    FieldParse::Target(target) => {
                        out.push(*target);
                        return;
                    }
                    // A malformed field was noticed while parsing it.
                    FieldParse::Malformed => return,
                    FieldParse::NotAField => {}
                }
            }
            for (key, child) in object {
                collect_fields(child, aliases, ctx, &format!("{location}/{key}"), out);
            }
        }
        _ => {}
    }
}

// --- Visual binding sites ----------------------------------------------------

/// Parses a visual query's `queryState`: role name → projections, plus the
/// report-local field parameters behind which the role's fields can swap.
/// Roles follow the JSON library's map order (alphabetical); projections keep
/// their file order.
fn wells(query_state: Option<&Value>, ctx: &mut Ctx, location: &str) -> Vec<FieldWell> {
    let Some(state) = query_state.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (role, role_state) in state {
        let role_location = format!("{location}/{role}");
        check_keys(role_state, &PROJECTION_STATE_KEYS, ctx, &role_location);
        let mut projections = Vec::new();
        if let Some(list) = role_state.get("projections").and_then(Value::as_array) {
            for (index, projection) in list.iter().enumerate() {
                let projection_location = format!("{role_location}/projections/{index}");
                check_keys(projection, &PROJECTION_KEYS, ctx, &projection_location);
                let field_location = format!("{projection_location}/field");
                let targets = match projection.get("field") {
                    Some(field) => {
                        match parse_field(field, &Aliases::new(), ctx, &field_location) {
                            FieldParse::Target(target) => vec![*target],
                            // The failure was already noticed inside parse_field.
                            FieldParse::Malformed => Vec::new(),
                            // A computed field is a container of references,
                            // not one reference: bind every model field it
                            // mentions, the first as the projection and the
                            // rest as inactive riders, like field parameters.
                            // A native visual calculation is DAX text, so its
                            // bracketed references come from the DAX lexer as
                            // written references the graph's ladder resolves.
                            FieldParse::NotAField => {
                                let mut found = Vec::new();
                                if let Some(expression) = field
                                    .get("NativeVisualCalculation")
                                    .and_then(|calc| calc.get("Expression"))
                                    .and_then(Value::as_str)
                                {
                                    for raw in dax::references(expression) {
                                        if let dax::RawRef::Field { table, name, .. } = raw {
                                            found.push(FieldTarget::Written(FieldRef {
                                                table: table.map(NameKey::new),
                                                name: NameKey::new(name),
                                            }));
                                        }
                                    }
                                } else if let Some(sparkline) = field.get("SparklineData") {
                                    // A sparkline binds its measured field and
                                    // every column it groups by.
                                    for part in ["Measure", "Groupings"] {
                                        if let Some(value) = sparkline.get(part) {
                                            collect_fields(
                                                value,
                                                &Aliases::new(),
                                                ctx,
                                                &format!("{field_location}/{part}"),
                                                &mut found,
                                            );
                                        }
                                    }
                                }
                                if found.is_empty() {
                                    collect_fields(
                                        field,
                                        &Aliases::new(),
                                        ctx,
                                        &field_location,
                                        &mut found,
                                    );
                                }
                                let mut unique: Vec<FieldTarget> = Vec::new();
                                for target in found {
                                    if !unique.contains(&target) {
                                        unique.push(target);
                                    }
                                }
                                if unique.is_empty() {
                                    ctx.notice(
                                        field_location.clone(),
                                        SkipKind::MalformedValue,
                                        "value is not a readable field reference",
                                    );
                                }
                                unique
                            }
                        }
                    }
                    None => Vec::new(),
                };
                if let Some((first, riders)) = targets.split_first() {
                    projections.push(Projection {
                        target: first.clone(),
                        query_ref: projection
                            .get("queryRef")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        active: projection
                            .get("active")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    });
                    for target in riders {
                        projections.push(Projection {
                            target: target.clone(),
                            query_ref: None,
                            active: false,
                        });
                    }
                }
            }
        }
        // A field parameter swaps the well's field behind a report-local
        // toggle; the columns it stands for bind like any other projection,
        // just never as the active one.
        if let Some(parameters) = role_state.get("fieldParameters").and_then(Value::as_array) {
            for (index, parameter) in parameters.iter().enumerate() {
                let parameter_location = format!("{role_location}/fieldParameters/{index}");
                if let Some(target) = required_field(
                    parameter.get("parameterExpr"),
                    &Aliases::new(),
                    ctx,
                    &format!("{parameter_location}/parameterExpr"),
                ) {
                    projections.push(Projection {
                        target,
                        query_ref: None,
                        active: false,
                    });
                }
            }
        }
        if !projections.is_empty() {
            out.push(FieldWell {
                role: role.clone(),
                projections,
            });
        }
    }
    out
}

/// Parses a visual query's `sortDefinition` into its sort-by fields.
fn sorts(query: &Value, ctx: &mut Ctx, location: &str) -> Vec<FieldTarget> {
    let mut out = Vec::new();
    let sort_definition = query.get("sortDefinition");
    if let Some(sort_definition) = sort_definition {
        check_keys(
            sort_definition,
            &SORT_KEYS,
            ctx,
            &format!("{location}/sortDefinition"),
        );
        if let Some(list) = sort_definition.get("sort").and_then(Value::as_array) {
            for (index, clause) in list.iter().enumerate() {
                let clause_location = format!("{location}/sortDefinition/sort/{index}");
                if let Some(target) = required_field(
                    clause.get("field"),
                    &Aliases::new(),
                    ctx,
                    &format!("{clause_location}/field"),
                ) {
                    out.push(target);
                }
            }
        }
    }
    out
}

/// Parses a visual's formatting `objects`. Two things hide in there:
///
/// - a `filter` property is a *persisted automatic filter* — the visual's own
///   filter, kept in the objects only after the filter pane has been expanded
///   in the report's authoring history. A `selfFilter` property is the same
///   filter in the shape a visual applies to its own data. Both bind the same
///   way: the condition tree's fields are references like any other, so both
///   join the visual's filters.
/// - every other property is conditional formatting, whose field references
///   (`FillRule` inputs and the like) are collected structurally, since the
///   property trees have shape only the schema knows.
fn visual_objects(
    objects: Option<&Value>,
    ctx: &mut Ctx,
    location: &str,
) -> (Vec<Filter>, Vec<FieldTarget>) {
    let mut filters = Vec::new();
    let mut conditional_formatting = Vec::new();
    let Some(map) = objects.and_then(Value::as_object) else {
        return (filters, conditional_formatting);
    };
    for (name, definitions) in map {
        let definitions_location = format!("{location}/{name}");
        let Some(list) = definitions.as_array() else {
            continue;
        };
        for (index, definition) in list.iter().enumerate() {
            let definition_location = format!("{definitions_location}/{index}");
            let Some(properties) = definition.get("properties").and_then(Value::as_object) else {
                continue;
            };
            for (property_name, property_value) in properties {
                let property_location = format!("{definition_location}/properties/{property_name}");
                let is_persisted_filter = matches!(property_name.as_str(), "filter" | "selfFilter")
                    && property_value
                        .get("filter")
                        .is_some_and(|definition| definition.get("Where").is_some());
                if is_persisted_filter {
                    filters.push(filter_entry(property_value, ctx, &property_location));
                } else {
                    collect_fields(
                        property_value,
                        &Aliases::new(),
                        ctx,
                        &property_location,
                        &mut conditional_formatting,
                    );
                }
            }
        }
    }
    (filters, conditional_formatting)
}

/// Reads a visual's accessibility alt text
/// (`visualContainerObjects.<group>[].properties.altText`). The canonical
/// spelling is `general`; the reference is collected wherever it hides,
/// because a screen reader reads the text — whatever model object it
/// interpolates stays alive.
fn alt_text_targets(objects: Option<&Value>, ctx: &mut Ctx, location: &str) -> Vec<FieldTarget> {
    let mut out = Vec::new();
    let Some(map) = objects.and_then(Value::as_object) else {
        return out;
    };
    for (group, definitions) in map {
        let Some(list) = definitions.as_array() else {
            continue;
        };
        for (index, definition) in list.iter().enumerate() {
            let Some(text) = definition
                .get("properties")
                .and_then(|properties| properties.get("altText"))
            else {
                continue;
            };
            collect_fields(
                text,
                &Aliases::new(),
                ctx,
                &format!("{location}/{group}/{index}/properties/altText"),
                &mut out,
            );
        }
    }
    out
}

/// Reads a visual's tooltip page reference
/// (`visualContainerObjects.visualTooltip[].properties.section`), whose value
/// is a literal with the page name in single quotes.
fn tooltip_page(objects: Option<&Value>, ctx: &mut Ctx, location: &str) -> Option<NameKey> {
    let objects = objects?;
    for key in ["visualTooltip", "visualHeaderTooltip"] {
        let Some(entries) = objects.get(key).and_then(Value::as_array) else {
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            let section_location = format!("{location}/{key}/{index}/properties/section");
            let Some(section) = entry
                .get("properties")
                .and_then(|properties| properties.get("section"))
            else {
                continue;
            };
            match section
                .get("expr")
                .and_then(|expr| expr.get("Literal"))
                .and_then(|literal| literal.get("Value"))
                .and_then(Value::as_str)
            {
                Some(page) => return Some(NameKey::new(unquote_literal(page))),
                None => ctx.notice(
                    section_location,
                    SkipKind::MalformedValue,
                    "tooltip section is not a literal page name",
                ),
            }
        }
    }
    None
}

// --- Key policies ------------------------------------------------------------

/// The key policy for one JSON object: `known` keys are parsed into the AST,
/// `ignored` keys are deliberately unmodeled (they cannot keep a model object
/// alive); anything else is unexpected drift and reported as a notice.
struct Keys {
    known: &'static [&'static str],
    ignored: &'static [&'static str],
}

/// Reports every key off both lists of `keys` as drift.
fn check_keys(value: &Value, keys: &Keys, ctx: &mut Ctx, location: &str) {
    let Some(object) = value.as_object() else {
        return;
    };
    for key in object.keys() {
        if !keys.known.contains(&key.as_str()) && !keys.ignored.contains(&key.as_str()) {
            ctx.notice(
                format!("{location}/{key}"),
                SkipKind::UnknownProperty,
                format!("unexpected property '{key}'"),
            );
        }
    }
}

/// `report.json` (display objects, themes, and settings are canvas chrome).
const REPORT_KEYS: Keys = Keys {
    known: &["$schema", "filterConfig"],
    ignored: &[
        "objects",
        "resourcePackages",
        "settings",
        "slowDataSourceSettings",
        "themeCollection",
    ],
};

/// `page.json`.
const PAGE_KEYS: Keys = Keys {
    known: &[
        "$schema",
        "name",
        "displayName",
        "filterConfig",
        "pageBinding",
        "visibility",
    ],
    ignored: &[
        "displayOption",
        "height",
        "objects",
        "type",
        "visualInteractions",
        "width",
    ],
};

/// `visual.json` (the visual container; position and grouping are layout).
const VISUAL_KEYS: Keys = Keys {
    known: &["$schema", "name", "visual", "filterConfig"],
    ignored: &[
        "howCreated",
        "isHidden",
        "parentGroupName",
        "position",
        "visualGroup",
    ],
};

/// `visual.json` under `visual` (the visual configuration).
const VISUAL_KEYS_INNER: Keys = Keys {
    known: &["visualType", "query", "objects", "visualContainerObjects"],
    ignored: &[
        "autoSelectVisualType",
        "drillFilterOtherVisuals",
        "expansionStates",
        "syncGroup",
    ],
};

/// `visual.json` under `visual.query`.
const QUERY_KEYS: Keys = Keys {
    known: &["queryState", "sortDefinition"],
    ignored: &["isDrillDisabled"],
};

/// `queryState` value, per role.
const PROJECTION_STATE_KEYS: Keys = Keys {
    known: &["projections", "fieldParameters"],
    ignored: &["showAll"],
};

/// One projection of a role.
const PROJECTION_KEYS: Keys = Keys {
    known: &["field", "queryRef"],
    ignored: &[
        "active",
        "displayName",
        "format",
        "hidden",
        "nativeQueryRef",
    ],
};

/// `query.sortDefinition`.
const SORT_KEYS: Keys = Keys {
    known: &["sort"],
    ignored: &["isDefaultSort"],
};

/// `page.json` under `pageBinding`.
const PAGE_BINDING_KEYS: Keys = Keys {
    known: &["name", "type", "parameters"],
    ignored: &["acceptsFilterContext"],
};

/// One `pageBinding.parameters[]` entry.
const PARAMETER_KEYS: Keys = Keys {
    known: &["name", "fieldExpr"],
    ignored: &["asAggregation", "boundFilter", "qnaSingleSelectRequired"],
};

/// A `filterConfig` object.
const FILTER_CONFIG_KEYS: Keys = Keys {
    known: &["filters"],
    ignored: &["filterSortOrder"],
};

/// One filter entry, at any scope and in bookmarks.
const FILTER_ENTRY_KEYS: Keys = Keys {
    known: &["name", "field", "expression", "filter"],
    ignored: &[
        "cachedDisplayNames",
        "displayName",
        "filterExpressionMetadata",
        "filterSortOrder",
        "howCreated",
        "isHiddenInViewMode",
        "isLockedInViewMode",
        "isTransient",
        "objects",
        "ordinal",
        "precedence",
        "restatement",
        "type",
    ],
};

/// A filter's `filter` definition (`semanticQuery` `FilterDefinition`).
const FILTER_DEFINITION_KEYS: Keys = Keys {
    known: &["From", "Where"],
    ignored: &["Annotations", "Version"],
};

/// One `Where` clause of a filter definition.
const QUERY_FILTER_KEYS: Keys = Keys {
    known: &["Condition", "Target"],
    ignored: &["Annotations"],
};

/// `*.bookmark.json`.
const BOOKMARK_KEYS: Keys = Keys {
    known: &["$schema", "name", "displayName", "explorationState"],
    ignored: &["options"],
};

/// `pages/pages.json`. Page order and the active page stay unmodeled (display
/// state), but `pageOrder` is read as one of the two authorities on which
/// pages exist that a bookmark section must clear (issue #48).
const PAGES_KEYS: Keys = Keys {
    known: &["$schema", "pageOrder"],
    ignored: &["activePageName", "landingPageName"],
};

/// `explorationState`. `activeSection` is read to diagnose stale state (the
/// deleted page the bookmark was captured on); it binds nothing itself.
const EXPLORATION_KEYS: Keys = Keys {
    known: &["filters", "sections", "activeSection"],
    ignored: &["dataSourceVariables", "objects", "version"],
};

/// One `explorationState.sections.<page>` value.
const SECTION_KEYS: Keys = Keys {
    known: &["filters", "visualContainers"],
    ignored: &["visualContainerGroups"],
};

/// One `visualContainers.<id>` value of a bookmark section.
const BOOKMARK_VISUAL_KEYS: Keys = Keys {
    known: &["filters", "singleVisual"],
    ignored: &["filterExpressionMetadata", "highlight"],
};

/// `visualContainers.<id>.singleVisual`. Saved formatting merges (`objects`),
/// sort state (`orderBy`), and highlight selections are deliberately
/// unmodeled — see the known gaps in `docs/formats.md`.
const SINGLE_VISUAL_KEYS: Keys = Keys {
    known: &["activeProjections", "projections"],
    ignored: &[
        "autoSelectVisualType",
        "cachedFilterDisplayItems",
        "display",
        "expansionStates",
        "filterExpressionMetadata",
        "isDrillDisabled",
        "objects",
        "orderBy",
        "parameters",
        "targetAutoSelectVisualType",
        "targetType",
        "visualType",
    ],
};

/// One `reportExtensions.json` measure (data type and presentation are
/// metadata; `references` is a redundant index over the expression).
const EXTENSION_MEASURE_KEYS: Keys = Keys {
    known: &["name", "expression", "formatString"],
    ignored: &[
        "annotations",
        "dataCategory",
        "dataType",
        "description",
        "displayFolder",
        "hidden",
        "measureTemplate",
        "references",
    ],
};

// --- Small utilities ---------------------------------------------------------

/// The file and accumulator every helper reports skips through.
struct Ctx<'a> {
    path: &'a Path,
    skips: &'a mut Vec<SkipNotice>,
}

impl Ctx<'_> {
    /// Records one skip at a JSON-pointer location in the file.
    fn notice(&mut self, location: impl Into<String>, kind: SkipKind, detail: impl Into<String>) {
        self.skips.push(SkipNotice {
            path: self.path.to_path_buf(),
            location: Some(location.into()),
            kind,
            detail: detail.into(),
        });
    }
}

/// Reads a required JSON document; I/O and parse errors fail the run.
fn read_json(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// Reads an optional JSON document: `None` when absent, a notice when it
/// exists but cannot be read or parsed.
fn read_optional(path: &Path, skips: &mut Vec<SkipNotice>) -> Option<Value> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            skips.push(SkipNotice {
                path: path.to_path_buf(),
                location: None,
                kind: SkipKind::MalformedValue,
                detail: format!("could not be read: {error}"),
            });
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            skips.push(SkipNotice {
                path: path.to_path_buf(),
                location: None,
                kind: SkipKind::MalformedValue,
                detail: format!("is not valid JSON: {error}"),
            });
            None
        }
    }
}

/// The immediate subdirectories of `path`, in no particular order.
fn child_folders(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

/// A path's final component as text, empty when it has none or is not UTF-8.
fn folder_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
}

/// A bookmark file's object name, from `Xyz.bookmark.json` → `Xyz`.
fn bookmark_name(path: &Path) -> &str {
    path.file_stem()
        .and_then(|name| name.to_str())
        .and_then(|stem| stem.strip_suffix(".bookmark"))
        .unwrap_or("")
}

/// Unquotes a PBIR literal value: surrounding single quotes off, doubled
/// internal quotes unescaped — the inverse of [`FieldRef`]'s `Display`.
fn unquote_literal(value: &str) -> String {
    match value.strip_prefix('\'') {
        Some(rest) if rest.ends_with('\'') => rest[..rest.len() - 1].replace("''", "'"),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// Parses a field container as written in `json`, with the given `From`
    /// aliases, returning the outcome and everything it noticed.
    fn parse_with_aliases(json: &str, aliases: Aliases) -> (FieldParse, Vec<SkipNotice>) {
        let value = serde_json::from_str(json).unwrap();
        let mut skips = Vec::new();
        let outcome = {
            let mut ctx = Ctx {
                path: Path::new("test/visual.json"),
                skips: &mut skips,
            };
            parse_field(&value, &aliases, &mut ctx, "/field")
        };
        (outcome, skips)
    }

    fn parse_field_json(json: &str) -> (FieldParse, Vec<SkipNotice>) {
        parse_with_aliases(json, Aliases::new())
    }

    fn direct_column() -> FieldTarget {
        FieldTarget::Column {
            table: NameKey::new("Product"),
            column: NameKey::new("Category"),
        }
    }

    mod column_and_measure {
        use super::*;

        #[test]
        fn a_column_resolves_its_entity_directly() {
            let (outcome, skips) = parse_field_json(
                r#"{"Column": {"Expression": {"SourceRef": {"Entity": "Product"}}, "Property": "Category"}}"#,
            );

            assert_eq!(outcome, FieldParse::Target(Box::new(direct_column())));
            assert!(skips.is_empty());
        }

        #[test]
        fn a_column_resolves_its_alias_through_from() {
            let (outcome, skips) = parse_with_aliases(
                r#"{"Column": {"Expression": {"SourceRef": {"Source": "p"}}, "Property": "Category"}}"#,
                Aliases::from([("p".to_string(), "Product".to_string())]),
            );

            assert_eq!(outcome, FieldParse::Target(Box::new(direct_column())));
            assert!(skips.is_empty());
        }

        #[test]
        fn alias_matching_is_case_insensitive() {
            let (outcome, skips) = parse_with_aliases(
                r#"{"Column": {"Expression": {"SourceRef": {"Source": "P"}}, "Property": "Category"}}"#,
                Aliases::from([("p".to_string(), "Product".to_string())]),
            );

            assert_eq!(outcome, FieldParse::Target(Box::new(direct_column())));
            assert!(skips.is_empty());
        }

        /// A lost table name must not lose the reference: it stays as written,
        /// and the loss is recorded so a human can judge it.
        #[test]
        fn an_unknown_alias_yields_a_written_reference_and_a_notice() {
            let (outcome, skips) = parse_field_json(
                r#"{"Column": {"Expression": {"SourceRef": {"Source": "x"}}, "Property": "Category"}}"#,
            );

            assert_eq!(
                outcome,
                FieldParse::Target(Box::new(FieldTarget::Written(FieldRef {
                    table: None,
                    name: NameKey::new("Category"),
                })))
            );
            assert_eq!(skips.len(), 1);
            assert_eq!(skips[0].kind, SkipKind::UnresolvedAlias);
        }

        #[test]
        fn a_column_without_property_is_malformed() {
            let (outcome, skips) = parse_field_json(
                r#"{"Column": {"Expression": {"SourceRef": {"Entity": "Product"}}}}"#,
            );

            assert_eq!(outcome, FieldParse::Malformed);
            assert_eq!(skips[0].kind, SkipKind::MalformedValue);
        }

        /// Measure names are model-global, so an unreadable table source still
        /// leaves a resolvable, table-less reference.
        #[test]
        fn a_measure_without_a_table_still_resolves_by_name() {
            let (outcome, skips) = parse_field_json(r#"{"Measure": {"Property": "Cost"}}"#);

            assert_eq!(
                outcome,
                FieldParse::Target(Box::new(FieldTarget::Measure {
                    home_table: None,
                    measure: NameKey::new("Cost"),
                }))
            );
            assert_eq!(skips[0].kind, SkipKind::MalformedValue);
        }

        #[test]
        fn a_measure_keeps_its_home_table() {
            let (outcome, skips) = parse_field_json(
                r#"{"Measure": {"Expression": {"SourceRef": {"Entity": "Sales"}}, "Property": "Cost"}}"#,
            );

            assert_eq!(
                outcome,
                FieldParse::Target(Box::new(FieldTarget::Measure {
                    home_table: Some(NameKey::new("Sales")),
                    measure: NameKey::new("Cost"),
                }))
            );
            assert!(skips.is_empty());
        }

        #[test]
        fn json_that_is_no_field_shape_is_not_a_field() {
            let (outcome, skips) = parse_field_json(r#"{"Literal": {"Value": "'Won'"}}"#);

            assert_eq!(outcome, FieldParse::NotAField);
            assert!(skips.is_empty());
        }
    }

    mod aggregations {
        use super::*;

        #[test]
        fn an_aggregation_wraps_its_inner_reference() {
            let (outcome, skips) = parse_field_json(
                r#"{"Aggregation": {"Expression": {"Column": {"Expression": {"SourceRef": {"Entity": "Sales"}}, "Property": "Units"}}, "Function": 0}}"#,
            );

            assert_eq!(
                outcome,
                FieldParse::Target(Box::new(FieldTarget::Aggregation {
                    function: Some("Sum".to_string()),
                    inner: Box::new(FieldTarget::Column {
                        table: NameKey::new("Sales"),
                        column: NameKey::new("Units"),
                    }),
                }))
            );
            assert!(skips.is_empty());
        }

        /// The code is diagnostics only; an unknown code must not lose the
        /// inner reference.
        #[test]
        fn an_unknown_function_code_keeps_the_inner_reference() {
            let (outcome, skips) = parse_field_json(
                r#"{"Aggregation": {"Expression": {"Column": {"Expression": {"SourceRef": {"Entity": "Sales"}}, "Property": "Units"}}, "Function": 99}}"#,
            );

            assert!(matches!(
                outcome,
                FieldParse::Target(target)
                    if matches!(*target, FieldTarget::Aggregation { function: None, .. })
            ));
            assert!(skips.is_empty());
        }

        #[test]
        fn min_max_and_percentile_read_as_aggregations() {
            for (json, function) in [
                (
                    r#"{"Min": {"Expression": {"Column": {"Expression": {"SourceRef": {"Entity": "Sales"}}, "Property": "Units"}}, "IncludeAllTypes": true}}"#,
                    "Min",
                ),
                (
                    r#"{"Max": {"Expression": {"Column": {"Expression": {"SourceRef": {"Entity": "Sales"}}, "Property": "Units"}}, "IncludeAllTypes": true}}"#,
                    "Max",
                ),
                (
                    r#"{"Percentile": {"Expression": {"Column": {"Expression": {"SourceRef": {"Entity": "Sales"}}, "Property": "Units"}}, "K": 90}}"#,
                    "Percentile",
                ),
            ] {
                let (outcome, skips) = parse_field_json(json);
                assert!(matches!(
                    outcome,
                    FieldParse::Target(ref target) if matches!(**target, FieldTarget::Aggregation { .. })
                ));
                if let FieldParse::Target(target) = outcome
                    && let FieldTarget::Aggregation { function: name, .. } = *target
                {
                    assert_eq!(name.as_deref(), Some(function));
                }
                assert!(skips.is_empty());
            }
        }
    }

    mod hierarchies {
        use super::*;

        #[test]
        fn a_hierarchy_level_resolves_table_hierarchy_and_level() {
            let (outcome, skips) = parse_field_json(
                r#"{"HierarchyLevel": {"Expression": {"Hierarchy": {"Expression": {"SourceRef": {"Entity": "Date"}}, "Hierarchy": "Calendar"}}, "Level": "Year"}}"#,
            );

            assert_eq!(
                outcome,
                FieldParse::Target(Box::new(FieldTarget::HierarchyLevel {
                    table: NameKey::new("Date"),
                    hierarchy: NameKey::new("Calendar"),
                    level: NameKey::new("Year"),
                    via_column: None,
                    via_variation: None,
                }))
            );
            assert!(skips.is_empty());
        }

        /// A hierarchy over a date variation carries the varied column and the
        /// variation's name — the join keys for the model-side `variation`
        /// declaration — while the written table stays the varied (base) one.
        #[test]
        fn a_hierarchy_over_a_property_variation_carries_the_variation_link() {
            let (outcome, skips) = parse_field_json(
                r#"{"HierarchyLevel": {"Expression": {"Hierarchy": {"Expression": {"PropertyVariationSource": {"Expression": {"SourceRef": {"Entity": "Opportunity Calendar"}}, "Name": "Variation", "Property": "Date"}}, "Hierarchy": "Date Hierarchy"}}, "Level": "Year"}}"#,
            );

            assert_eq!(
                outcome,
                FieldParse::Target(Box::new(FieldTarget::HierarchyLevel {
                    table: NameKey::new("Opportunity Calendar"),
                    hierarchy: NameKey::new("Date Hierarchy"),
                    level: NameKey::new("Year"),
                    via_column: Some(NameKey::new("Date")),
                    via_variation: Some(NameKey::new("Variation")),
                }))
            );
            assert!(skips.is_empty(), "the engine's own shape is not drift");
        }

        /// A whole hierarchy has no dedicated variant; its table-qualified
        /// name is kept as written so resolution still sees it.
        #[test]
        fn a_bare_hierarchy_is_kept_as_written() {
            let (outcome, skips) = parse_field_json(
                r#"{"Hierarchy": {"Expression": {"SourceRef": {"Entity": "Date"}}, "Hierarchy": "Calendar"}}"#,
            );

            assert_eq!(
                outcome,
                FieldParse::Target(Box::new(FieldTarget::Written(FieldRef {
                    table: Some(NameKey::new("Date")),
                    name: NameKey::new("Calendar"),
                })))
            );
            assert!(skips.is_empty());
        }
    }

    mod condition_trees {
        use super::*;

        /// Condition trees nest fields under many wrapper kinds; the walk is
        /// structural, so a wrapper the schema added last month still yields
        /// its fields. Literal values are data and must never surface.
        #[test]
        fn fields_are_found_wherever_they_nest_and_literals_never_surface() {
            let value = serde_json::from_str(
                r#"{"In": {
                    "Expressions": [{"Column": {"Expression": {"SourceRef": {"Source": "o"}}, "Property": "Status"}}],
                    "Values": [[{"Literal": {"Value": "'Open'"}}], [{"Literal": {"Value": "'Won'"}}]]
                }}"#,
            )
            .unwrap();
            let mut out = Vec::new();
            let mut skips = Vec::new();
            let mut ctx = Ctx {
                path: Path::new("test/bookmark.json"),
                skips: &mut skips,
            };
            collect_fields(
                &value,
                &Aliases::from([("o".to_string(), "Opportunities".to_string())]),
                &mut ctx,
                "/filter/Where/0/Condition",
                &mut out,
            );

            assert_eq!(
                out,
                vec![FieldTarget::Column {
                    table: NameKey::new("Opportunities"),
                    column: NameKey::new("Status"),
                }]
            );
            assert!(skips.is_empty());
        }
    }

    mod literals {
        use super::*;

        #[rstest]
        #[case::quoted("'Top'", "Top")]
        #[case::internal_quote_doubled("'Sales''s Data'", "Sales's Data")]
        #[case::unquoted("Top", "Top")]
        fn unquote_literal_strips_quotes(#[case] input: &str, #[case] expected: &str) {
            assert_eq!(unquote_literal(input), expected);
        }
    }

    /// A visual-calculation subquery: the Select carries the queryRef, but an
    /// AI narrative's Transform steps also consume aliased model fields —
    /// a shape a real textbox ships (`HighPointLowPointSummary`).
    mod subquery {
        use super::*;

        fn collect(json: &str) -> Vec<FieldTarget> {
            let value = serde_json::from_str(json).unwrap();
            let mut skips = Vec::new();
            let mut ctx = Ctx {
                path: Path::new("test/visual.json"),
                skips: &mut skips,
            };
            let mut out = Vec::new();
            collect_fields(&value, &Aliases::new(), &mut ctx, "/field", &mut out);
            assert!(skips.is_empty(), "no notices: {skips:?}");
            out
        }

        #[test]
        fn the_subquery_alias_scope_covers_its_transform_steps() {
            let out = collect(
                r#"{
                  "Subquery": {
                    "Query": {
                      "Version": 2,
                      "From": [
                        {"Name": "o", "Entity": "Opportunity Calendar", "Type": 0}
                      ],
                      "Select": [
                        {"QueryRef": "x", "Expression": {"Measure": {
                          "Expression": {"SourceRef": {"Source": "o"}},
                          "Property": "Revenue Won"}}}
                      ],
                      "Transform": [
                        {
                          "Name": "HighAndLowPoint",
                          "Algorithm": "HighPointLowPointSummary",
                          "Input": {
                            "Parameters": [
                              {
                                "Literal": {"Value": "10000L"},
                                "Name": "initialTemplateId"
                              }
                            ],
                            "Table": {
                              "Name": "HighAndLowPointInput",
                              "Columns": [
                                {
                                  "Expression": {"Column": {
                                    "Expression": {"SourceRef": {"Source": "o"}},
                                    "Property": "YEAR MONTH"}},
                                  "Name": "d2",
                                  "Role": "PrimarySeriesColumnRole"
                                }
                              ]
                            }
                          }
                        }
                      ]
                    }
                  }
                }"#,
            );

            assert_eq!(
                out,
                [
                    FieldTarget::Measure {
                        home_table: Some(NameKey::new("Opportunity Calendar")),
                        measure: NameKey::new("Revenue Won"),
                    },
                    FieldTarget::Column {
                        table: NameKey::new("Opportunity Calendar"),
                        column: NameKey::new("YEAR MONTH"),
                    },
                ]
            );
        }
    }

    /// A well projection whose field is a *computed* expression — the real
    /// shape: a percent-of-total ratio (`measure ÷ the same measure scoped to
    /// the Columns role`). It is a container of references, not one reference,
    /// so the well binds what it mentions instead of dropping the projection.
    mod computed_projections {
        use super::*;

        fn parse_wells(json: &str) -> (Vec<FieldWell>, Vec<SkipNotice>) {
            let value = serde_json::from_str(json).unwrap();
            let mut skips = Vec::new();
            let mut ctx = Ctx {
                path: Path::new("test/visual.json"),
                skips: &mut skips,
            };
            let out = wells(Some(&value), &mut ctx, "/visual/query/queryState");
            (out, skips)
        }

        fn measure(table: &str, name: &str) -> FieldTarget {
            FieldTarget::Measure {
                home_table: Some(NameKey::new(table)),
                measure: NameKey::new(name),
            }
        }

        #[test]
        fn an_arithmetic_projection_binds_its_operands_once() {
            let (wells_out, skips) = parse_wells(
                r#"{"Values": {"projections": [
                  {"queryRef": "x", "active": true, "field": {
                    "Arithmetic": {
                      "Left": {
                        "Measure": {
                          "Expression": {"SourceRef": {"Entity": "Sales"}},
                          "Property": "Revenue"}},
                      "Right": {
                        "ScopedEval": {
                          "Expression": {
                            "Measure": {
                              "Expression": {"SourceRef": {"Entity": "Sales"}},
                              "Property": "Revenue"}},
                          "Scope": [{"RoleRef": {"Role": "Columns"}}]}},
                      "Operator": 3}}}
                ]}}"#,
            );

            assert!(skips.is_empty(), "no notices: {skips:?}");
            assert_eq!(wells_out.len(), 1);
            assert_eq!(
                wells_out[0].projections,
                [Projection {
                    target: measure("Sales", "Revenue"),
                    query_ref: Some("x".to_string()),
                    active: true,
                }]
            );
        }

        #[test]
        fn a_sparkline_binds_its_measure_and_groupings() {
            let (wells_out, skips) = parse_wells(
                r#"{"Values": {"projections": [
                    {"field": {"SparklineData": {
                        "Measure": {"Measure": {
                            "Expression": {"SourceRef": {"Entity": "Sales"}},
                            "Property": "Revenue"}},
                        "Groupings": [{"Column": {
                            "Expression": {"SourceRef": {"Entity": "Calendar"}},
                            "Property": "MonthNumber"}}]}}}
                ]}}"#,
            );

            assert!(skips.is_empty(), "no notices: {skips:?}");
            assert_eq!(wells_out.len(), 1);
            assert_eq!(
                wells_out[0].projections,
                [
                    Projection {
                        target: measure("Sales", "Revenue"),
                        query_ref: None,
                        active: false,
                    },
                    Projection {
                        target: FieldTarget::Column {
                            table: NameKey::new("Calendar"),
                            column: NameKey::new("MonthNumber"),
                        },
                        query_ref: None,
                        active: false,
                    },
                ]
            );
        }

        #[test]
        fn a_projection_that_mentions_no_field_still_notices() {
            let (wells_out, skips) = parse_wells(
                r#"{"Values": {"projections": [
                    {"field": {"Literal": {"Value": "'just text'"}}}
                ]}}"#,
            );

            assert!(wells_out.is_empty());
            assert_eq!(skips.len(), 1);
            assert_eq!(skips[0].kind, SkipKind::MalformedValue);
        }

        /// A native visual calculation is DAX over the visual's own fields;
        /// its bracketed references become written targets the graph's ladder
        /// resolves, as inactive riders (the calculation itself is visual-
        /// level, so it carries no active model field of its own).
        #[test]
        fn a_native_visual_calculation_binds_its_dax_references() {
            let (wells_out, skips) = parse_wells(
                r#"{"Values": {"projections": [
                    {"field": {"NativeVisualCalculation": {
                        "Language": "dax",
                        "Expression": "EXPAND(AVERAGE([Revenue]), [Week])",
                        "Name": "Avg"}}}
                ]}}"#,
            );

            assert!(skips.is_empty(), "no notices: {skips:?}");
            assert_eq!(wells_out.len(), 1);
            assert_eq!(
                wells_out[0].projections,
                [
                    Projection {
                        target: FieldTarget::Written(FieldRef {
                            table: None,
                            name: NameKey::new("Revenue"),
                        }),
                        query_ref: None,
                        active: false,
                    },
                    Projection {
                        target: FieldTarget::Written(FieldRef {
                            table: None,
                            name: NameKey::new("Week"),
                        }),
                        query_ref: None,
                        active: false,
                    },
                ]
            );
        }
    }

    /// A `selfFilter` property is the visual's own persisted filter in the
    /// same `{filter: {From, Where}}` wrapper as the filter pane's; its
    /// condition tree must resolve through its own alias scope.
    mod self_filter {
        use super::*;

        #[test]
        fn a_self_filter_binds_through_its_own_aliases() {
            let value = serde_json::from_str::<Value>(
                r#"{"general": [{"properties": {"selfFilter": {"filter": {
                  "Version": 2,
                  "From": [{"Name": "c", "Entity": "Customers", "Type": 0}],
                  "Where": [{"Condition": {"Contains": {
                    "Left": {"Column": {"Expression": {"SourceRef": {"Source": "c"}}, "Property": "Name"}},
                    "Right": {"Literal": {"Value": "'west'"}}}}}]}}}}]}"#,
            )
            .unwrap();
            let mut skips = Vec::new();
            let mut ctx = Ctx {
                path: Path::new("test/visual.json"),
                skips: &mut skips,
            };
            let (filters, conditional_formatting) =
                visual_objects(Some(&value), &mut ctx, "/visual/objects");

            assert!(skips.is_empty(), "no notices: {skips:?}");
            assert!(conditional_formatting.is_empty());
            assert_eq!(filters.len(), 1);
            assert_eq!(filters[0].target, None);
            assert_eq!(
                filters[0].references,
                [FieldTarget::Column {
                    table: NameKey::new("Customers"),
                    column: NameKey::new("Name"),
                }]
            );
        }
    }

    /// Deleted pages leave their sections inside bookmarks forever (issue
    /// #48): the stale-section skip and the page-source reconciliation.
    mod stale_state {
        use super::*;

        /// A filter expression naming `'Product'[Color]`, valid at any scope.
        fn color_filter(name: &str) -> String {
            format!(
                r#"{{"name": "{name}", "type": "Categorical", "expression": {{
                    "Column": {{
                        "Expression": {{"SourceRef": {{"Entity": "Product"}}}},
                        "Property": "Color"
                    }}
                }}}}"#
            )
        }

        /// A bookmark with a report-level filter and one section per given
        /// page name, anchored on `active`.
        fn bookmark_json(active: &str, pages: &[&str]) -> Value {
            let section_filters: Vec<String> = pages
                .iter()
                .map(|page| {
                    format!(
                        r#""{page}": {{"filters": {{"byExpr": [{}]}}}}"#,
                        color_filter("SectionFilter")
                    )
                })
                .collect();
            serde_json::from_str(&format!(
                r#"{{
                    "name": "B1",
                    "explorationState": {{
                        "activeSection": "{active}",
                        "filters": {{"byExpr": [{}]}},
                        "sections": {{{}}}
                    }}
                }}"#,
                color_filter("ReportFilter"),
                section_filters.join(", ")
            ))
            .unwrap()
        }

        fn parse_bookmark(json: &Value, live: &[&str]) -> (Bookmark, Vec<SkipNotice>) {
            let live: HashSet<String> = live.iter().map(|name| fold_name(name)).collect();
            let mut skips = Vec::new();
            let parsed = {
                let mut ctx = Ctx {
                    path: Path::new("test/B1.bookmark.json"),
                    skips: &mut skips,
                };
                bookmark_file(json, "B1", &live, &mut ctx)
            };
            (parsed, skips)
        }

        #[test]
        fn a_stale_section_is_skipped_whole_with_a_notice() {
            let (bookmark, skips) =
                parse_bookmark(&bookmark_json("PLive", &["PLive", "PGone"]), &["PLive"]);

            // The live section survives; the stale one is gone whole.
            assert_eq!(bookmark.sections.len(), 1);
            assert_eq!(bookmark.sections[0].page.as_str(), "PLive");
            assert_eq!(bookmark.sections[0].filters.len(), 1);
            // The report-level saved filter is untouched by its section.
            assert_eq!(bookmark.filters.len(), 1);

            assert_eq!(skips.len(), 1, "one notice: {skips:?}");
            assert_eq!(skips[0].kind, SkipKind::StaleState);
            assert_eq!(
                skips[0].location.as_deref(),
                Some("/explorationState/sections/PGone")
            );
            assert!(skips[0].detail.contains("PGone"));
            assert!(skips[0].detail.contains("no longer defines"));
        }

        /// The section skip is the notice — the active section that coincides
        /// with it must not fire a second one.
        #[test]
        fn a_stale_active_section_folds_into_its_sections_notice() {
            let (_, skips) = parse_bookmark(&bookmark_json("PGone", &["PGone"]), &[]);

            assert_eq!(skips.len(), 1, "exactly the section notice: {skips:?}");
            assert_eq!(
                skips[0].location.as_deref(),
                Some("/explorationState/sections/PGone")
            );
            assert!(skips[0].detail.contains("active section"));
        }

        /// An active section with no `sections` entry binds nothing, but the
        /// deleted page it anchors on is still worth reporting.
        #[test]
        fn a_stale_active_section_without_a_section_gets_its_own_notice() {
            let (_, skips) = parse_bookmark(&bookmark_json("PGone", &["PLive"]), &["PLive"]);

            assert_eq!(skips.len(), 1, "exactly the anchor notice: {skips:?}");
            assert_eq!(
                skips[0].location.as_deref(),
                Some("/explorationState/activeSection")
            );
        }

        #[test]
        fn a_fully_live_bookmark_parses_unchanged() {
            let (bookmark, skips) =
                parse_bookmark(&bookmark_json("PLive", &["PLive"]), &["Pother", "PLive"]);

            assert_eq!(bookmark.sections.len(), 1);
            assert_eq!(bookmark.sections[0].filters.len(), 1);
            assert_eq!(bookmark.filters.len(), 1);
            assert!(skips.is_empty(), "no notices: {skips:?}");
        }

        /// The common case behind issue #48: sections are matched
        /// case-insensitively like every object name.
        #[test]
        fn section_ids_match_case_insensitively() {
            let (bookmark, skips) = parse_bookmark(&bookmark_json("plive", &["PLIVE"]), &["plive"]);

            assert_eq!(bookmark.sections.len(), 1);
            assert!(skips.is_empty(), "no notices: {skips:?}");
        }

        /// Reconciles two page-name sources, returning (set, notices).
        fn reconcile(order: &[&str], folders: &[&str]) -> (HashSet<String>, Vec<SkipNotice>) {
            reconcile_with(order, folders, true)
        }

        fn reconcile_with(
            order: &[&str],
            folders: &[&str],
            order_known: bool,
        ) -> (HashSet<String>, Vec<SkipNotice>) {
            let order: Vec<String> = order.iter().map(|s| s.to_string()).collect();
            let folders: Vec<String> = folders.iter().map(|s| s.to_string()).collect();
            let mut skips = Vec::new();
            let set = {
                let mut ctx = Ctx {
                    path: Path::new("test/pages.json"),
                    skips: &mut skips,
                };
                reconcile_pages(&order, &folders, order_known, &mut ctx)
            };
            (set, skips)
        }

        #[test]
        fn agreeing_page_sources_union_without_notices() {
            let (set, skips) = reconcile(&["P1", "P2"], &["P2", "P1"]);

            assert_eq!(set.len(), 2);
            assert!(set.contains(&fold_name("P1")));
            assert!(skips.is_empty(), "no notices: {skips:?}");
        }

        /// Either source alone proves a page exists, so the union keeps it —
        /// but the disagreement is reported at the pageOrder side.
        #[test]
        fn a_page_in_page_order_only_is_live_but_noticed() {
            let (set, skips) = reconcile(&["P1", "P2"], &["P1"]);

            assert!(set.contains(&fold_name("P2")));
            assert_eq!(skips.len(), 1, "one notice: {skips:?}");
            assert_eq!(skips[0].kind, SkipKind::StaleState);
            assert_eq!(skips[0].location.as_deref(), Some("/pageOrder/1"));
            assert!(skips[0].detail.contains("P2"));
        }

        #[test]
        fn a_page_with_a_folder_only_is_live_but_noticed() {
            let (set, skips) = reconcile(&["P1"], &["P1", "P2"]);

            assert!(set.contains(&fold_name("P2")));
            assert_eq!(skips.len(), 1, "one notice: {skips:?}");
            assert_eq!(skips[0].kind, SkipKind::StaleState);
            assert_eq!(skips[0].location.as_deref(), Some("/pageOrder"));
            assert!(skips[0].detail.contains("P2"));
        }

        #[test]
        fn a_page_missing_from_neither_source_is_never_in_the_set() {
            let (set, skips) = reconcile(&["P1"], &["P1"]);

            assert!(!set.contains(&fold_name("PGone")));
            assert!(skips.is_empty());
        }

        /// Without a readable `pageOrder` there is nothing to disagree with:
        /// the folders stand alone, silently.
        #[test]
        fn an_unknown_page_order_union_the_folders_without_notices() {
            let (set, skips) = reconcile_with(&[], &["P1", "P2"], false);

            assert_eq!(set.len(), 2);
            assert!(skips.is_empty(), "no notices: {skips:?}");
        }
    }
}
