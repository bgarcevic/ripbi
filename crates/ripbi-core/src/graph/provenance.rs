//! Per-edge provenance: what kind of use every dependency edge records.
//!
//! Provenance is first-class graph data, stored as the petgraph edge weight at
//! build time — never derived at render time. The planned `ripbi deps` view
//! consumes it straight off [`consumers_of`](super::DependencyGraph::consumers_of)
//! to annotate edges (`visual 'Card' on page 'P2'`, `RLS role 'Reader' filter`),
//! and [`scan`](super) uses it to explain why an unused object is referenced only
//! by other unused objects.

use std::fmt;

use crate::identity::{NameKey, Quoted};
use crate::model::DaxExpressionKind;

/// Why one object depends on another — the edge weight of the dependency graph.
///
/// The report-binding payload is boxed so the enum stays small: it is cloned
/// once per deduped edge, and the large variant would otherwise dominate every
/// edge weight's size.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// The target is referenced inside a DAX expression. The edge's source node
    /// plus `kind` identify the expression site exactly: every
    /// `(owner, kind)` pair has exactly one production site in the expression
    /// enumerations.
    Dax {
        /// Which property of the source object the expression came from.
        kind: DaxExpressionKind,
    },
    /// The target is bound by a report: a field well, filter, sort, drillthrough
    /// parameter, or conditional-formatting rule.
    Binding(
        /// Which binding, and where it lives.
        Box<BindingEdge>,
    ),
    /// The target is referenced from an M expression by name.
    M,
    /// Liveness flows through model structure, with no written reference anywhere.
    Structural {
        /// Which structural rule produces the edge.
        role: StructuralEdge,
    },
}

/// One report binding and the site it lives in — the payload of
/// [`Provenance::Binding`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingEdge {
    /// What the binding does.
    pub kind: BindingSite,
    /// The report carrying the binding, when the source recorded a name.
    pub report: Option<NameKey>,
    /// The page the binding lives on; `None` for report-level bindings.
    pub page: Option<NameKey>,
    /// The visual the binding lives in; `None` outside visuals.
    pub visual: Option<NameKey>,
    /// The bookmark whose saved state carries the binding; `None` for live
    /// bindings.
    pub bookmark: Option<NameKey>,
    /// Whether the binding lives in the phone layout (`definition.mobile/`)
    /// rather than the desktop tree. Provenance only — both layouts bind
    /// identically — but it tells a user auditing a survivor which surface to
    /// look at (issue #49).
    pub mobile: bool,
}

impl fmt::Display for BindingEdge {
    /// Human-readable site description, e.g.
    /// `field well 'Y' — visual 'V1' on page 'P1' in report 'Mini'`. The same
    /// phrase a [`Provenance::Binding`] renders, published on the edge itself
    /// so findings that carry a binding without being a graph edge (the
    /// broken-visual records) render identically.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let BindingEdge {
            kind,
            report,
            page,
            visual,
            bookmark,
            mobile,
        } = self;
        if *mobile {
            f.write_str("mobile layout ")?;
        }
        write_site(f, kind)?;
        if let Some(visual) = visual {
            write!(f, " — visual {}", Quoted(visual.as_str()))?;
        }
        if let Some(page) = page {
            write!(f, " on page {}", Quoted(page.as_str()))?;
        }
        if let Some(bookmark) = bookmark {
            write!(f, " in bookmark {}", Quoted(bookmark.as_str()))?;
        }
        if let Some(report) = report {
            write!(f, " in report {}", Quoted(report.as_str()))?;
        }
        Ok(())
    }
}

impl Provenance {
    /// True when the strong reachability pass may traverse this edge — every
    /// edge except relationship endpoints (which keep a key column alive
    /// without keeping its table alive) and inactive-relationship references
    /// (which never confer liveness — see the module docs of [`super`]).
    pub(super) fn is_strong_pass_edge(&self) -> bool {
        !matches!(
            self,
            Provenance::Structural {
                role: StructuralEdge::RelationshipEndpoint
            } | Provenance::Structural {
                role: StructuralEdge::InactiveRelationship
            } | Provenance::Structural {
                role: StructuralEdge::InactiveRelationshipEndpoint
            }
        )
    }

    /// True when the weak reachability pass may traverse this edge — every
    /// edge except containment from a table member to its table, so liveness
    /// gained weakly can never propagate into a table, and except the
    /// inactive-relationship edges, which never confer liveness in any pass:
    /// only a live `USERELATIONSHIP` reference can activate the relationship.
    pub(super) fn is_weak_pass_edge(&self) -> bool {
        !matches!(
            self,
            Provenance::Structural {
                role: StructuralEdge::TableMember
            } | Provenance::Structural {
                role: StructuralEdge::InactiveRelationship
            } | Provenance::Structural {
                role: StructuralEdge::InactiveRelationshipEndpoint
            }
        )
    }
}

impl fmt::Display for Provenance {
    /// Human-readable site description for "used by" lines, e.g.
    /// `field well 'Y' — visual 'V1', page 'P1', report 'Mini'` or `RLS filter`.
    /// The edge's *source* object is rendered by the [`ObjectId`](crate::ObjectId)
    /// at the other half of the pair, so a provenance never repeats it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Provenance::Dax { kind } => f.write_str(dax_site(*kind)),
            Provenance::Binding(edge) => edge.fmt(f),
            Provenance::M => f.write_str("Power Query expression"),
            Provenance::Structural { role } => write!(f, "{role}"),
        }
    }
}

impl Provenance {
    /// The stable snake_case machine key for this relationship — the same
    /// relationship [`Display`](fmt::Display) renders as a phrase. Structured
    /// output (`ripbi deps --plain`/`--json`) emits this so scripts can match on
    /// it; the phrase is for humans and carries punctuation that never belongs
    /// in a pipeline field.
    pub fn key(&self) -> &'static str {
        match self {
            Provenance::Dax { kind } => kind.key(),
            Provenance::Binding(_) => "binding",
            Provenance::M => "power_query",
            Provenance::Structural { role } => role.key(),
        }
    }
}

impl BindingSite {
    /// The stable machine key of this binding kind. A field well is
    /// `visual_binding` — the role (`Values`, `Y`, …) is separate data, not part
    /// of the kind.
    pub fn key(&self) -> &'static str {
        match self {
            BindingSite::FieldWell { .. } => "visual_binding",
            BindingSite::Filter => "filter",
            BindingSite::Sort => "sort",
            BindingSite::Drillthrough => "drillthrough",
            BindingSite::ConditionalFormatting => "conditional_formatting",
            BindingSite::AltText => "alt_text",
        }
    }
}

impl StructuralEdge {
    /// The stable machine key of this structural rule, in lockstep with
    /// [`Display`](fmt::Display).
    pub fn key(&self) -> &'static str {
        match self {
            StructuralEdge::TableMember => "table_member",
            StructuralEdge::TablePartition => "table_partition",
            StructuralEdge::Relationship => "relationship",
            StructuralEdge::InactiveRelationship => "inactive_relationship",
            StructuralEdge::RelationshipEndpoint => "relationship_endpoint",
            StructuralEdge::InactiveRelationshipEndpoint => "inactive_relationship_endpoint",
            StructuralEdge::SortByColumn => "sort_by_column",
            StructuralEdge::GroupByColumn => "group_by_column",
            StructuralEdge::HierarchyLevel => "hierarchy_level",
            StructuralEdge::EngineManaged => "engine_managed",
            StructuralEdge::MParameterBinding => "m_parameter_binding",
            StructuralEdge::RolePermission => "role_permission",
        }
    }
}

impl DaxExpressionKind {
    /// The stable machine key of this expression site, in lockstep with the
    /// `dax_site` phrase table.
    pub fn key(&self) -> &'static str {
        match self {
            DaxExpressionKind::Measure => "measure_expression",
            DaxExpressionKind::MeasureFormatString => "measure_format_string",
            DaxExpressionKind::MeasureDetailRows => "measure_detail_rows",
            DaxExpressionKind::KpiTarget => "kpi_target",
            DaxExpressionKind::KpiStatus => "kpi_status",
            DaxExpressionKind::KpiTrend => "kpi_trend",
            DaxExpressionKind::CalculatedColumn => "calculated_column_expression",
            DaxExpressionKind::CalculatedTable => "calculated_table_expression",
            DaxExpressionKind::ChangeDetection => "change_detection_expression",
            DaxExpressionKind::TableDetailRows => "table_detail_rows",
            DaxExpressionKind::RlsFilter => "rls_filter",
            DaxExpressionKind::CalculationItem => "calculation_item_expression",
            DaxExpressionKind::CalculationItemFormatString => "calculation_item_format_string",
            DaxExpressionKind::CalculationGroupNoSelection => "calculation_group_no_selection",
            DaxExpressionKind::CalculationGroupNoSelectionFormatString => {
                "calculation_group_no_selection_format_string"
            }
            DaxExpressionKind::CalculationGroupMultipleOrEmptySelection => {
                "calculation_group_multiple_or_empty_selection"
            }
            DaxExpressionKind::CalculationGroupMultipleOrEmptySelectionFormatString => {
                "calculation_group_multiple_or_empty_selection_format_string"
            }
            DaxExpressionKind::Function => "function_body",
            DaxExpressionKind::ReportMeasure => "report_measure_expression",
            DaxExpressionKind::ReportMeasureFormatString => "report_measure_format_string",
        }
    }
}

/// What kind of report-side usage a binding represents — the owned form of
/// [`BindingKind`](crate::BindingKind), whose field-well role is borrowed from
/// the report AST.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BindingSite {
    /// A field projected into a visual's field well.
    FieldWell {
        /// Role name as written, e.g. `"Category"`, `"Y"`.
        role: String,
    },
    /// A filter at report, page, visual, or bookmark level.
    Filter,
    /// A visual's sort-by field.
    Sort,
    /// A drillthrough parameter's bound field.
    Drillthrough,
    /// A field driving a conditional-formatting rule.
    ConditionalFormatting,
    /// A visual's accessibility alt text.
    AltText,
}

fn write_site(f: &mut fmt::Formatter<'_>, site: &BindingSite) -> fmt::Result {
    match site {
        BindingSite::FieldWell { role } => {
            write!(f, "field well {}", Quoted(role.as_str()))
        }
        BindingSite::Filter => f.write_str("filter"),
        BindingSite::Sort => f.write_str("sort definition"),
        BindingSite::Drillthrough => f.write_str("drillthrough parameter"),
        BindingSite::ConditionalFormatting => f.write_str("conditional formatting"),
        BindingSite::AltText => f.write_str("alt text"),
    }
}

impl fmt::Display for BindingSite {
    /// The site phrase on its own — what a view renders when the binding's
    /// location is shown structurally (report → page → visual levels of its
    /// own) and only the kind of use remains for the leaf.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_site(f, self)
    }
}

/// The structural rule an edge came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StructuralEdge {
    /// The source is defined on the target table; a used member keeps its table
    /// alive.
    TableMember,
    /// The table loads its rows through this partition.
    TablePartition,
    /// The relationship hangs off this table; either endpoint keeps it alive.
    Relationship,
    /// An inactive relationship hangs off this table, but the reference is
    /// recorded without liveness: switching an inactive relationship on at
    /// query time is DAX's job (`USERELATIONSHIP`), so an unactivated one is
    /// itself a finding.
    InactiveRelationship,
    /// The relationship needs this key column.
    RelationshipEndpoint,
    /// An inactive relationship names this key column, but activating it at
    /// query time is DAX's job (`USERELATIONSHIP`): the edge is recorded so
    /// findings can point at the relationship, yet it confers no liveness —
    /// until a live DAX reference switches the relationship on, the key is
    /// unloadable bloat.
    InactiveRelationshipEndpoint,
    /// The source column is sorted by the target column.
    SortByColumn,
    /// The source column is grouped by the target column.
    GroupByColumn,
    /// The hierarchy drills down through this column.
    HierarchyLevel,
    /// The column is materialized by the engine together with its table
    /// (calculated-table columns, calculation-group columns, calendar columns)
    /// and cannot be dropped independently.
    EngineManaged,
    /// A dynamic M query parameter is bound to this column
    /// (`parameterValuesColumn`): at view time the report feeds the column's
    /// values into the parameter, so a consumed parameter keeps its bound
    /// column alive (issue #50).
    MParameterBinding,
    /// The role grants access to this table, or holds an object-level
    /// permission on this column — granting or revoking (`none`) both count,
    /// since dropping either object would break the role.
    RolePermission,
}

impl fmt::Display for StructuralEdge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            StructuralEdge::TableMember => "table member",
            StructuralEdge::TablePartition => "table partition",
            StructuralEdge::Relationship => "relationship",
            StructuralEdge::InactiveRelationship => "inactive relationship",
            StructuralEdge::RelationshipEndpoint => "relationship endpoint",
            StructuralEdge::InactiveRelationshipEndpoint => "inactive relationship endpoint",
            StructuralEdge::SortByColumn => "sort-by column",
            StructuralEdge::GroupByColumn => "group-by column",
            StructuralEdge::HierarchyLevel => "hierarchy level",
            StructuralEdge::EngineManaged => "engine-managed column",
            StructuralEdge::MParameterBinding => "dynamic M parameter binding",
            StructuralEdge::RolePermission => "role permission",
        })
    }
}

/// The site phrase for a DAX expression kind. The edge's source object names the
/// owner; this only says which property of it made the reference.
fn dax_site(kind: DaxExpressionKind) -> &'static str {
    match kind {
        DaxExpressionKind::Measure => "measure expression",
        DaxExpressionKind::MeasureFormatString => "measure format string",
        DaxExpressionKind::MeasureDetailRows => "measure detail rows",
        DaxExpressionKind::KpiTarget => "KPI target",
        DaxExpressionKind::KpiStatus => "KPI status",
        DaxExpressionKind::KpiTrend => "KPI trend",
        DaxExpressionKind::CalculatedColumn => "calculated column expression",
        DaxExpressionKind::CalculatedTable => "calculated table expression",
        DaxExpressionKind::ChangeDetection => "change detection expression",
        DaxExpressionKind::TableDetailRows => "table detail rows",
        DaxExpressionKind::RlsFilter => "RLS filter",
        DaxExpressionKind::CalculationItem => "calculation item expression",
        DaxExpressionKind::CalculationItemFormatString => "calculation item format string",
        DaxExpressionKind::CalculationGroupNoSelection => "no-selection expression",
        DaxExpressionKind::CalculationGroupNoSelectionFormatString => "no-selection format string",
        DaxExpressionKind::CalculationGroupMultipleOrEmptySelection => {
            "multiple-or-empty-selection expression"
        }
        DaxExpressionKind::CalculationGroupMultipleOrEmptySelectionFormatString => {
            "multiple-or-empty-selection format string"
        }
        DaxExpressionKind::Function => "function body",
        DaxExpressionKind::ReportMeasure => "report measure expression",
        DaxExpressionKind::ReportMeasureFormatString => "report measure format string",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(kind: BindingSite) -> Provenance {
        Provenance::Binding(Box::new(BindingEdge {
            kind,
            report: Some(NameKey::new("Mini")),
            page: Some(NameKey::new("P1")),
            visual: Some(NameKey::new("V1")),
            bookmark: None,
            mobile: false,
        }))
    }

    mod display {
        use super::*;

        #[test]
        fn a_field_well_renders_its_site_chain() {
            assert_eq!(
                binding(BindingSite::FieldWell {
                    role: "Y".to_string(),
                })
                .to_string(),
                "field well 'Y' — visual 'V1' on page 'P1' in report 'Mini'"
            );
        }

        #[test]
        fn a_bookmark_follows_the_visual_it_saved() {
            let provenance = Provenance::Binding(Box::new(BindingEdge {
                kind: BindingSite::Filter,
                report: None,
                page: Some(NameKey::new("P1")),
                visual: Some(NameKey::new("V1")),
                bookmark: Some(NameKey::new("B1")),
                mobile: false,
            }));

            assert_eq!(
                provenance.to_string(),
                "filter — visual 'V1' on page 'P1' in bookmark 'B1'"
            );
        }

        /// A phone-layout binding says so up front: a user auditing why a field
        /// survived needs to know which surface to look at (issue #49).
        #[test]
        fn a_mobile_layout_binding_says_so() {
            let provenance = Provenance::Binding(Box::new(BindingEdge {
                kind: BindingSite::FieldWell {
                    role: "Values".to_string(),
                },
                report: Some(NameKey::new("Mini")),
                page: Some(NameKey::new("P1")),
                visual: Some(NameKey::new("V1")),
                bookmark: None,
                mobile: true,
            }));

            assert_eq!(
                provenance.to_string(),
                "mobile layout field well 'Values' — visual 'V1' on page 'P1' in report 'Mini'"
            );
        }

        #[test]
        fn a_report_level_filter_names_no_site() {
            let provenance = Provenance::Binding(Box::new(BindingEdge {
                kind: BindingSite::Filter,
                report: None,
                page: None,
                visual: None,
                bookmark: None,
                mobile: false,
            }));

            assert_eq!(provenance.to_string(), "filter");
        }

        #[test]
        fn structural_and_m_sites_render_as_phrases() {
            assert_eq!(
                Provenance::Structural {
                    role: StructuralEdge::RelationshipEndpoint
                }
                .to_string(),
                "relationship endpoint"
            );
            assert_eq!(
                Provenance::Structural {
                    role: StructuralEdge::InactiveRelationship
                }
                .to_string(),
                "inactive relationship"
            );
            assert_eq!(Provenance::M.to_string(), "Power Query expression");
        }

        #[test]
        fn dax_sites_render_as_phrases() {
            assert_eq!(
                Provenance::Dax {
                    kind: DaxExpressionKind::RlsFilter
                }
                .to_string(),
                "RLS filter"
            );
            assert_eq!(
                Provenance::Dax {
                    kind: DaxExpressionKind::Measure
                }
                .to_string(),
                "measure expression"
            );
        }
    }

    mod keys {
        use std::collections::HashSet;

        use super::*;

        /// Every variant has a distinct snake_case key: structured output keys
        /// records by these, so a collision would silently merge two
        /// relationships.
        #[test]
        fn keys_within_each_family_are_distinct() {
            let dax = [
                DaxExpressionKind::Measure,
                DaxExpressionKind::MeasureFormatString,
                DaxExpressionKind::MeasureDetailRows,
                DaxExpressionKind::KpiTarget,
                DaxExpressionKind::KpiStatus,
                DaxExpressionKind::KpiTrend,
                DaxExpressionKind::CalculatedColumn,
                DaxExpressionKind::CalculatedTable,
                DaxExpressionKind::ChangeDetection,
                DaxExpressionKind::TableDetailRows,
                DaxExpressionKind::RlsFilter,
                DaxExpressionKind::CalculationItem,
                DaxExpressionKind::CalculationItemFormatString,
                DaxExpressionKind::CalculationGroupNoSelection,
                DaxExpressionKind::CalculationGroupNoSelectionFormatString,
                DaxExpressionKind::CalculationGroupMultipleOrEmptySelection,
                DaxExpressionKind::CalculationGroupMultipleOrEmptySelectionFormatString,
                DaxExpressionKind::Function,
                DaxExpressionKind::ReportMeasure,
                DaxExpressionKind::ReportMeasureFormatString,
            ];
            let structural = [
                StructuralEdge::TableMember,
                StructuralEdge::TablePartition,
                StructuralEdge::Relationship,
                StructuralEdge::InactiveRelationship,
                StructuralEdge::RelationshipEndpoint,
                StructuralEdge::InactiveRelationshipEndpoint,
                StructuralEdge::SortByColumn,
                StructuralEdge::GroupByColumn,
                StructuralEdge::HierarchyLevel,
                StructuralEdge::EngineManaged,
                StructuralEdge::MParameterBinding,
                StructuralEdge::RolePermission,
            ];
            let sites = [
                BindingSite::FieldWell {
                    role: String::new(),
                },
                BindingSite::Filter,
                BindingSite::Sort,
                BindingSite::Drillthrough,
                BindingSite::ConditionalFormatting,
                BindingSite::AltText,
            ];

            for (family, keys) in [
                ("dax", dax.iter().map(|kind| kind.key()).collect::<Vec<_>>()),
                (
                    "structural",
                    structural.iter().map(|role| role.key()).collect::<Vec<_>>(),
                ),
                (
                    "sites",
                    sites.iter().map(|site| site.key()).collect::<Vec<_>>(),
                ),
            ] {
                let mut sorted = keys.clone();
                sorted.sort_unstable();
                assert_eq!(
                    keys.len(),
                    sorted.iter().collect::<HashSet<_>>().len(),
                    "{family} keys must be distinct"
                );
                assert!(
                    keys.iter().all(|key| {
                        !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    }),
                    "{family} keys must be snake_case"
                );
            }
        }

        #[test]
        fn provenance_keys_delegate_to_their_family() {
            assert_eq!(
                Provenance::Dax {
                    kind: DaxExpressionKind::RlsFilter
                }
                .key(),
                "rls_filter"
            );
            assert_eq!(
                Provenance::Structural {
                    role: StructuralEdge::HierarchyLevel
                }
                .key(),
                "hierarchy_level"
            );
            assert_eq!(Provenance::M.key(), "power_query");
            assert_eq!(
                binding(BindingSite::FieldWell {
                    role: "Values".to_string(),
                })
                .key(),
                "binding"
            );
        }

        /// A field well's role never leaks into the machine kind: the role is
        /// separate data (`Values`, `Y`, …), the kind is always `visual_binding`.
        #[test]
        fn a_field_well_keys_as_visual_binding() {
            assert_eq!(
                BindingSite::FieldWell {
                    role: "Values".to_string(),
                }
                .key(),
                "visual_binding"
            );
        }
    }

    mod classification {
        use super::*;

        #[test]
        fn the_strong_pass_excludes_relationship_endpoints_and_inactive_relationships() {
            let endpoint = Provenance::Structural {
                role: StructuralEdge::RelationshipEndpoint,
            };
            let inactive = Provenance::Structural {
                role: StructuralEdge::InactiveRelationship,
            };
            let inactive_key = Provenance::Structural {
                role: StructuralEdge::InactiveRelationshipEndpoint,
            };
            let member = Provenance::Structural {
                role: StructuralEdge::TableMember,
            };

            assert!(!endpoint.is_strong_pass_edge());
            assert!(!inactive.is_strong_pass_edge());
            assert!(!inactive_key.is_strong_pass_edge());
            assert!(member.is_strong_pass_edge());
            assert!(Provenance::M.is_strong_pass_edge());
            assert!(
                Provenance::Dax {
                    kind: DaxExpressionKind::Measure
                }
                .is_strong_pass_edge()
            );
        }

        #[test]
        fn the_weak_pass_excludes_containment_and_the_inactive_relationship_edges() {
            let endpoint = Provenance::Structural {
                role: StructuralEdge::RelationshipEndpoint,
            };
            let inactive = Provenance::Structural {
                role: StructuralEdge::InactiveRelationship,
            };
            let inactive_key = Provenance::Structural {
                role: StructuralEdge::InactiveRelationshipEndpoint,
            };
            let member = Provenance::Structural {
                role: StructuralEdge::TableMember,
            };

            assert!(!member.is_weak_pass_edge());
            assert!(endpoint.is_weak_pass_edge());
            // The whole point of the inactive variants: the relationship and
            // its endpoints are recorded references, never sources of
            // liveness. Only a live USERELATIONSHIP call (a Dax edge) can
            // activate the relationship.
            assert!(!inactive.is_weak_pass_edge());
            assert!(!inactive_key.is_weak_pass_edge());
            assert!(Provenance::M.is_weak_pass_edge());
        }
    }
}
