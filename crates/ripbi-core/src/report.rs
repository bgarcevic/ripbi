//! Format-agnostic report AST: the normalized shape every report source format
//! (PBIR `definition/` folders, PBIR-Legacy `report.json` Layout) is parsed into.
//!
//! The types here are plain data with no parsing or I/O behaviour. Their only logic
//! is the enumeration at the bottom of this module ([`ReportModel::bindings`] and
//! [`ReportModel::dax_expressions`]), which is the single place that knows where
//! report-side reachability roots and report-owned DAX live. The graph layer
//! consumes those two functions instead of walking the AST itself, so a new
//! binding-bearing field cannot be silently omitted from reachability analysis.
//!
//! Bindings hold references *as written* — structured entity trees in PBIR,
//! written names in legacy Layout — never resolved model objects. Resolution
//! against the semantic model is the graph layer's job, via
//! [`ModelIndex`](crate::ModelIndex); keeping the written form is what lets both
//! source formats populate the same structures.

use std::fmt;

use crate::identity::{FieldRef, NameKey, Quoted};
use crate::model::{DaxExpressionKind, DaxExpressionRef, ExpressionOwner};

/// Normalized report definition, regardless of source format (PBIR, PBIR-Legacy
/// Layout). One instance per report: an analysis runs one
/// [`TabularDatabase`](crate::TabularDatabase) against the reports that share it,
/// and each report's `name` completes its bindings' provenance.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReportModel {
    /// Report identity for provenance: the `.platform` display name, or the report
    /// folder's name when the source records none.
    pub name: Option<String>,
    /// The semantic model this report connects to (PBIR `datasetReference`).
    pub dataset: DatasetReference,
    /// Report-level filters (PBIR `report.json` filterConfig).
    pub filters: Vec<Filter>,
    /// Pages in source order.
    pub pages: Vec<Page>,
    /// Bookmarks in source order.
    pub bookmarks: Vec<Bookmark>,
    /// Report-level measures (PBIR `reportExtensions.json`): DAX that lives in the
    /// report, not the model.
    pub measures: Vec<ReportMeasure>,
}

/// How a report reaches its semantic model (PBIR `datasetReference`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatasetReference {
    /// Relative path to a sibling semantic-model folder (`byPath`). Forward slashes,
    /// never absolute.
    ByPath {
        /// Path as written, e.g. `../Sales.SemanticModel`.
        path: String,
    },
    /// Live connection to a remote semantic model (`byConnection`).
    ByConnection {
        /// Connection string as written.
        connection_string: String,
    },
    /// Absent, unrecognized, or not yet parsed.
    Unresolved,
}

impl Default for DatasetReference {
    /// An unparsed reference is `Unresolved`, never a path or a connection,
    /// mirroring [`PartitionSource::Other`](crate::PartitionSource): schema drift
    /// must never fabricate a report↔model pairing.
    fn default() -> Self {
        Self::Unresolved
    }
}

/// One page of a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// Page object name, e.g. `ReportSectionacd41c847407a998c130`. PBIR keys its
    /// folders and files by it, and bookmarks reference pages by it.
    pub name: NameKey,
    /// Author-facing name, e.g. `Overview`.
    pub display_name: Option<String>,
    /// Hidden pages still bind fields — their visuals render on demand — so this
    /// flag is display-only, never liveness.
    pub is_hidden: bool,
    /// Filters applied to the whole page.
    pub filters: Vec<Filter>,
    /// The page's drillthrough/tooltip role, if it has one.
    pub binding: Option<PageBinding>,
    /// Visuals on the page, in source order.
    pub visuals: Vec<Visual>,
}

/// The role a page plays in drillthrough and tooltips (PBIR `pageBinding.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PageBindingKind {
    /// An ordinary page. The default.
    #[default]
    Default,
    /// Reached via drillthrough; its parameters bind fields.
    Drillthrough,
    /// Rendered as a tooltip for other visuals.
    Tooltip,
}

/// A page's drillthrough/tooltip configuration (PBIR `pageBinding`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageBinding {
    /// What kind of page binding this is.
    pub kind: PageBindingKind,
    /// Fields a drillthrough caller must supply, in source order.
    pub parameters: Vec<DrillthroughParameter>,
}

/// One drillthrough field (PBIR `pageBinding.parameters[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrillthroughParameter {
    /// Parameter name as written, e.g. `Param_Filter5`.
    pub name: Option<NameKey>,
    /// The bound field (PBIR `fieldExpr`).
    pub target: FieldTarget,
}

/// One visual on a page.
///
/// Slicers are not a separate kind: a slicer is a visual with `visual_type`
/// `"slicer"`, and its field wells carry the binding. Saved slicer *selections*
/// are literal values, not references, and are not modeled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visual {
    /// Visual object name — the PBIR folder name and bookmarks' key.
    pub name: NameKey,
    /// Visual type as written, e.g. `"donutChart"`, `"slicer"`, `"card"`.
    pub visual_type: String,
    /// Field wells (PBIR `query.queryState`): role → projections.
    pub wells: Vec<FieldWell>,
    /// Filters applied to this visual only.
    pub filters: Vec<Filter>,
    /// Sort-by fields (PBIR `sortDefinition`), in sort order.
    pub sorts: Vec<FieldTarget>,
    /// Fields driving conditional-formatting rules.
    pub conditional_formatting: Vec<FieldTarget>,
    /// Page used as this visual's tooltip, by page object name. A report-internal
    /// reference: it keeps the page reachable, not a model object.
    pub tooltip_page: Option<NameKey>,
}

/// One field well of a visual: everything projected into a single role.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldWell {
    /// Role name as written, e.g. `"Category"`, `"Y"`, `"Tooltips"`, `"Values"`.
    pub role: String,
    /// Projections in the well, in source order.
    pub projections: Vec<Projection>,
}

/// One field projected into a well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    /// The projected field.
    pub target: FieldTarget,
    /// Written display form as it appears in the file (PBIR `queryRef`), e.g.
    /// `Sales.Customers % of Total`. Diagnostics only — `target` is authoritative.
    pub query_ref: Option<String>,
    /// Whether the projection is active. Inactive projections still bind: they are
    /// one toggle away from live, and dropping them would under-count roots.
    pub active: bool,
}

/// A filter at report, page, visual, or bookmark level.
///
/// The filtered *values* (the condition tree's literals) are data, not references,
/// and are not modeled — only the fields a filter touches can keep objects alive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    /// Filter name within its scope, e.g. `Filter5`. Drillthrough parameters bind
    /// to filters by this name (PBIR `boundFilter`).
    pub name: Option<NameKey>,
    /// The filtered field itself (PBIR `filterConfig.filters[].field`).
    pub target: Option<FieldTarget>,
    /// Further fields referenced by the filter's condition tree, with query aliases
    /// (`SourceRef.Source`) already resolved to entities by the parser.
    pub references: Vec<FieldTarget>,
}

/// A saved exploration state, restorable by a reader.
///
/// Bookmark bindings are enumerated like any other: applying a bookmark re-applies
/// its saved filters and projections, so a field kept alive only by a bookmark is
/// still alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    /// Bookmark object name — the PBIR file name, and the provenance key.
    pub name: NameKey,
    /// Author-facing name.
    pub display_name: Option<String>,
    /// Captured state, per page it spans (usually one).
    pub sections: Vec<BookmarkSection>,
}

/// The slice of a bookmark's state belonging to one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkSection {
    /// The captured page, by object name.
    pub page: NameKey,
    /// Saved filters (`byName` and `byExpr`).
    pub filters: Vec<Filter>,
    /// Saved per-visual state, in source order.
    pub visuals: Vec<BookmarkVisual>,
}

/// A bookmark's saved state for one visual.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkVisual {
    /// The visual, by object name.
    pub visual: NameKey,
    /// Fields active when the bookmark was captured, as wells by role.
    pub wells: Vec<FieldWell>,
}

/// A DAX measure defined in the report (PBIR `reportExtensions.json`), not the
/// model.
///
/// A report measure bridges usage in both directions: its body references model
/// objects (so it is an expression source the graph must consume), and visuals
/// reference it by name (so it is a reachability root of its own). Name lookup
/// should try report measures before model measures — within its report, a report
/// measure shadows a model measure of the same name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportMeasure {
    /// Measure name, unique within its report.
    pub name: NameKey,
    /// The measure's DAX expression.
    pub expression: String,
    /// Dynamic format string (DAX).
    pub format_string: Option<String>,
}

/// A model-object reference as written in a report binding, before resolution.
///
/// PBIR bindings are structured JSON entity trees (`Column`, `Measure`,
/// `HierarchyLevel`, `Aggregation`); legacy Layout binds written names. Both
/// normalize here, so downstream code never branches on source format.
///
/// The column/measure discrimination is kept rather than collapsed into
/// [`FieldRef`], because the binding states it outright and resolution would
/// otherwise be guessing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FieldTarget {
    /// A column, by its table (`SourceRef.Entity`) and name (`Property`).
    Column {
        /// Owning table as written.
        table: NameKey,
        /// Column name as written.
        column: NameKey,
    },
    /// A measure, by name. Measures are model-global; the entity PBIR writes
    /// alongside them is the home table as displayed, carried for provenance only.
    Measure {
        /// Home table as written, if any.
        home_table: Option<NameKey>,
        /// Measure name as written.
        measure: NameKey,
    },
    /// A hierarchy level: the level a visual drills to, which keeps the whole
    /// hierarchy (and its level columns) alive.
    HierarchyLevel {
        /// Owning table as written.
        table: NameKey,
        /// Hierarchy name as written.
        hierarchy: NameKey,
        /// Level name as written.
        level: NameKey,
    },
    /// An aggregation over an inner reference, e.g. Sum of `'Sales'[Units]`.
    /// The inner target is what stays alive; the function is diagnostics.
    Aggregation {
        /// Aggregation function as written, e.g. `"Sum"`.
        function: Option<String>,
        /// The aggregated field.
        inner: Box<FieldTarget>,
    },
    /// A written name the parser could not structure — legacy Layout strings,
    /// unresolved query aliases. Kept anyway: a binding we cannot read is still a
    /// binding, and dropping it would under-count roots.
    Written(FieldRef),
}

impl fmt::Display for FieldTarget {
    /// Human-readable form for diagnostics, quoting names the way [`FieldRef`]
    /// does. The hierarchy-level and aggregation forms are illustrative, not valid
    /// DAX — level references have no DAX syntax.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldTarget::Column { table, column } => {
                write!(f, "{}[{}]", Quoted(table.as_str()), column.as_str())
            }
            FieldTarget::Measure {
                home_table,
                measure,
            } => match home_table {
                Some(table) => write!(f, "{}[{}]", Quoted(table.as_str()), measure.as_str()),
                None => write!(f, "[{}]", measure.as_str()),
            },
            FieldTarget::HierarchyLevel {
                table,
                hierarchy,
                level,
            } => write!(
                f,
                "hierarchy {}[{}] level {}",
                Quoted(table.as_str()),
                hierarchy.as_str(),
                Quoted(level.as_str())
            ),
            FieldTarget::Aggregation { function, inner } => match function {
                Some(function) => write!(f, "{function}({inner})"),
                None => write!(f, "Aggregation({inner})"),
            },
            FieldTarget::Written(reference) => write!(f, "{reference}"),
        }
    }
}

/// What kind of report-side usage a binding represents.
///
/// The kind powers "used by" explanations (`'Sales'[Amount]` ← filter on
/// *Overview* ← page 2) and groups bindings for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingKind<'a> {
    /// A field projected into a visual's field well, with the well's role.
    FieldWell {
        /// Role name as written, e.g. `"Category"`, `"Y"`, `"Tooltips"`.
        role: &'a str,
    },
    /// A filter at report, page, visual, or bookmark level.
    Filter,
    /// A visual's sort-by field.
    Sort,
    /// A drillthrough parameter's bound field.
    Drillthrough,
    /// A field driving a conditional-formatting rule.
    ConditionalFormatting,
}

/// Borrowed view of one report binding, with its provenance.
///
/// The page/visual/bookmark fields answer *where* the binding lives — the `None`s
/// narrow it: a report-level filter has neither page nor visual, a page filter has
/// no visual. Which *report* a binding came from is answered by the
/// [`ReportModel`] it was enumerated from, so the report name is not repeated here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BindingRef<'a> {
    /// Page the binding lives on; `None` for report-level bindings.
    pub page: Option<&'a NameKey>,
    /// Visual the binding lives in; `None` outside a visual.
    pub visual: Option<&'a NameKey>,
    /// Bookmark whose saved state carries the binding; `None` for live bindings.
    pub bookmark: Option<&'a NameKey>,
    /// What kind of binding this is.
    pub kind: BindingKind<'a>,
    /// The model object referenced, as written.
    pub target: &'a FieldTarget,
}

impl ReportModel {
    /// Every model-object reference the report makes, with its provenance — the
    /// reachability roots the graph's BFS starts from.
    ///
    /// Order follows report order (report filters, then per page: drillthrough
    /// parameters, page filters, and each visual's wells, filters, sorts, and
    /// conditional formatting; then per bookmark: section filters and saved wells),
    /// so the result is deterministic for a given report and diffable across runs.
    ///
    /// Everything borrows from the report, so this allocates only the returned
    /// `Vec`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ripbi_core::report::{BindingKind, FieldTarget, Filter, ReportModel};
    /// use ripbi_core::NameKey;
    ///
    /// let report = ReportModel {
    ///     filters: vec![Filter {
    ///         target: Some(FieldTarget::Column {
    ///             table: NameKey::new("Product"),
    ///             column: NameKey::new("Category"),
    ///         }),
    ///         ..Default::default()
    ///     }],
    ///     ..Default::default()
    /// };
    ///
    /// let bindings = report.bindings();
    /// assert_eq!(bindings.len(), 1);
    /// assert_eq!(bindings[0].kind, BindingKind::Filter);
    /// // A report-level filter belongs to no page and no visual.
    /// assert_eq!(bindings[0].page, None);
    /// assert_eq!(bindings[0].visual, None);
    /// ```
    #[must_use]
    pub fn bindings(&self) -> Vec<BindingRef<'_>> {
        let mut out = Vec::new();

        for filter in &self.filters {
            extend_with_filter(&mut out, None, None, None, filter);
        }

        for page in &self.pages {
            let page_id = Some(&page.name);

            if let Some(binding) = &page.binding {
                for parameter in &binding.parameters {
                    out.push(BindingRef {
                        page: page_id,
                        visual: None,
                        bookmark: None,
                        kind: BindingKind::Drillthrough,
                        target: &parameter.target,
                    });
                }
            }

            for filter in &page.filters {
                extend_with_filter(&mut out, page_id, None, None, filter);
            }

            for visual in &page.visuals {
                let visual_id = Some(&visual.name);
                extend_with_wells(&mut out, page_id, visual_id, None, &visual.wells);

                for filter in &visual.filters {
                    extend_with_filter(&mut out, page_id, visual_id, None, filter);
                }

                for target in &visual.sorts {
                    out.push(BindingRef {
                        page: page_id,
                        visual: visual_id,
                        bookmark: None,
                        kind: BindingKind::Sort,
                        target,
                    });
                }

                for target in &visual.conditional_formatting {
                    out.push(BindingRef {
                        page: page_id,
                        visual: visual_id,
                        bookmark: None,
                        kind: BindingKind::ConditionalFormatting,
                        target,
                    });
                }
            }
        }

        for bookmark in &self.bookmarks {
            let bookmark_id = Some(&bookmark.name);

            for section in &bookmark.sections {
                let page_id = Some(&section.page);

                for filter in &section.filters {
                    extend_with_filter(&mut out, page_id, None, bookmark_id, filter);
                }

                for visual in &section.visuals {
                    extend_with_wells(
                        &mut out,
                        page_id,
                        Some(&visual.visual),
                        bookmark_id,
                        &visual.wells,
                    );
                }
            }
        }

        out
    }

    /// Every DAX expression defined report-side — report-level measures — on top of
    /// [`TabularDatabase::dax_expressions`](crate::TabularDatabase::dax_expressions),
    /// which covers the model side.
    ///
    /// A report measure has no home table, so `home_table` is always `None`:
    /// unqualified `[Name]` references in its body can only be measures — of the
    /// report first, then the model.
    ///
    /// Owners borrow their names, so this allocates only the returned `Vec`.
    #[must_use]
    pub fn dax_expressions(&self) -> Vec<DaxExpressionRef<'_>> {
        let mut out = Vec::new();

        for measure in &self.measures {
            let owner = ExpressionOwner::ReportMeasure {
                measure: measure.name.as_str(),
            };
            out.push(DaxExpressionRef {
                owner,
                kind: DaxExpressionKind::ReportMeasure,
                home_table: None,
                text: &measure.expression,
            });
            if let Some(text) = &measure.format_string {
                out.push(DaxExpressionRef {
                    owner,
                    kind: DaxExpressionKind::ReportMeasureFormatString,
                    home_table: None,
                    text,
                });
            }
        }

        out
    }
}

/// Appends one [`BindingRef`] per field a filter carries, all tagged
/// [`BindingKind::Filter`]: the declared `target` first, then the condition tree's
/// `references`, preserving file order for stable diffs.
fn extend_with_filter<'a>(
    out: &mut Vec<BindingRef<'a>>,
    page: Option<&'a NameKey>,
    visual: Option<&'a NameKey>,
    bookmark: Option<&'a NameKey>,
    filter: &'a Filter,
) {
    for target in filter.target.iter().chain(&filter.references) {
        out.push(BindingRef {
            page,
            visual,
            bookmark,
            kind: BindingKind::Filter,
            target,
        });
    }
}

/// Appends one [`BindingRef`] per projection in the wells, tagged with its well's
/// role. Inactive projections bind too: they are one toggle away from live.
fn extend_with_wells<'a>(
    out: &mut Vec<BindingRef<'a>>,
    page: Option<&'a NameKey>,
    visual: Option<&'a NameKey>,
    bookmark: Option<&'a NameKey>,
    wells: &'a [FieldWell],
) {
    for well in wells {
        for projection in &well.projections {
            out.push(BindingRef {
                page,
                visual,
                bookmark,
                kind: BindingKind::FieldWell {
                    role: well.role.as_str(),
                },
                target: &projection.target,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn column_target(table: &str, column: &str) -> FieldTarget {
        FieldTarget::Column {
            table: NameKey::new(table),
            column: NameKey::new(column),
        }
    }

    fn measure_target(home_table: Option<&str>, measure: &str) -> FieldTarget {
        FieldTarget::Measure {
            home_table: home_table.map(NameKey::new),
            measure: NameKey::new(measure),
        }
    }

    fn filter_on(target: FieldTarget) -> Filter {
        Filter {
            target: Some(target),
            ..Default::default()
        }
    }

    fn page(name: &str) -> Page {
        Page {
            name: NameKey::new(name),
            display_name: None,
            is_hidden: false,
            filters: Vec::new(),
            binding: None,
            visuals: Vec::new(),
        }
    }

    fn visual(name: &str, visual_type: &str) -> Visual {
        Visual {
            name: NameKey::new(name),
            visual_type: visual_type.to_string(),
            wells: Vec::new(),
            filters: Vec::new(),
            sorts: Vec::new(),
            conditional_formatting: Vec::new(),
            tooltip_page: None,
        }
    }

    fn well(role: &str, targets: &[FieldTarget]) -> FieldWell {
        FieldWell {
            role: role.to_string(),
            projections: targets
                .iter()
                .cloned()
                .map(|target| Projection {
                    target,
                    query_ref: None,
                    active: true,
                })
                .collect(),
        }
    }

    mod field_target {
        use super::*;

        #[rstest]
        #[case::column(column_target("Product", "Category"), "'Product'[Category]")]
        #[case::measure_with_home_table(measure_target(Some("Sales"), "Cost"), "'Sales'[Cost]")]
        #[case::measure_without_home_table(measure_target(None, "Cost"), "[Cost]")]
        #[case::hierarchy_level(
            FieldTarget::HierarchyLevel {
                table: NameKey::new("Accounts"),
                hierarchy: NameKey::new("Street Hierarchy"),
                level: NameKey::new("State or Province"),
            },
            "hierarchy 'Accounts'[Street Hierarchy] level 'State or Province'"
        )]
        #[case::aggregation(
            FieldTarget::Aggregation {
                function: Some("Sum".to_string()),
                inner: Box::new(column_target("Sales", "Units")),
            },
            "Sum('Sales'[Units])"
        )]
        #[case::aggregation_without_function(
            FieldTarget::Aggregation {
                function: None,
                inner: Box::new(column_target("Sales", "Units")),
            },
            "Aggregation('Sales'[Units])"
        )]
        #[case::written(
            FieldTarget::Written(FieldRef {
                table: Some(NameKey::new("Sales")),
                name: NameKey::new("Amount"),
            }),
            "'Sales'[Amount]"
        )]
        fn displays_for_diagnostics(#[case] target: FieldTarget, #[case] expected: &str) {
            assert_eq!(target.to_string(), expected);
        }

        #[test]
        fn compares_equal_ignoring_case() {
            assert_eq!(
                column_target("Product", "Category"),
                column_target("PRODUCT", "CATEGORY")
            );
            assert_eq!(
                measure_target(Some("Sales"), "Cost"),
                measure_target(Some("sales"), "COST")
            );
        }

        /// The binding states column-or-measure outright; losing it would make
        /// resolution guess where it currently knows.
        #[test]
        fn distinguishes_a_column_from_a_measure_with_the_same_names() {
            assert_ne!(
                column_target("Sales", "Cost"),
                measure_target(Some("Sales"), "Cost")
            );
        }
    }

    mod bindings {
        use super::*;

        /// The common report identity; each test adds the binding site it checks.
        fn sample() -> ReportModel {
            ReportModel {
                name: Some("Sales overview".to_string()),
                dataset: DatasetReference::ByPath {
                    path: "../Sales.SemanticModel".to_string(),
                },
                ..Default::default()
            }
        }

        fn sample_with_page(page: Page) -> ReportModel {
            ReportModel {
                pages: vec![page],
                ..sample()
            }
        }

        /// One binding's full provenance: page, visual, bookmark, kind, and the
        /// target as `Display` — what resolution consumes.
        type Provenance<'a> = (
            Option<&'a str>,
            Option<&'a str>,
            Option<&'a str>,
            BindingKind<'a>,
            String,
        );

        /// `Provenance` per binding, in enumeration order — the full
        /// specification `bindings()` must satisfy.
        fn provenance(report: &ReportModel) -> Vec<Provenance<'_>> {
            report
                .bindings()
                .into_iter()
                .map(|binding| {
                    (
                        binding.page.map(NameKey::as_str),
                        binding.visual.map(NameKey::as_str),
                        binding.bookmark.map(NameKey::as_str),
                        binding.kind,
                        binding.target.to_string(),
                    )
                })
                .collect()
        }

        #[test]
        fn a_report_filter_has_no_page_visual_or_bookmark() {
            let report = ReportModel {
                filters: vec![filter_on(column_target("Product", "Category"))],
                ..sample()
            };
            let bindings = report.bindings();

            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].page, None);
            assert_eq!(bindings[0].visual, None);
            assert_eq!(bindings[0].bookmark, None);
            assert_eq!(bindings[0].kind, BindingKind::Filter);
        }

        #[test]
        fn a_drillthrough_parameter_is_tagged_on_its_page() {
            let report = sample_with_page(Page {
                binding: Some(PageBinding {
                    kind: PageBindingKind::Drillthrough,
                    parameters: vec![DrillthroughParameter {
                        name: Some(NameKey::new("Param_Filter5")),
                        target: column_target("Industries", "Industry"),
                    }],
                }),
                ..page("ReportSection1")
            });

            let bindings = report.bindings();
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].page.unwrap().as_str(), "ReportSection1");
            assert_eq!(bindings[0].visual, None);
            assert_eq!(bindings[0].kind, BindingKind::Drillthrough);
        }

        #[test]
        fn a_page_filter_carries_its_page_but_no_visual() {
            let report = sample_with_page(Page {
                filters: vec![filter_on(column_target("Owners", "Sales owner"))],
                ..page("ReportSection1")
            });

            let bindings = report.bindings();
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].page.unwrap().as_str(), "ReportSection1");
            assert_eq!(bindings[0].visual, None);
            assert_eq!(bindings[0].kind, BindingKind::Filter);
        }

        #[test]
        fn a_visual_well_carries_role_page_and_visual() {
            let report = sample_with_page(Page {
                visuals: vec![Visual {
                    wells: vec![well("Category", &[column_target("Product", "Category")])],
                    ..visual("visual1", "donutChart")
                }],
                ..page("ReportSection1")
            });

            let bindings = report.bindings();
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].page.unwrap().as_str(), "ReportSection1");
            assert_eq!(bindings[0].visual.unwrap().as_str(), "visual1");
            assert_eq!(bindings[0].bookmark, None);
            assert_eq!(
                bindings[0].kind,
                BindingKind::FieldWell { role: "Category" }
            );
        }

        #[test]
        fn sorts_and_conditional_formatting_are_tagged_as_such() {
            let report = sample_with_page(Page {
                visuals: vec![Visual {
                    sorts: vec![column_target("Product", "Category")],
                    conditional_formatting: vec![measure_target(Some("Sales"), "Margin")],
                    ..visual("visual1", "tableEx")
                }],
                ..page("ReportSection1")
            });

            let bindings = report.bindings();
            assert_eq!(
                bindings.iter().map(|b| b.kind).collect::<Vec<_>>(),
                vec![BindingKind::Sort, BindingKind::ConditionalFormatting]
            );
            // Both bindings belong to their visual, at page level.
            for binding in &bindings {
                assert_eq!(binding.page.unwrap().as_str(), "ReportSection1");
                assert_eq!(binding.visual.unwrap().as_str(), "visual1");
            }
        }

        #[test]
        fn a_bookmark_filter_carries_bookmark_and_page() {
            let report = ReportModel {
                bookmarks: vec![Bookmark {
                    name: NameKey::new("Bookmark1"),
                    display_name: Some("FY24".to_string()),
                    sections: vec![BookmarkSection {
                        page: NameKey::new("ReportSection1"),
                        filters: vec![filter_on(column_target("Products", "Product category"))],
                        visuals: Vec::new(),
                    }],
                }],
                ..sample()
            };

            let bindings = report.bindings();
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].page.unwrap().as_str(), "ReportSection1");
            assert_eq!(bindings[0].visual, None);
            assert_eq!(bindings[0].bookmark.unwrap().as_str(), "Bookmark1");
            assert_eq!(bindings[0].kind, BindingKind::Filter);
        }

        #[test]
        fn bookmark_wells_carry_bookmark_page_and_visual() {
            let report = ReportModel {
                bookmarks: vec![Bookmark {
                    name: NameKey::new("Bookmark1"),
                    display_name: None,
                    sections: vec![BookmarkSection {
                        page: NameKey::new("ReportSection1"),
                        filters: Vec::new(),
                        visuals: vec![BookmarkVisual {
                            visual: NameKey::new("visual1"),
                            wells: vec![well("Rows", &[column_target("Product", "Subcategory")])],
                        }],
                    }],
                }],
                ..sample()
            };

            let bindings = report.bindings();
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].page.unwrap().as_str(), "ReportSection1");
            assert_eq!(bindings[0].visual.unwrap().as_str(), "visual1");
            assert_eq!(bindings[0].bookmark.unwrap().as_str(), "Bookmark1");
            assert_eq!(bindings[0].kind, BindingKind::FieldWell { role: "Rows" });
        }

        /// The declared target comes first, then the condition tree's references,
        /// in file order.
        #[test]
        fn a_filter_yields_target_then_references_in_order() {
            let report = sample_with_page(Page {
                visuals: vec![Visual {
                    filters: vec![Filter {
                        name: Some(NameKey::new("Filter5")),
                        target: Some(column_target("Product", "Category")),
                        references: vec![
                            column_target("Product", "Subcategory"),
                            measure_target(None, "Units"),
                        ],
                    }],
                    ..visual("visual1", "donutChart")
                }],
                ..page("ReportSection1")
            });

            let targets: Vec<&FieldTarget> =
                report.bindings().into_iter().map(|b| b.target).collect();
            assert_eq!(
                targets,
                vec![
                    &column_target("Product", "Category"),
                    &column_target("Product", "Subcategory"),
                    &measure_target(None, "Units"),
                ]
            );
        }

        /// A filter the parser could not give a structured target still binds:
        /// its references are roots even when `target` is `None`.
        #[test]
        fn a_filter_without_a_target_still_binds_its_references() {
            let report = sample_with_page(Page {
                visuals: vec![Visual {
                    filters: vec![Filter {
                        name: Some(NameKey::new("Filter5")),
                        target: None,
                        references: vec![
                            column_target("Product", "Subcategory"),
                            measure_target(None, "Units"),
                        ],
                    }],
                    ..visual("visual1", "donutChart")
                }],
                ..page("ReportSection1")
            });

            let targets: Vec<&FieldTarget> =
                report.bindings().into_iter().map(|b| b.target).collect();
            assert_eq!(
                targets,
                vec![
                    &column_target("Product", "Subcategory"),
                    &measure_target(None, "Units"),
                ]
            );
        }

        /// An inactive projection is one toggle away from live; dropping it would
        /// under-count roots and report live code as unused.
        #[test]
        fn an_inactive_projection_still_binds() {
            let report = sample_with_page(Page {
                visuals: vec![Visual {
                    wells: vec![FieldWell {
                        role: "Y".to_string(),
                        projections: vec![Projection {
                            target: column_target("Sales", "Units"),
                            query_ref: None,
                            active: false,
                        }],
                    }],
                    ..visual("visual1", "lineChart")
                }],
                ..page("ReportSection1")
            });

            assert_eq!(report.bindings().len(), 1);
        }

        /// Hidden is not dead: a hidden page's visuals render on demand, so their
        /// wells bind like any other page's. Skipping hidden pages would
        /// under-count roots and report live code as unused.
        #[test]
        fn a_hidden_pages_visuals_still_bind() {
            let report = sample_with_page(Page {
                is_hidden: true,
                visuals: vec![Visual {
                    wells: vec![well("Values", &[column_target("Sales", "Units")])],
                    ..visual("visual1", "card")
                }],
                ..page("ReportSection1")
            });

            let bindings = report.bindings();
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].page.unwrap().as_str(), "ReportSection1");
            assert_eq!(bindings[0].kind, BindingKind::FieldWell { role: "Values" });
        }

        /// Enumeration walks report order — report filters, page (parameters,
        /// filters, visuals: wells, filters, sorts, conditional formatting), then
        /// bookmarks — so runs are diffable. Every binding's full provenance is
        /// pinned, not just its kind: a slipped page, visual, or bookmark on any
        /// site must fail here.
        #[test]
        fn order_follows_report_structure() {
            let report = ReportModel {
                filters: vec![filter_on(column_target("Product", "Category"))],
                pages: vec![
                    Page {
                        binding: Some(PageBinding {
                            kind: PageBindingKind::Drillthrough,
                            parameters: vec![DrillthroughParameter {
                                name: None,
                                target: column_target("Industries", "Industry"),
                            }],
                        }),
                        filters: vec![filter_on(column_target("Owners", "Sales owner"))],
                        visuals: vec![Visual {
                            wells: vec![well("Category", &[column_target("Product", "Category")])],
                            filters: vec![filter_on(column_target("Region", "Country"))],
                            sorts: vec![measure_target(Some("Sales"), "Sales")],
                            conditional_formatting: vec![measure_target(Some("Sales"), "Margin")],
                            ..visual("visual1", "donutChart")
                        }],
                        ..page("ReportSection1")
                    },
                    Page {
                        visuals: vec![Visual {
                            wells: vec![well(
                                "Tooltips",
                                &[measure_target(Some("Sales"), "Customers %")],
                            )],
                            ..visual("visual2", "slicer")
                        }],
                        ..page("ReportSection2")
                    },
                ],
                bookmarks: vec![Bookmark {
                    name: NameKey::new("Bookmark1"),
                    display_name: None,
                    sections: vec![BookmarkSection {
                        page: NameKey::new("ReportSection1"),
                        filters: vec![filter_on(column_target("Products", "Product category"))],
                        visuals: vec![BookmarkVisual {
                            visual: NameKey::new("visual1"),
                            wells: vec![well("Rows", &[column_target("Product", "Subcategory")])],
                        }],
                    }],
                }],
                measures: Vec::new(),
                ..sample()
            };

            assert_eq!(
                provenance(&report),
                vec![
                    // Report filter.
                    (
                        None,
                        None,
                        None,
                        BindingKind::Filter,
                        "'Product'[Category]".to_string(),
                    ),
                    // Page 1 drillthrough parameter.
                    (
                        Some("ReportSection1"),
                        None,
                        None,
                        BindingKind::Drillthrough,
                        "'Industries'[Industry]".to_string(),
                    ),
                    // Page 1 filter.
                    (
                        Some("ReportSection1"),
                        None,
                        None,
                        BindingKind::Filter,
                        "'Owners'[Sales owner]".to_string(),
                    ),
                    // Visual 1 well.
                    (
                        Some("ReportSection1"),
                        Some("visual1"),
                        None,
                        BindingKind::FieldWell { role: "Category" },
                        "'Product'[Category]".to_string(),
                    ),
                    // Visual 1 filter.
                    (
                        Some("ReportSection1"),
                        Some("visual1"),
                        None,
                        BindingKind::Filter,
                        "'Region'[Country]".to_string(),
                    ),
                    // Visual 1 sort.
                    (
                        Some("ReportSection1"),
                        Some("visual1"),
                        None,
                        BindingKind::Sort,
                        "'Sales'[Sales]".to_string(),
                    ),
                    // Visual 1 conditional formatting.
                    (
                        Some("ReportSection1"),
                        Some("visual1"),
                        None,
                        BindingKind::ConditionalFormatting,
                        "'Sales'[Margin]".to_string(),
                    ),
                    // Visual 2 well.
                    (
                        Some("ReportSection2"),
                        Some("visual2"),
                        None,
                        BindingKind::FieldWell { role: "Tooltips" },
                        "'Sales'[Customers %]".to_string(),
                    ),
                    // Bookmark filter.
                    (
                        Some("ReportSection1"),
                        None,
                        Some("Bookmark1"),
                        BindingKind::Filter,
                        "'Products'[Product category]".to_string(),
                    ),
                    // Bookmark well.
                    (
                        Some("ReportSection1"),
                        Some("visual1"),
                        Some("Bookmark1"),
                        BindingKind::FieldWell { role: "Rows" },
                        "'Product'[Subcategory]".to_string(),
                    ),
                ]
            );
        }
    }

    /// The same field, bound through PBIR's structured entities and through legacy
    /// Layout's written names, must enumerate to the same provenance: this
    /// equivalence is the AST's whole reason to exist.
    mod format_agnostic {
        use super::*;

        fn report_with(well_target: FieldTarget) -> ReportModel {
            ReportModel {
                pages: vec![Page {
                    visuals: vec![Visual {
                        wells: vec![well("Y", &[well_target])],
                        ..visual("visual1", "clusteredColumnChart")
                    }],
                    ..page("ReportSection1")
                }],
                ..Default::default()
            }
        }

        #[test]
        fn structured_and_written_targets_bind_alike() {
            let pbir = report_with(FieldTarget::Measure {
                home_table: Some(NameKey::new("Sales")),
                measure: NameKey::new("Cost"),
            });
            let legacy = report_with(FieldTarget::Written(FieldRef {
                table: Some(NameKey::new("Sales")),
                name: NameKey::new("Cost"),
            }));

            let pbir_bindings = pbir.bindings();
            let legacy_bindings = legacy.bindings();
            assert_eq!(pbir_bindings.len(), 1);
            assert_eq!(legacy_bindings.len(), 1);

            // Provenance and kind are identical; only the target's variant differs.
            assert_eq!(pbir_bindings[0].page, legacy_bindings[0].page);
            assert_eq!(pbir_bindings[0].visual, legacy_bindings[0].visual);
            assert_eq!(pbir_bindings[0].bookmark, legacy_bindings[0].bookmark);
            assert_eq!(pbir_bindings[0].kind, legacy_bindings[0].kind);
        }
    }

    mod dax_expressions {
        use super::*;

        #[test]
        fn enumerates_a_report_measures_body_and_format_string() {
            let report = ReportModel {
                measures: vec![ReportMeasure {
                    name: NameKey::new("Growth %"),
                    expression: "DIVIDE([Sales] - [Prior Sales], [Prior Sales])".to_string(),
                    format_string: Some("0.0%;-0.0%;0.0%".to_string()),
                }],
                ..Default::default()
            };

            let expressions = report.dax_expressions();
            assert_eq!(expressions.len(), 2);

            assert_eq!(
                expressions[0],
                DaxExpressionRef {
                    owner: ExpressionOwner::ReportMeasure {
                        measure: "Growth %"
                    },
                    kind: DaxExpressionKind::ReportMeasure,
                    home_table: None,
                    text: "DIVIDE([Sales] - [Prior Sales], [Prior Sales])",
                }
            );
            assert_eq!(
                expressions[1],
                DaxExpressionRef {
                    owner: ExpressionOwner::ReportMeasure {
                        measure: "Growth %"
                    },
                    kind: DaxExpressionKind::ReportMeasureFormatString,
                    home_table: None,
                    text: "0.0%;-0.0%;0.0%",
                }
            );
        }

        /// Most report measures carry no dynamic format string; only the body is
        /// then an expression source.
        #[test]
        fn a_measure_without_a_format_string_enumerates_only_its_body() {
            let report = ReportModel {
                measures: vec![ReportMeasure {
                    name: NameKey::new("Total Units"),
                    expression: "SUM('Sales'[Units])".to_string(),
                    format_string: None,
                }],
                ..Default::default()
            };

            let expressions = report.dax_expressions();
            assert_eq!(expressions.len(), 1);
            assert_eq!(expressions[0].kind, DaxExpressionKind::ReportMeasure);
        }

        #[test]
        fn a_report_without_measures_has_none() {
            assert!(ReportModel::default().dax_expressions().is_empty());
        }

        /// The owner is the measure's reachability identity: visuals reference it,
        /// its body references model objects, and the graph node must match both.
        /// Compared through `Display`: `ObjectId` equality ignores case, so it
        /// could never catch a lowercased or rewritten name.
        #[test]
        fn owner_materializes_a_report_measure_object_id() {
            let owner = ExpressionOwner::ReportMeasure {
                measure: "Growth %",
            };
            assert_eq!(
                owner.to_object_id().to_string(),
                "report measure 'Growth %'"
            );
        }
    }

    mod expression_views {
        use super::*;

        /// Mirrors the model-side guarantee: bindings enumerate without allocating
        /// beyond the returned `Vec`, which holds only while every field borrows.
        #[test]
        fn are_copy_so_enumeration_borrows_everything() {
            fn assert_copy<T: Copy>() {}
            assert_copy::<BindingRef<'_>>();
            assert_copy::<BindingKind<'_>>();
        }
    }

    mod defaults {
        use super::*;

        /// An unparsed dataset reference must never masquerade as a path or a
        /// connection, or a wrong report↔model pairing would reach the graph.
        #[test]
        fn a_dataset_reference_is_unresolved() {
            assert_eq!(DatasetReference::default(), DatasetReference::Unresolved);
            assert_eq!(ReportModel::default().dataset, DatasetReference::Unresolved);
        }

        /// Most pages are plain pages; PBIR omits the binding for them entirely.
        #[test]
        fn a_page_binding_kind_is_default() {
            assert_eq!(PageBindingKind::default(), PageBindingKind::Default);
            assert_eq!(PageBinding::default().kind, PageBindingKind::Default);
        }
    }
}
