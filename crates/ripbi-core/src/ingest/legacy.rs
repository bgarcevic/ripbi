//! Legacy UTF-16 `Report/Layout` ingestion. References remain written names.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::Result;
use crate::dax::{self, RawRef};
use crate::identity::{FieldRef, NameKey};
use crate::ingest::{SkipKind, SkipNotice};
use crate::report::{
    Bookmark, BookmarkSection, BookmarkVisual, DatasetReference, FieldTarget, FieldWell, Filter,
    Page, Projection, ReportModel, Visual,
};

pub(super) fn load(bytes: &[u8], path: &Path, skips: &mut Vec<SkipNotice>) -> Result<ReportModel> {
    let text = super::decode_json_text(bytes, "Report/Layout")?;
    let root: Value = serde_json::from_str(&text)?;
    let mut report = ReportModel {
        name: path
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned()),
        dataset: DatasetReference::Unresolved,
        filters: filters(root.get("filters"), path, "/filters", skips),
        ..Default::default()
    };
    if let Some(sections) = root.get("sections").and_then(Value::as_array) {
        for (index, section) in sections.iter().enumerate() {
            let page_name = section
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("section_{index}"));
            let mut page = Page {
                name: NameKey::new(&page_name),
                display_name: string(section, "displayName"),
                is_hidden: section.get("visibility").and_then(Value::as_u64) == Some(1),
                filters: filters(
                    section.get("filters"),
                    path,
                    &format!("/sections/{index}/filters"),
                    skips,
                ),
                binding: None,
                visuals: Vec::new(),
            };
            if let Some(containers) = section.get("visualContainers").and_then(Value::as_array) {
                for (visual_index, container) in containers.iter().enumerate() {
                    page.visuals
                        .push(visual(container, path, index, visual_index, skips));
                }
            }
            report.pages.push(page);
        }
    }
    let live_pages: HashSet<NameKey> = report.pages.iter().map(|page| page.name.clone()).collect();
    if let Some(bookmarks) = root.get("bookmarks").and_then(Value::as_array) {
        for (index, entry) in bookmarks.iter().enumerate() {
            let Some(state) = blob(Some(entry), path, &format!("/bookmarks/{index}"), skips) else {
                continue;
            };
            let mut bookmark = Bookmark {
                name: NameKey::new(
                    state
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("bookmark"),
                ),
                display_name: string(&state, "displayName"),
                filters: filters(
                    state.get("filters"),
                    path,
                    &format!("/bookmarks/{index}/filters"),
                    skips,
                ),
                sections: Vec::new(),
                stale_sections: Vec::new(),
            };
            if let Some(sections) = state.get("sections").and_then(Value::as_array) {
                for section in sections {
                    let Some(name) = section.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let saved = BookmarkSection {
                        page: NameKey::new(name),
                        filters: filters(
                            section.get("filters"),
                            path,
                            "/bookmarks/sections/filters",
                            skips,
                        ),
                        visuals: section
                            .get("visualContainers")
                            .and_then(Value::as_array)
                            .map(|visuals| {
                                visuals
                                    .iter()
                                    .enumerate()
                                    .map(|(index, item)| {
                                        let parsed = visual(item, path, 0, index, skips);
                                        BookmarkVisual {
                                            visual: parsed.name,
                                            wells: parsed.wells,
                                            filters: parsed.filters,
                                        }
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    };
                    if live_pages.contains(&saved.page) {
                        bookmark.sections.push(saved);
                    } else {
                        bookmark.stale_sections.push(saved);
                    }
                }
            }
            report.bookmarks.push(bookmark);
        }
    }
    Ok(report)
}

fn visual(
    container: &Value,
    path: &Path,
    page_index: usize,
    index: usize,
    skips: &mut Vec<SkipNotice>,
) -> Visual {
    let location = format!("/sections/{page_index}/visualContainers/{index}");
    let config = blob(
        container.get("config"),
        path,
        &format!("{location}/config"),
        skips,
    )
    .unwrap_or(Value::Null);
    let inner = config.get("singleVisual").unwrap_or(&Value::Null);
    let query = blob(
        container.get("query"),
        path,
        &format!("{location}/query"),
        skips,
    )
    .unwrap_or(Value::Null);
    let transforms = blob(
        container.get("dataTransforms"),
        path,
        &format!("{location}/dataTransforms"),
        skips,
    )
    .unwrap_or(Value::Null);
    let name = container
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| config.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| format!("visual_{index}"));
    let mut source_aliases = aliases(inner.get("prototypeQuery"));
    if source_aliases.is_empty() {
        let nested = query.pointer("/Commands/0/SemanticQueryDataShapeCommand/Query");
        source_aliases = aliases(nested);
    }
    let mut targets = Vec::new();
    collect_targets(&config, &source_aliases, &mut targets);
    collect_targets(&query, &source_aliases, &mut targets);
    collect_targets(&transforms, &source_aliases, &mut targets);
    collect_visual_calculations(container, &mut targets);
    collect_visual_calculations(&config, &mut targets);
    collect_visual_calculations(&query, &mut targets);
    let mut unique = HashSet::new();
    targets.retain(|target| unique.insert(target.clone()));
    let wells = vec![FieldWell {
        role: "Values".to_string(),
        projections: targets
            .into_iter()
            .map(|target| Projection {
                target,
                query_ref: None,
                active: true,
            })
            .collect(),
    }];
    let filters = filters(
        container.get("filters"),
        path,
        &format!("{location}/filters"),
        skips,
    );
    let mut sorts = Vec::new();
    if let Some(order) = inner.get("prototypeQuery").and_then(|q| q.get("OrderBy")) {
        collect_targets(order, &source_aliases, &mut sorts);
    }
    let mut conditional_formatting = Vec::new();
    if let Some(objects) = inner.get("objects") {
        collect_targets(objects, &source_aliases, &mut conditional_formatting);
    }
    let visual_type = inner
        .get("visualType")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if !config.is_null() && !known_visual_type(visual_type) {
        skips.push(SkipNotice {
            path: path.to_path_buf(),
            location: Some(format!("{location}/config/singleVisual/visualType")),
            kind: SkipKind::UnknownObject,
            detail: format!(
                "unrecognized legacy visual type '{visual_type}'; field references were retained"
            ),
        });
    }
    Visual {
        name: NameKey::new(&name),
        visual_type: visual_type.to_string(),
        wells,
        filters,
        sorts,
        conditional_formatting,
        alt_text: Vec::new(),
        tooltip_page: None,
    }
}

fn known_visual_type(name: &str) -> bool {
    matches!(
        name,
        "areaChart"
            | "barChart"
            | "card"
            | "clusteredBarChart"
            | "clusteredColumnChart"
            | "columnChart"
            | "comboChart"
            | "decompositionTreeVisual"
            | "donutChart"
            | "filledMap"
            | "funnel"
            | "gauge"
            | "image"
            | "lineChart"
            | "lineClusteredColumnComboChart"
            | "lineStackedColumnComboChart"
            | "map"
            | "matrix"
            | "multiRowCard"
            | "pieChart"
            | "ribbonChart"
            | "scatterChart"
            | "slicer"
            | "stackedAreaChart"
            | "stackedBarChart"
            | "stackedColumnChart"
            | "table"
            | "tableEx"
            | "textbox"
            | "treemap"
            | "waterfallChart"
    )
}

fn filters(
    value: Option<&Value>,
    path: &Path,
    location: &str,
    skips: &mut Vec<SkipNotice>,
) -> Vec<Filter> {
    let Some(value) = blob(value, path, location, skips) else {
        return Vec::new();
    };
    let entries = value
        .as_array()
        .or_else(|| value.get("filters").and_then(Value::as_array));
    let Some(entries) = entries else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|entry| {
            let mut refs = Vec::new();
            collect_targets(entry, &HashMap::new(), &mut refs);
            let target = if refs.is_empty() {
                None
            } else {
                Some(refs.remove(0))
            };
            Filter {
                name: string(entry, "name").map(NameKey::new),
                target,
                references: refs,
                ..Default::default()
            }
        })
        .collect()
}

fn blob(
    value: Option<&Value>,
    path: &Path,
    location: &str,
    skips: &mut Vec<SkipNotice>,
) -> Option<Value> {
    let value = value?;
    if let Some(text) = value.as_str() {
        match serde_json::from_str(text) {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                skips.push(SkipNotice {
                    path: path.to_path_buf(),
                    location: Some(location.to_string()),
                    kind: SkipKind::MalformedValue,
                    detail: format!("stringified JSON could not be parsed: {error}"),
                });
                None
            }
        }
    } else {
        Some(value.clone())
    }
}

fn aliases(query: Option<&Value>) -> HashMap<String, String> {
    query
        .and_then(|query| query.get("From"))
        .and_then(Value::as_array)
        .map(|from| {
            from.iter()
                .filter_map(|source| Some((string(source, "Name")?, string(source, "Entity")?)))
                .collect()
        })
        .unwrap_or_default()
}

fn collect_targets(
    value: &Value,
    aliases: &HashMap<String, String>,
    targets: &mut Vec<FieldTarget>,
) {
    if let Some(items) = value.as_array() {
        for item in items {
            collect_targets(item, aliases, targets);
        }
        return;
    }
    let Some(object) = value.as_object() else {
        if let Some(text) = value.as_str() {
            if (text.starts_with('{') || text.starts_with('['))
                && let Ok(nested) = serde_json::from_str::<Value>(text)
            {
                collect_targets(&nested, aliases, targets);
            }
            for raw in dax::references(text) {
                if let RawRef::Field { table, name, .. } = raw {
                    targets.push(written(table, name));
                }
            }
        }
        return;
    };
    if let (Some(entity), Some(property)) = (
        object.get("Entity").and_then(Value::as_str),
        object.get("Property").and_then(Value::as_str),
    ) {
        targets.push(written(Some(entity), property));
    }
    if let Some(property) = object.get("Property").and_then(Value::as_str)
        && let Some(source) = value
            .get("Expression")
            .and_then(|expression| expression.get("SourceRef"))
    {
        let table = source.get("Entity").and_then(Value::as_str).or_else(|| {
            source
                .get("Source")
                .and_then(Value::as_str)
                .and_then(|alias| aliases.get(alias).map(String::as_str))
        });
        targets.push(written(table, property));
    }
    if let Some(query_ref) = object.get("queryRef").and_then(Value::as_str)
        && let Some((table, field)) = query_ref.rsplit_once('.')
    {
        targets.push(written(Some(table.trim_matches('\'')), field));
    }
    for child in object.values() {
        collect_targets(child, aliases, targets);
    }
}

fn collect_visual_calculations(value: &Value, targets: &mut Vec<FieldTarget>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_visual_calculations(item, targets);
            }
        }
        Value::Object(object) => {
            if object.get("Name").and_then(Value::as_str).is_some()
                && let Some(expression) = object.get("Expression").and_then(Value::as_str)
            {
                for raw in dax::references(expression) {
                    if let RawRef::Field { table, name, .. } = raw {
                        targets.push(written(table, name));
                    }
                }
            }
            for child in object.values() {
                collect_visual_calculations(child, targets);
            }
        }
        _ => {}
    }
}

fn written(table: Option<&str>, name: &str) -> FieldTarget {
    FieldTarget::Written(FieldRef {
        table: table.map(NameKey::new),
        name: NameKey::new(name),
    })
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}
