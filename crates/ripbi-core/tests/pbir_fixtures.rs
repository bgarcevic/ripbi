//! Fixture tests for PBIR ingestion: a golden report parsed exactly (fields,
//! skips, and the extracted root set), plus locator semantics.
//!
//! The golden fixture `Mini.Report` is hand-built from the published PBIR
//! schemas (see the `$schema` URLs in its files) so it can cover every binding
//! site the real samples never co-locate: report/page/visual filters, a
//! persisted automatic filter, drillthrough parameters, conditional
//! formatting, a tooltip page, and bookmarks' three filter levels.

use std::path::PathBuf;

use ripbi_core::ingest::{SkipKind, report};
use ripbi_core::report::{
    BindingKind, Bookmark, BookmarkSection, BookmarkVisual, DatasetReference,
    DrillthroughParameter, FieldTarget, FieldWell, Filter, Page, PageBinding, PageBindingKind,
    Projection, ReportMeasure, ReportModel, Visual,
};
use ripbi_core::{DaxExpressionKind, DaxExpressionRef, ExpressionOwner, NameKey};

fn fixture(groups: &[&str]) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("pbir");
    for group in groups {
        path.push(group);
    }
    path
}

fn column(table: &str, name: &str) -> FieldTarget {
    FieldTarget::Column {
        table: NameKey::new(table),
        column: NameKey::new(name),
    }
}

fn measure(table: &str, name: &str) -> FieldTarget {
    FieldTarget::Measure {
        home_table: Some(NameKey::new(table)),
        measure: NameKey::new(name),
    }
}

/// A filter whose only reference is its declared target field. The golden
/// fixture writes a `type` on every `filterConfig` filter; bookmarks and the
/// visual's persisted automatic filter repeat the shape.
fn filter_on(name: &str, filter_type: &str, target: FieldTarget) -> Filter {
    Filter {
        name: Some(NameKey::new(name)),
        filter_type: Some(filter_type.to_string()),
        target: Some(target),
        ..Default::default()
    }
}

/// The `Mini` golden report, hand-built: what `Mini.Report` must parse into,
/// field for field.
fn golden_report() -> ReportModel {
    ReportModel {
        name: Some("Mini".to_string()),
        dataset: DatasetReference::ByPath {
            // Points at the TMDL golden model, so the report is a complete,
            // Desktop-openable .pbip pair without duplicating the model.
            path: "../../../tmdl/golden/Mini.SemanticModel".to_string(),
        },
        filters: vec![filter_on(
            "Filter1",
            "Categorical",
            column("Product", "Category"),
        )],
        pages: vec![
            Page {
                name: NameKey::new("P1"),
                display_name: Some("Overview".to_string()),
                is_hidden: false,
                // The acceptance-critical root: `Date'[Calendar Year]` is
                // referenced by this page filter and nothing else.
                filters: vec![filter_on(
                    "PageFilter",
                    "Categorical",
                    column("Date", "Calendar Year"),
                )],
                binding: None,
                visuals: vec![
                    Visual {
                        name: NameKey::new("V1"),
                        visual_type: "slicer".to_string(),
                        wells: vec![FieldWell {
                            role: "Category".to_string(),
                            projections: vec![Projection {
                                target: column("Product", "Category"),
                                query_ref: Some("Product.Category".to_string()),
                                active: true,
                            }],
                        }],
                        // The declared field and the condition tree's aliased
                        // reference resolve to the same target. This is the
                        // one filter carrying a `displayName` in the fixture.
                        filters: vec![Filter {
                            name: Some(NameKey::new("V1Filter")),
                            display_name: Some(
                                "Business Type is not Regular Superstore".to_string(),
                            ),
                            filter_type: Some("Advanced".to_string()),
                            target: Some(column("Reseller", "Business Type")),
                            references: vec![column("Reseller", "Business Type")],
                        }],
                        sorts: vec![column("Product", "Category")],
                        conditional_formatting: Vec::new(),
                        // A literal alt text carries no model reference.
                        alt_text: Vec::new(),
                        tooltip_page: None,
                    },
                    Visual {
                        name: NameKey::new("V2"),
                        visual_type: "card".to_string(),
                        wells: vec![FieldWell {
                            role: "Values".to_string(),
                            projections: vec![
                                Projection {
                                    target: measure("Sales", "Sales"),
                                    query_ref: Some("Sales.Sales".to_string()),
                                    active: true,
                                },
                                Projection {
                                    target: FieldTarget::Aggregation {
                                        function: Some("Sum".to_string()),
                                        inner: Box::new(column("Sales", "Units")),
                                    },
                                    query_ref: Some("Sum(Sales.Units)".to_string()),
                                    active: false,
                                },
                            ],
                        }],
                        // The persisted automatic filter declares no target
                        // field; its condition tree is the reference.
                        filters: vec![Filter {
                            name: None,
                            display_name: None,
                            filter_type: None,
                            target: None,
                            references: vec![column("Date", "Date Role")],
                        }],
                        sorts: Vec::new(),
                        conditional_formatting: vec![measure("Sales", "Cost")],
                        // The accessibility alt text: read aloud by screen
                        // readers, so the interpolated column stays alive.
                        alt_text: vec![column("Sales", "Units")],
                        tooltip_page: Some(NameKey::new("P2")),
                    },
                ],
            },
            Page {
                name: NameKey::new("P2"),
                display_name: Some("Customer Drill".to_string()),
                is_hidden: false,
                filters: Vec::new(),
                binding: Some(PageBinding {
                    kind: PageBindingKind::Drillthrough,
                    parameters: vec![DrillthroughParameter {
                        name: Some(NameKey::new("Param_Filter5")),
                        target: column("Industry", "Industry"),
                    }],
                }),
                visuals: Vec::new(),
            },
        ],
        // The phone layout (`definition.mobile/`, issue #49): mirrors the
        // desktop card on P1. Report-level state — anchor, report measures,
        // bookmarks — is desktop-only and absent here.
        mobile_pages: vec![Page {
            name: NameKey::new("P1"),
            display_name: Some("Overview".to_string()),
            is_hidden: false,
            filters: Vec::new(),
            binding: None,
            visuals: vec![Visual {
                name: NameKey::new("VM"),
                visual_type: "card".to_string(),
                wells: vec![FieldWell {
                    role: "Values".to_string(),
                    projections: vec![Projection {
                        target: measure("Sales", "Sales"),
                        query_ref: Some("Sales.Sales".to_string()),
                        active: true,
                    }],
                }],
                filters: Vec::new(),
                sorts: Vec::new(),
                conditional_formatting: Vec::new(),
                alt_text: Vec::new(),
                tooltip_page: None,
            }],
        }],
        bookmarks: vec![
            Bookmark {
                name: NameKey::new("B1"),
                display_name: Some("Saved view".to_string()),
                filters: vec![filter_on(
                    "BookmarkFilter",
                    "Categorical",
                    column("Geography", "Country"),
                )],
                sections: vec![BookmarkSection {
                    page: NameKey::new("P1"),
                    filters: vec![filter_on(
                        "Filter4",
                        "Categorical",
                        column("Owners", "Sales owner"),
                    )],
                    visuals: vec![BookmarkVisual {
                        visual: NameKey::new("V1"),
                        wells: vec![FieldWell {
                            role: "Rows".to_string(),
                            projections: vec![Projection {
                                target: column("Product", "Subcategory"),
                                query_ref: None,
                                active: false,
                            }],
                        }],
                        filters: vec![filter_on(
                            "BookmarkV1Filter",
                            "Categorical",
                            column("Product", "Color"),
                        )],
                    }],
                }],
            },
            // Saved on a page since deleted: `Pgone` appears in neither
            // `pages.json` `pageOrder` nor the `pages/` folders, so the
            // section — and the `'Product'[Color]` filter it carries — never
            // reaches the AST (issue #48). It is the fixture's one notice.
            Bookmark {
                name: NameKey::new("B2"),
                display_name: Some("Deleted page view".to_string()),
                filters: Vec::new(),
                sections: Vec::new(),
            },
        ],
        measures: vec![ReportMeasure {
            name: NameKey::new("Budget %"),
            expression: "DIVIDE([Sales], [Budget])".to_string(),
            format_string: Some("0.0%".to_string()),
        }],
    }
}

#[test]
fn golden_report_parses_exactly_with_expected_skips() {
    let ingested = report(&fixture(&["golden", "Mini.Report"])).expect("golden fixture parses");

    assert_eq!(ingested.value, golden_report());
    // Exactly one notice: B2's saved state for the deleted page `Pgone`
    // (issue #48). Everything else skipped is deliberately-unmodeled and
    // silent.
    assert_eq!(ingested.skips.len(), 1, "{:#?}", ingested.skips);
    assert_eq!(ingested.skips[0].kind, SkipKind::StaleState);
    assert_eq!(
        ingested.skips[0].location.as_deref(),
        Some("/explorationState/sections/Pgone")
    );
    assert_eq!(
        ingested.skips[0].detail,
        "bookmark 'B2' saves state for page 'Pgone', which the report no longer defines; \
         its saved filters bind nothing (it is also the bookmark's active section)"
    );
    assert_eq!(
        ingested.skips[0].path.file_name().and_then(|n| n.to_str()),
        Some("B2.bookmark.json")
    );
}

/// The extracted root set, with full provenance — the reachability roots the
/// graph's BFS starts from. A slipped site anywhere must fail here; in
/// particular `'Date'[Calendar Year]` is a root through its page filter alone.
#[test]
fn the_extracted_root_set_is_complete_and_ordered() {
    let ingested = report(&fixture(&["golden", "Mini.Report"])).expect("golden fixture parses");

    /// One root's full provenance: page, visual, bookmark, layout, kind, and
    /// the target as `Display`.
    type Root<'a> = (
        Option<&'a str>,
        Option<&'a str>,
        Option<&'a str>,
        bool,
        BindingKind<'a>,
        String,
    );

    let roots: Vec<Root<'_>> = ingested
        .value
        .bindings()
        .into_iter()
        .map(|binding| {
            (
                binding.page.map(NameKey::as_str),
                binding.visual.map(NameKey::as_str),
                binding.bookmark.map(NameKey::as_str),
                binding.mobile,
                binding.kind,
                binding.target.to_string(),
            )
        })
        .collect();

    assert_eq!(
        roots,
        vec![
            // Report filter.
            (
                None,
                None,
                None,
                false,
                BindingKind::Filter,
                "'Product'[Category]".to_string()
            ),
            // Page filter: the only reference to this field anywhere.
            (
                Some("P1"),
                None,
                None,
                false,
                BindingKind::Filter,
                "'Date'[Calendar Year]".to_string()
            ),
            // V1: well, filter (target then reference), sort.
            (
                Some("P1"),
                Some("V1"),
                None,
                false,
                BindingKind::FieldWell { role: "Category" },
                "'Product'[Category]".to_string()
            ),
            (
                Some("P1"),
                Some("V1"),
                None,
                false,
                BindingKind::Filter,
                "'Reseller'[Business Type]".to_string()
            ),
            (
                Some("P1"),
                Some("V1"),
                None,
                false,
                BindingKind::Filter,
                "'Reseller'[Business Type]".to_string()
            ),
            (
                Some("P1"),
                Some("V1"),
                None,
                false,
                BindingKind::Sort,
                "'Product'[Category]".to_string()
            ),
            // V2: wells, the persisted automatic filter, conditional formatting.
            (
                Some("P1"),
                Some("V2"),
                None,
                false,
                BindingKind::FieldWell { role: "Values" },
                "'Sales'[Sales]".to_string()
            ),
            (
                Some("P1"),
                Some("V2"),
                None,
                false,
                BindingKind::FieldWell { role: "Values" },
                "Sum('Sales'[Units])".to_string()
            ),
            (
                Some("P1"),
                Some("V2"),
                None,
                false,
                BindingKind::Filter,
                "'Date'[Date Role]".to_string()
            ),
            (
                Some("P1"),
                Some("V2"),
                None,
                false,
                BindingKind::ConditionalFormatting,
                "'Sales'[Cost]".to_string()
            ),
            // V2's alt text interpolates a column; V1's is a literal (silent).
            (
                Some("P1"),
                Some("V2"),
                None,
                false,
                BindingKind::AltText,
                "'Sales'[Units]".to_string()
            ),
            // Drillthrough parameter.
            (
                Some("P2"),
                None,
                None,
                false,
                BindingKind::Drillthrough,
                "'Industry'[Industry]".to_string()
            ),
            // The phone layout's card: same binding shape, tagged mobile,
            // enumerating after every desktop page (issue #49).
            (
                Some("P1"),
                Some("VM"),
                None,
                true,
                BindingKind::FieldWell { role: "Values" },
                "'Sales'[Sales]".to_string()
            ),
            // Bookmark: report-level filter, section filter, well, visual filter.
            (
                None,
                None,
                Some("B1"),
                false,
                BindingKind::Filter,
                "'Geography'[Country]".to_string()
            ),
            (
                Some("P1"),
                None,
                Some("B1"),
                false,
                BindingKind::Filter,
                "'Owners'[Sales owner]".to_string()
            ),
            (
                Some("P1"),
                Some("V1"),
                Some("B1"),
                false,
                BindingKind::FieldWell { role: "Rows" },
                "'Product'[Subcategory]".to_string()
            ),
            (
                Some("P1"),
                Some("V1"),
                Some("B1"),
                false,
                BindingKind::Filter,
                "'Product'[Color]".to_string()
            ),
        ]
    );
}

/// A report measure is an expression source on top of the model's own, body
/// and dynamic format string both.
#[test]
fn a_report_measure_is_a_dax_expression_source() {
    let ingested = report(&fixture(&["golden", "Mini.Report"])).expect("golden fixture parses");

    assert_eq!(
        ingested.value.dax_expressions(),
        vec![
            DaxExpressionRef {
                owner: ExpressionOwner::ReportMeasure {
                    measure: "Budget %"
                },
                kind: DaxExpressionKind::ReportMeasure,
                home_table: None,
                text: "DIVIDE([Sales], [Budget])",
            },
            DaxExpressionRef {
                owner: ExpressionOwner::ReportMeasure {
                    measure: "Budget %"
                },
                kind: DaxExpressionKind::ReportMeasureFormatString,
                home_table: None,
                text: "0.0%",
            },
        ]
    );
}

#[test]
fn accepts_the_definition_folder_directly() {
    let item = fixture(&["golden", "Mini.Report"]);
    let direct = report(&item.join("definition")).expect("definition/ parses");
    let via_item = report(&item).expect("item folder parses");

    assert_eq!(direct.value, via_item.value);
    // `.platform` is still found beside the definition folder.
    assert_eq!(direct.value.name.as_deref(), Some("Mini"));
}

#[test]
fn rejects_a_folder_that_is_not_a_report() {
    let error = report(&fixture(&["golden"]));
    assert!(error.is_err(), "the fixtures root has no report.json");
}

// --- Resilience: drift parses *and* is noticed -------------------------------

#[test]
fn an_unresolved_alias_keeps_the_reference_as_written_and_is_noticed() {
    let ingested = report(&fixture(&["resilience", "unresolved-alias"]))
        .expect("drift must not fail the parse");

    let target = ingested.value.pages[0].filters[0].target.as_ref();
    assert_eq!(
        target,
        Some(&FieldTarget::Written(ripbi_core::FieldRef {
            table: None,
            name: NameKey::new("Region"),
        })),
        "the alias is not a table name, so the reference stays table-less"
    );

    assert_eq!(ingested.skips.len(), 1);
    assert_eq!(ingested.skips[0].kind, SkipKind::UnresolvedAlias);
    assert_eq!(
        ingested.skips[0].location.as_deref(),
        Some("/filterConfig/filters/0/field/Expression")
    );
}

#[test]
fn a_malformed_filter_field_is_dropped_and_noticed_but_the_visual_stays() {
    let ingested = report(&fixture(&["resilience", "malformed-filter"]))
        .expect("drift must not fail the parse");

    let visual = &ingested.value.pages[0].visuals[0];
    assert_eq!(visual.visual_type, "tableEx");
    assert_eq!(visual.wells.len(), 1, "the rest of the visual parses");
    assert!(
        visual.filters[0].target.is_none(),
        "the unreadable field must not reach the AST"
    );

    assert_eq!(ingested.skips.len(), 1);
    assert_eq!(ingested.skips[0].kind, SkipKind::MalformedValue);
    assert!(
        ingested.skips[0].detail.contains("Property"),
        "{}",
        ingested.skips[0].detail
    );
}

/// Unreadable display metadata (`displayName`, `type`) degrades to `None` with
/// a notice; the filter's binding — the part that keeps objects alive — parses
/// regardless (issue #33's inventory reads the metadata, never relies on it).
#[test]
fn malformed_filter_metadata_degrades_to_none_but_the_filter_binds() {
    let ingested = report(&fixture(&["resilience", "malformed-filter-metadata"]))
        .expect("drift must not fail the parse");

    let filter = &ingested.value.pages[0].visuals[0].filters[0];
    assert_eq!(filter.name.as_ref().map(NameKey::as_str), Some("F1"));
    assert_eq!(filter.display_name, None);
    assert_eq!(filter.filter_type, None);
    assert_eq!(
        filter.target,
        Some(column("Product", "Category")),
        "the field reference is unaffected by the metadata drift"
    );

    let pointers: Vec<_> = ingested
        .skips
        .iter()
        .map(|skip| (skip.kind, skip.location.as_deref()))
        .collect();
    assert_eq!(
        pointers,
        vec![
            (
                SkipKind::MalformedValue,
                Some("/filterConfig/filters/0/displayName")
            ),
            (
                SkipKind::MalformedValue,
                Some("/filterConfig/filters/0/type")
            ),
        ],
        "{:#?}",
        ingested.skips
    );
}

#[test]
fn an_unknown_property_is_noticed_with_its_pointer() {
    let ingested = report(&fixture(&["resilience", "unknown-property"]))
        .expect("drift must not fail the parse");

    assert_eq!(ingested.value.pages[0].visuals[0].visual_type, "tableEx");

    assert_eq!(ingested.skips.len(), 1);
    assert_eq!(ingested.skips[0].kind, SkipKind::UnknownProperty);
    assert_eq!(
        ingested.skips[0].location.as_deref(),
        Some("/mysteryProperty"),
    );
    assert!(
        ingested.skips[0].detail.contains("mysteryProperty"),
        "{}",
        ingested.skips[0].detail
    );
}

#[test]
fn a_malformed_page_is_skipped_and_noticed_but_the_report_parses() {
    let ingested =
        report(&fixture(&["resilience", "malformed-page"])).expect("drift must not fail the parse");

    assert!(
        ingested.value.pages.is_empty(),
        "the unreadable page must not reach the AST"
    );

    assert_eq!(ingested.skips.len(), 1);
    assert_eq!(ingested.skips[0].kind, SkipKind::MalformedValue);
    assert_eq!(
        ingested.skips[0]
            .path
            .file_name()
            .and_then(|name| name.to_str()),
        Some("page.json")
    );
}

/// Drift inside the phone layout behaves like desktop drift: the page is
/// dropped, the notice points into `definition.mobile/` so the user can find
/// it, and the report itself still parses (issue #49).
#[test]
fn a_malformed_mobile_page_is_skipped_and_noticed_but_the_report_parses() {
    let ingested = report(&fixture(&["resilience", "malformed-mobile"]))
        .expect("drift must not fail the parse");

    assert!(
        ingested.value.mobile_pages.is_empty(),
        "the unreadable phone page must not reach the AST"
    );
    assert!(
        ingested.value.pages.is_empty(),
        "the desktop tree is unaffected"
    );

    assert_eq!(ingested.skips.len(), 1);
    assert_eq!(ingested.skips[0].kind, SkipKind::MalformedValue);
    assert_eq!(
        ingested.skips[0]
            .path
            .file_name()
            .and_then(|name| name.to_str()),
        Some("page.json")
    );
    assert!(
        ingested.skips[0]
            .path
            .components()
            .any(|component| component.as_os_str() == "definition.mobile"),
        "the notice must point into the phone layout: {:?}",
        ingested.skips[0].path
    );
}

/// A report that ships no phone layout — the common case — parses to no
/// mobile pages and no notices.
#[test]
fn a_report_without_a_phone_layout_has_no_mobile_pages() {
    let ingested = report(&fixture(&["resilience", "malformed-page"]))
        .expect("the desktop-only report parses");

    assert!(ingested.value.mobile_pages.is_empty());
}
