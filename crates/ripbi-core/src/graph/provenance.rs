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
            Provenance::Binding(edge) => {
                let BindingEdge {
                    kind,
                    report,
                    page,
                    visual,
                    bookmark,
                } = edge.as_ref();
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
            Provenance::M => f.write_str("Power Query expression"),
            Provenance::Structural { role } => write!(f, "{role}"),
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
    /// The role grants access to this table.
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
            }));

            assert_eq!(
                provenance.to_string(),
                "filter — visual 'V1' on page 'P1' in bookmark 'B1'"
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
