//! Excel workbooks as report consumers (issue #172): PivotTable fields,
//! slicers, timelines, CUBE formula literals, and calculated members bind the
//! model they name; the cache's full hierarchy list never does.

use std::path::PathBuf;

use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest::{is_workbook, report, semantic_model};
use ripbi_core::report::FieldTarget;
use ripbi_core::{
    BindingSite, Error, NameKey, ObjectId, Provenance, ReportModel, SkipKind, TabularDatabase,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/xlsx")
        .join(name)
}

fn model() -> TabularDatabase {
    semantic_model(&fixture("model.bim"))
        .expect("reads the model")
        .value
}

fn workbook(name: &str) -> ReportModel {
    report(&fixture(name)).expect("reads the workbook").value
}

fn column(table: &str, column: &str) -> ObjectId {
    ObjectId::Column {
        table: NameKey::new(table),
        column: NameKey::new(column),
    }
}

fn measure(table: &str, measure: &str) -> ObjectId {
    ObjectId::Measure {
        table: NameKey::new(table),
        measure: NameKey::new(measure),
    }
}

fn unused(model: &TabularDatabase, reports: &[&ReportModel]) -> Vec<ObjectId> {
    DependencyGraph::build(model, reports)
        .unused_objects()
        .into_iter()
        .map(|unused| unused.id)
        .collect()
}

/// Every binding of a visual as `role: target`, in report order.
fn bindings(report: &ReportModel, page: &str, visual: &str) -> Vec<String> {
    let page = report
        .pages
        .iter()
        .find(|p| p.name.as_str() == page)
        .unwrap_or_else(|| panic!("no page {page}"));
    let visual = page
        .visuals
        .iter()
        .find(|v| v.name.as_str() == visual)
        .unwrap_or_else(|| panic!("no visual {visual} on {}", page.name.as_str()));
    visual
        .wells
        .iter()
        .flat_map(|well| {
            well.projections
                .iter()
                .map(move |projection| format!("{}: {}", well.role, projection.target))
        })
        .collect()
}

#[test]
fn workbooks_are_recognized_by_extension() {
    assert!(is_workbook(&fixture("olap-pivot-foodmart.xlsx")));
    assert!(is_workbook("Book.XLSM".as_ref()));
    assert!(!is_workbook("Sales.pbix".as_ref()));
}

#[test]
fn a_binary_workbook_is_refused() {
    let error = report("missing.xlsb".as_ref()).expect_err("binary workbooks are refused");
    assert!(
        matches!(&error, Error::UnsupportedFormat(detail) if detail.contains("binary workbook")),
        "{error}"
    );
}

#[test]
fn pivot_fields_bind_from_every_area() {
    let book = workbook("olap-pivot-pbiazure-fields.xlsx");
    assert_eq!(
        book.name.as_deref(),
        Some("olap-pivot-pbiazure-fields.xlsx")
    );
    assert_eq!(
        bindings(&book, "Sheet2", "Sales Model"),
        [
            "Rows: [Product].[Product Key].[Product Key]",
            "Rows: [Product].[Category].[Category]",
            "Filters: [Customer].[Country].[Country]",
            "Values: [Measures].[Average Price]",
            "Values: sum([Sales].[Amount].[Amount])",
        ]
    );
}

#[test]
fn cube_formulas_bind_their_literals_and_flag_what_they_cannot_read() {
    let ingested = report(&fixture("olap-pivot-pbiazure-fields.xlsx")).expect("reads");
    let book = ingested.value;
    assert_eq!(
        bindings(&book, "Sheet2", "A22"),
        ["CUBEMEMBER: [Ship Date]"]
    );
    assert_eq!(
        bindings(&book, "Sheet2", "A25"),
        [
            "CUBEVALUE: [Measures].[Total Sales]",
            "CUBEVALUE: [Ship Date].[Date]"
        ]
    );
    // `CUBEVALUE("Sales Model",A1)`: A1 holds no CUBE formula.
    let notices: Vec<_> = ingested
        .skips
        .iter()
        .filter(|skip| skip.kind == SkipKind::NotAnalyzed)
        .collect();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].location.as_deref(), Some("Sheet2!A24"));
    assert!(notices[0].detail.contains("a cell reference (`A1`)"));
}

#[test]
fn a_measure_used_only_from_excel_is_live_with_the_workbook() {
    let model = model();
    let without = unused(&model, &[]);
    assert!(without.contains(&measure("Sales", "Total Sales")));

    let book = workbook("olap-pivot-pbiazure-fields.xlsx");
    let with = unused(&model, &[&book]);
    // Only a CUBE formula uses it.
    assert!(!with.contains(&measure("Sales", "Total Sales")));
    // Pivot rows, page filter, explicit and implicit measures.
    for live in [
        column("Product", "Product Key"),
        column("Product", "Category"),
        column("Customer", "Country"),
        measure("Key Measures", "Average Price"),
        column("Sales", "Amount"),
        column("Ship Date", "Date"),
    ] {
        assert!(!with.contains(&live), "{live:?} should be live");
    }
    // Listed in the cache's 22 hierarchies, placed nowhere.
    for dead in [
        column("Product", "Size"),
        column("Customer", "Segment"),
        measure("Sales", "Order Count"),
        measure("Key Measures", "Margin %"),
    ] {
        assert!(with.contains(&dead), "{dead:?} should stay unused");
    }
}

#[test]
fn an_empty_pivot_binds_nothing() {
    let book = workbook("olap-pivot-pbiazure-empty.xlsx");
    assert!(book.bindings().is_empty());
    assert!(book.queries.is_empty());
}

#[test]
fn the_reason_names_the_workbook_sheet_and_pivot() {
    let model = model();
    let book = workbook("olap-pivot-pbiazure-fields.xlsx");
    let graph = DependencyGraph::build(&model, &[&book]);
    let reasons: Vec<String> = graph
        .roots_of(&column("Product", "Category"))
        .into_iter()
        .filter_map(|provenance| match provenance {
            Provenance::Binding(edge) => Some(edge.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].contains("Sales Model"), "{}", reasons[0]);
    assert!(reasons[0].contains("Sheet2"), "{}", reasons[0]);
    assert!(
        reasons[0].contains("olap-pivot-pbiazure-fields.xlsx"),
        "{}",
        reasons[0]
    );
    assert!(graph.broken_bindings().is_empty());
}

#[test]
fn a_stale_pivot_field_is_a_broken_binding() {
    let mut model = model();
    let product = model
        .tables
        .iter_mut()
        .find(|t| t.name == "Product")
        .expect("Product");
    product.columns.retain(|c| c.name != "Category");
    let book = workbook("olap-pivot-pbiazure-fields.xlsx");
    let graph = DependencyGraph::build(&model, &[&book]);
    let broken: Vec<String> = graph
        .broken_bindings()
        .iter()
        .map(|b| b.target.to_string())
        .collect();
    assert_eq!(broken, ["[Product].[Category].[Category]"]);
}

#[test]
fn the_cache_hierarchy_list_is_not_a_root() {
    // 313 cache hierarchies per cache; three fields placed.
    let book = workbook("olap-pivots-adventure-works-strict.xlsx");
    let mut targets: Vec<String> = book
        .bindings()
        .iter()
        .map(|b| b.target.to_string())
        .collect();
    targets.sort();
    targets.dedup();
    assert_eq!(
        targets,
        [
            "[Customer].[Country].[Country]",
            "[Date].[Calendar Year].[Calendar Year]",
            "[Measures].[Internet Sales Amount]",
        ]
    );
    assert_eq!(book.pages.iter().map(|p| p.visuals.len()).sum::<usize>(), 5);
}

#[test]
fn calculated_members_bind_their_formula_not_their_name() {
    let book = workbook("olap-pivot-foodmart.xlsx");
    let all: Vec<String> = book
        .bindings()
        .iter()
        .map(|b| b.target.to_string())
        .collect();
    // The named set's own name is local; its formula names the dimension.
    assert!(!all.iter().any(|t| t.contains("TestSet")), "{all:?}");
    assert!(
        all.contains(&"[Product].[All Products]".to_string()),
        "{all:?}"
    );
    assert!(all.contains(&"[Measures].[Profit]".to_string()));
    assert!(all.contains(&"[Marital Status].[Marital Status]".to_string()));
}

#[test]
fn slicers_timelines_and_shared_caches_bind_per_pivot() {
    let ingested = report(&fixture("olap-pivots-spec-slicers-xmla.xlsx")).expect("reads");
    let book = ingested.value;
    assert_eq!(
        bindings(&book, "Sheet2", "Sales Model"),
        [
            "Rows: [Product].[Category].[Category]",
            "Columns: [Date].[Year].[Year]",
            "Values: [Measures].[Total Sales]",
        ]
    );
    assert_eq!(
        bindings(&book, "Sheet3", "Segments"),
        [
            "Rows: [Customer].[Segment].[Segment]",
            "Values: [Measures].[Order Count]",
        ]
    );
    // The calculated measure binds what its MDX names, never itself.
    assert_eq!(
        bindings(&book, "Sheet4", "XMLA Pivot"),
        [
            "Rows: [Product].[Category].[Category]",
            "Values: [Measures].[Average Price]",
            "Calculated members: [Measures].[Total Sales]",
        ]
    );
    let slicers: Vec<String> = book
        .pages
        .iter()
        .flat_map(|p| &p.visuals)
        .filter(|v| v.visual_type == "slicer" || v.visual_type == "timeline")
        .flat_map(|v| {
            v.wells
                .iter()
                .flat_map(|w| &w.projections)
                .map(move |p| format!("{}: {}", v.visual_type, p.target))
        })
        .collect();
    assert_eq!(
        slicers,
        [
            "slicer: [Customer].[Country].[Country]",
            "timeline: [Date].[Date]"
        ]
    );
    // The CUBE formulas: a set, a ranked member and a value over the set
    // cell and the slicer's name — all covered, so nothing is a notice.
    assert_eq!(
        bindings(&book, "Sheet2", "A20"),
        [
            "CUBESET: [Product].[Category]",
            "CUBESET: [Measures].[Total Sales]"
        ]
    );
    assert!(
        ingested
            .skips
            .iter()
            .all(|skip| skip.kind != SkipKind::NotAnalyzed),
        "{:?}",
        ingested.skips
    );

    let model = model();
    let graph = DependencyGraph::build(&model, &[&book]);
    assert!(
        graph.broken_bindings().is_empty(),
        "{:?}",
        graph.broken_bindings()
    );
    let unused: Vec<ObjectId> = graph.unused_objects().into_iter().map(|u| u.id).collect();
    for live in [
        column("Customer", "Country"),
        column("Customer", "Segment"),
        column("Date", "Date"),
        column("Date", "Year"),
        measure("Sales", "Order Count"),
    ] {
        assert!(!unused.contains(&live), "{live:?} should be live");
    }
}

#[test]
fn mdx_names_resolve_to_columns_hierarchies_and_levels() {
    let model = model();
    let target =
        |dimension: &str, hierarchy: &str, level: Option<&str>, exact: bool| FieldTarget::Mdx {
            dimension: NameKey::new(dimension),
            hierarchy: Some(NameKey::new(hierarchy)),
            level: level.map(NameKey::new),
            exact,
        };
    let book = ReportModel {
        name: Some("Book.xlsx".to_string()),
        filters: vec![ripbi_core::Filter {
            target: Some(target("Date", "Calendar", Some("Month"), true)),
            references: vec![
                // A member name in scanned MDX is not a level, and not broken.
                target("Date", "Calendar", Some("2024"), false),
                // A member key was dropped; the column stays.
                target("Customer", "Segment", None, false),
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let graph = DependencyGraph::build(&model, &[&book]);
    assert!(graph.broken_bindings().is_empty());
    let unused: Vec<ObjectId> = graph.unused_objects().into_iter().map(|u| u.id).collect();
    assert!(!unused.contains(&column("Date", "Month")));
    assert!(!unused.contains(&column("Customer", "Segment")));
    assert!(!unused.contains(&ObjectId::Hierarchy {
        table: NameKey::new("Date"),
        hierarchy: NameKey::new("Calendar"),
    }));

    // An exact name whose level the hierarchy lacks is broken.
    let stale = ReportModel {
        filters: vec![ripbi_core::Filter {
            target: Some(target("Date", "Calendar", Some("Quarter"), true)),
            ..Default::default()
        }],
        ..Default::default()
    };
    let graph = DependencyGraph::build(&model, &[&stale]);
    assert_eq!(graph.broken_bindings().len(), 1);
}

#[test]
fn a_dax_query_table_keeps_what_it_queries() {
    let model = model();
    let book = ReportModel {
        name: Some("Book.xlsx".to_string()),
        queries: vec![ripbi_core::ReportQuery {
            connection: "Sales Query".to_string(),
            text: "DEFINE MEASURE Sales[Local] = 1\nEVALUATE SUMMARIZECOLUMNS('Product'[Size], \"x\", [Order Count], \"y\", [Local])".to_string(),
        }],
        ..Default::default()
    };
    let graph = DependencyGraph::build(&model, &[&book]);
    let unused: Vec<ObjectId> = graph.unused_objects().into_iter().map(|u| u.id).collect();
    assert!(!unused.contains(&column("Product", "Size")));
    assert!(!unused.contains(&measure("Sales", "Order Count")));
    assert!(graph.broken_bindings().is_empty());
    let site = graph
        .roots_of(&column("Product", "Size"))
        .into_iter()
        .find_map(|provenance| match provenance {
            Provenance::Binding(edge) => match &edge.kind {
                BindingSite::Query { connection } => Some(connection.clone()),
                _ => None,
            },
            _ => None,
        });
    assert_eq!(site.as_deref(), Some("Sales Query"));
}
