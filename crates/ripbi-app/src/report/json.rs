//! The `report` JSON contract, schema v1 (`crates/ripbi-cli/docs/report.md`):
//! what `ripbi report --json` prints.

use std::io;

use serde::Serialize;

use super::{FilterNode, Inventory, PageNode, UnresolvedNode};

/// Prints the JSON inventory (schema_version 1 — `docs/report.md`).
pub fn write(out: &mut dyn io::Write, inventory: &Inventory) -> io::Result<()> {
    let payload = JsonInventory {
        schema_version: 1,
        target: inventory.target.clone(),
        reports: inventory
            .reports
            .iter()
            .map(|report| JsonReport {
                path: report.path.clone(),
                name: report.name.clone(),
                unresolved: report.unresolved.iter().map(json_unresolved).collect(),
                pages: report
                    .pages
                    .iter()
                    .map(|page| JsonPage {
                        name: page.name.clone(),
                        display_name: page.display_name.clone(),
                        hidden: page.hidden,
                        mobile: page.mobile,
                        filters: page.filters.iter().map(json_filter).collect(),
                        unresolved: page.unresolved.iter().map(json_unresolved).collect(),
                        visuals: report_visuals(page),
                    })
                    .collect(),
                bookmarks: report.bookmarks,
                report_measures: report.report_measures.clone(),
                unresolved_count: report.unresolved_count,
            })
            .collect(),
        skips: JsonSkips {
            count: inventory.skips.len(),
            notices: inventory
                .skips
                .iter()
                .map(|skip| JsonSkip {
                    path: skip.path.clone(),
                    location: skip.location.clone(),
                    kind: skip.kind,
                    detail: skip.detail.clone(),
                })
                .collect(),
        },
    };
    serde_json::to_writer_pretty(&mut *out, &payload)?;
    writeln!(out)
}

/// A page's visuals as JSON DTOs.
fn report_visuals(page: &PageNode) -> Vec<JsonVisual> {
    page.visuals
        .iter()
        .map(|visual| JsonVisual {
            name: visual.name.clone(),
            visual_type: visual.visual_type.clone(),
            fields: visual
                .fields
                .iter()
                .map(|field| JsonField {
                    kind: field.kind,
                    target: field.target.clone(),
                    binding_site: field.binding_site.clone(),
                    active: field.active,
                })
                .collect(),
            filters: visual.filters.iter().map(json_filter).collect(),
            unresolved: visual.unresolved.iter().map(json_unresolved).collect(),
        })
        .collect()
}

/// The filter DTO report/page/visual filters share.
fn json_filter(filter: &FilterNode) -> JsonFilter {
    JsonFilter {
        name: filter.name.clone(),
        display_name: filter.display_name.clone(),
        filter_type: filter.filter_type.clone(),
        target: filter.target.clone(),
        references: filter.references.clone(),
    }
}

/// The unresolved DTO every level shares.
fn json_unresolved(unresolved: &UnresolvedNode) -> JsonUnresolved {
    JsonUnresolved {
        target: unresolved.target.clone(),
        reason: unresolved.reason,
        bound_artifact: unresolved.artifact.clone(),
        binding_site: unresolved.binding_site.clone(),
    }
}

#[derive(Serialize)]
struct JsonInventory {
    schema_version: u8,
    target: String,
    reports: Vec<JsonReport>,
    skips: JsonSkips,
}

#[derive(Serialize)]
struct JsonReport {
    path: String,
    name: String,
    /// Broken live bindings at report level.
    unresolved: Vec<JsonUnresolved>,
    pages: Vec<JsonPage>,
    /// v1 lists the count only — saved bookmark state is not itemized.
    bookmarks: usize,
    report_measures: Vec<String>,
    /// The itemized unresolved bindings: every visual-, page-, and
    /// report-level row this report carries.
    unresolved_count: usize,
}

#[derive(Serialize)]
struct JsonPage {
    name: String,
    display_name: Option<String>,
    hidden: bool,
    mobile: bool,
    filters: Vec<JsonFilter>,
    /// Broken live bindings at page level.
    unresolved: Vec<JsonUnresolved>,
    visuals: Vec<JsonVisual>,
}

#[derive(Serialize)]
struct JsonVisual {
    name: String,
    #[serde(rename = "type")]
    visual_type: String,
    fields: Vec<JsonField>,
    filters: Vec<JsonFilter>,
    unresolved: Vec<JsonUnresolved>,
}

#[derive(Serialize)]
struct JsonField {
    /// `column`, `measure`, `hierarchy_level`, `aggregation`, or `written`.
    kind: &'static str,
    target: String,
    /// `field_well:<role>`, `sort`, `conditional_formatting`, or `alt_text`.
    binding_site: String,
    /// Present on field-well projections only; inactive projections still
    /// bind.
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<bool>,
}

#[derive(Serialize)]
struct JsonFilter {
    name: Option<String>,
    display_name: Option<String>,
    /// As written, e.g. `Categorical` or `Advanced`.
    filter_type: Option<String>,
    target: Option<String>,
    references: Vec<String>,
}

#[derive(Serialize)]
struct JsonUnresolved {
    target: String,
    /// The same vocabulary as `scan`'s broken bindings.
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bound_artifact: Option<String>,
    binding_site: String,
}

#[derive(Serialize)]
struct JsonSkips {
    count: usize,
    notices: Vec<JsonSkip>,
}

#[derive(Serialize)]
struct JsonSkip {
    path: String,
    location: Option<String>,
    kind: &'static str,
    detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{FieldNode, ReportNode, UsedRow, VisualNode};

    fn sample() -> Inventory {
        Inventory {
            target: "models/Sales.SemanticModel".to_string(),
            reports: vec![ReportNode {
                path: "reports/Sales.Report".to_string(),
                name: "Sales".to_string(),
                filters: Vec::new(),
                unresolved: Vec::new(),
                pages: vec![PageNode {
                    name: "P1".to_string(),
                    display_name: Some("Overview".to_string()),
                    hidden: false,
                    mobile: false,
                    filters: Vec::new(),
                    unresolved: Vec::new(),
                    visuals: vec![VisualNode {
                        name: "V1".to_string(),
                        visual_type: "card".to_string(),
                        fields: vec![FieldNode {
                            kind: "measure",
                            target: "'Sales'[Total]".to_string(),
                            binding_site: "field_well:Values".to_string(),
                            active: Some(true),
                        }],
                        filters: vec![FilterNode {
                            name: Some("Filter5".to_string()),
                            display_name: None,
                            filter_type: Some("Categorical".to_string()),
                            target: Some("'Product'[Color]".to_string()),
                            references: Vec::new(),
                        }],
                        unresolved: vec![UnresolvedNode {
                            target: "'Sales'[Gone]".to_string(),
                            reason: "field_not_found",
                            artifact: None,
                            binding_site: "field_well:Values".to_string(),
                        }],
                    }],
                }],
                bookmarks: 1,
                report_measures: vec!["Budget %".to_string()],
                unresolved_count: 1,
            }],
            skips: Vec::new(),
            used: vec![
                UsedRow {
                    kind: "column",
                    id: "'Sales'[Amount]".to_string(),
                },
                UsedRow {
                    kind: "measure",
                    id: "'Sales'[Total]".to_string(),
                },
            ],
        }
    }

    /// `--json` pins schema_version 1 and the field/vocabulary shapes.
    #[test]
    fn json_pins_the_v1_schema() {
        let mut out = Vec::new();
        write(&mut out, &sample()).expect("json renders");

        let payload: serde_json::Value = serde_json::from_slice(&out).expect("valid json");
        assert_eq!(payload["schema_version"], 1);
        let report = &payload["reports"][0];
        assert_eq!(report["name"], "Sales");
        assert_eq!(report["unresolved_count"], 1);
        let visual = &report["pages"][0]["visuals"][0];
        assert_eq!(visual["type"], "card");
        assert_eq!(visual["fields"][0]["binding_site"], "field_well:Values");
        assert_eq!(visual["fields"][0]["active"], true);
        assert_eq!(visual["unresolved"][0]["reason"], "field_not_found");
    }
}
