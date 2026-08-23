//! Format-agnostic tabular AST: the normalized shape every source format
//! (TMDL, TMSL `model.bim`, `.pbix` `DataModelSchema`) is parsed into.
//!
//! The types here are plain data with no parsing or I/O behaviour. Their only logic is
//! the expression enumeration at the bottom of this module
//! ([`TabularDatabase::dax_expressions`] and [`TabularDatabase::m_expressions`]), which
//! is the single place that knows where expressions live. The graph layer consumes those
//! two functions instead of walking the AST itself, so a new expression-bearing field
//! cannot be silently omitted from reachability analysis.
//!
//! Name-based lookup lives in the [`index`] submodule; the handle accessors on
//! [`TabularDatabase`] ([`table`](TabularDatabase::table),
//! [`column`](TabularDatabase::column), … and [`object_id`](TabularDatabase::object_id))
//! turn the handles it hands out back into borrowed AST nodes.
//!
//! String fields hold names with their original casing and compare case-sensitively.
//! Case-insensitive comparison is the job of [`crate::identity::NameKey`], which these
//! names are converted into when they become graph nodes.

pub mod index;

use crate::identity::{NameKey, ObjectId};
use crate::model::index::{
    ColumnHandle, ExpressionHandle, HierarchyHandle, MeasureHandle, Resolved, TableHandle,
};

/// Normalized semantic model, regardless of source format (TMDL, model.bim,
/// .pbix DataModelSchema). Downstream code never branches on source format.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TabularDatabase {
    /// Model name, when the source format records one.
    pub name: Option<String>,
    /// Tables in source order.
    pub tables: Vec<Table>,
    /// Relationships between table columns.
    pub relationships: Vec<Relationship>,
    /// Row-level-security roles.
    pub roles: Vec<Role>,
    /// Model-level shared M expressions (TMDL expressions.tmdl / TMSL
    /// model.expressions): Power Query parameters and shared queries.
    pub expressions: Vec<SharedExpression>,
}

/// A table and everything defined on it.
///
/// A calculation-group table carries its synthetic columns (the group's field column
/// and its ordinal column) in `columns` like any other table; nothing distinguishes
/// them structurally from data columns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    /// Table name.
    pub name: String,
    /// Columns in source order.
    pub columns: Vec<Column>,
    /// Measures whose home table this is.
    pub measures: Vec<Measure>,
    /// Partitions supplying the table's rows.
    pub partitions: Vec<Partition>,
    /// User-defined hierarchies.
    pub hierarchies: Vec<Hierarchy>,
    /// In TOM a calculation group is a property of a table.
    pub calculation_group: Option<CalculationGroup>,
    /// DAX defaultDetailRowsDefinition (drillthrough detail rows).
    pub detail_rows_expression: Option<String>,
    /// Hidden from report authors; hidden objects are still live if referenced.
    pub is_hidden: bool,
}

impl Table {
    /// A calculated table is a table whose partition source is DAX.
    pub fn is_calculated(&self) -> bool {
        self.partitions
            .iter()
            .any(|partition| matches!(partition.source, PartitionSource::Calculated { .. }))
    }
}

/// A column of a table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Column {
    /// Column name.
    pub name: String,
    /// How the column's values are produced.
    pub kind: ColumnKind,
    /// Hidden from report authors; hidden objects are still live if referenced.
    pub is_hidden: bool,
    /// Name of another column in the same table (TOM sortByColumn).
    /// Liveness edge: a used column keeps its sort-by column alive.
    pub sort_by_column: Option<String>,
}

/// How a column's values are produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ColumnKind {
    /// Sourced from the partition query (TOM dataColumn). The default.
    #[default]
    Data,
    /// DAX-defined column (TOM calculatedColumn).
    Calculated {
        /// DAX expression evaluated per row.
        expression: String,
    },
    /// Column of a calculated table (TOM calculatedTableColumn);
    /// materialized by the table's DAX partition, no own expression.
    CalculatedTableColumn,
}

/// A DAX measure.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Measure {
    /// Measure name; unique across the whole model, not just its home table.
    pub name: String,
    /// The measure's DAX expression.
    pub expression: String,
    /// Hidden from report authors; hidden objects are still live if referenced.
    pub is_hidden: bool,
    /// Dynamic format string (DAX).
    pub format_string_expression: Option<String>,
    /// DAX detailRowsDefinition (drillthrough detail rows).
    pub detail_rows_expression: Option<String>,
    /// KPI attached to this measure.
    pub kpi: Option<Kpi>,
}

/// KPI expressions are DAX and can be the sole reference keeping an object alive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Kpi {
    /// DAX expression for the KPI target value.
    pub target_expression: Option<String>,
    /// DAX expression for the KPI status.
    pub status_expression: Option<String>,
    /// DAX expression for the KPI trend.
    pub trend_expression: Option<String>,
}

/// A partition supplying a table's rows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Partition {
    /// Partition name.
    pub name: String,
    /// The partition's source query and its language.
    pub source: PartitionSource,
}

/// A partition's source query, discriminated by query language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartitionSource {
    /// Power Query (TOM m).
    M {
        /// M expression text.
        expression: String,
    },
    /// DAX — this is what makes a table a calculated table (TOM calculated).
    Calculated {
        /// DAX expression producing the table.
        expression: String,
    },
    /// Legacy native query partition (TOM query).
    Query {
        /// Native query text, in the data source's own dialect.
        query: String,
    },
    /// entity (DirectLake), inferred, future kinds — schema drift never panics;
    /// the raw kind string is kept for diagnostics.
    Other {
        /// The source kind as written in the model, when one was present.
        kind: Option<String>,
    },
}

impl Default for PartitionSource {
    /// An unparsed source is `Other`, never a query language, so an unrecognized
    /// partition can never be mistaken for DAX or M by the expression enumeration.
    fn default() -> Self {
        PartitionSource::Other { kind: None }
    }
}

/// A relationship between a column of one table and a column of another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    /// TMDL relationship names are GUIDs; kept for diagnostics only.
    pub name: Option<String>,
    /// Table on the "from" (typically many) side.
    pub from_table: String,
    /// Key column in `from_table`.
    pub from_column: String,
    /// Table on the "to" (typically one) side.
    pub to_table: String,
    /// Key column in `to_table`.
    pub to_column: String,
    /// Inactive relationships still keep key columns alive (USERELATIONSHIP);
    /// the flag exists for reporting/linting, not liveness.
    pub is_active: bool,
}

impl Default for Relationship {
    /// `is_active` defaults to `true`, matching TOM: the flag is omitted from the
    /// source for active relationships. A derived `Default` would make every
    /// relationship built field-by-field silently inactive.
    fn default() -> Self {
        Self {
            name: None,
            from_table: String::new(),
            from_column: String::new(),
            to_table: String::new(),
            to_column: String::new(),
            is_active: true,
        }
    }
}

/// A user-defined hierarchy on a table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hierarchy {
    /// Hierarchy name.
    pub name: String,
    /// Levels from coarsest to finest, in source order.
    pub levels: Vec<HierarchyLevel>,
    /// Hidden from report authors; hidden objects are still live if referenced.
    pub is_hidden: bool,
}

/// One level of a hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HierarchyLevel {
    /// Level name; may differ from the underlying column name.
    pub name: String,
    /// Column name in the owning table.
    pub column: String,
}

/// A row-level-security role.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Role {
    /// Role name.
    pub name: String,
    /// Per-table permissions granted by this role.
    pub table_permissions: Vec<TablePermission>,
}

/// A role's permission on one table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TablePermission {
    /// Target table name.
    pub table: String,
    /// DAX row filter; None = metadata-only permission.
    pub filter_expression: Option<String>,
}

/// The calculation group defined on a table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CalculationGroup {
    /// Calculation items in source order.
    pub items: Vec<CalculationItem>,
}

/// One item of a calculation group.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CalculationItem {
    /// Item name.
    pub name: String,
    /// The item's DAX expression, typically wrapping SELECTEDMEASURE().
    pub expression: String,
    /// Dynamic format string (DAX) applied when this item is selected.
    pub format_string_expression: Option<String>,
}

/// A model-level shared M expression: a Power Query parameter or shared query.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SharedExpression {
    /// Expression name, as referenced from other M queries.
    pub name: String,
    /// M expression text.
    pub expression: String,
}

/// Handle dereferencing: turning a positional handle from
/// [`ModelIndex`](index::ModelIndex) back into the object it points at.
///
/// Every accessor goes through `.get()` and returns [`Option`]. A handle is only
/// meaningful against the database its index was built from, and a handle from a
/// different or since-mutated database is a normal miss, never a panic.
impl TabularDatabase {
    /// The table a handle points at, or `None` if the handle is stale.
    pub fn table(&self, h: TableHandle) -> Option<&Table> {
        self.tables.get(h.0)
    }

    /// The column a handle points at, or `None` if either index is out of range.
    pub fn column(&self, h: ColumnHandle) -> Option<&Column> {
        self.tables.get(h.table)?.columns.get(h.column)
    }

    /// The measure a handle points at, or `None` if either index is out of range.
    pub fn measure(&self, h: MeasureHandle) -> Option<&Measure> {
        self.tables.get(h.table)?.measures.get(h.measure)
    }

    /// The hierarchy a handle points at, or `None` if either index is out of range.
    pub fn hierarchy(&self, h: HierarchyHandle) -> Option<&Hierarchy> {
        self.tables.get(h.table)?.hierarchies.get(h.hierarchy)
    }

    /// The shared M expression a handle points at, or `None` if the handle is stale.
    pub fn shared_expression(&self, h: ExpressionHandle) -> Option<&SharedExpression> {
        self.expressions.get(h.0)
    }

    /// The stable graph-node identity of a resolved reference.
    ///
    /// Names come from the objects themselves, so the id carries the model's own
    /// casing for display; [`ObjectId`] still compares case-insensitively.
    pub fn object_id(&self, r: Resolved) -> Option<ObjectId> {
        match r {
            Resolved::Column(h) => {
                let table = self.tables.get(h.table)?;
                let column = table.columns.get(h.column)?;
                Some(ObjectId::Column {
                    table: NameKey::new(table.name.clone()),
                    column: NameKey::new(column.name.clone()),
                })
            }
            Resolved::Measure(h) => {
                let table = self.tables.get(h.table)?;
                let measure = table.measures.get(h.measure)?;
                Some(ObjectId::Measure {
                    table: NameKey::new(table.name.clone()),
                    measure: NameKey::new(measure.name.clone()),
                })
            }
        }
    }
}

/// Which model property a DAX expression came from.
///
/// The graph layer matches on this to decide what kind of edge a discovered
/// reference produces; the enumeration in [`TabularDatabase::dax_expressions`]
/// guarantees every variant has exactly one production site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DaxExpressionKind {
    /// A measure's own expression.
    Measure,
    /// A measure's dynamic format string.
    MeasureFormatString,
    /// A measure's detail-rows (drillthrough) expression.
    MeasureDetailRows,
    /// A KPI's target expression.
    KpiTarget,
    /// A KPI's status expression.
    KpiStatus,
    /// A KPI's trend expression.
    KpiTrend,
    /// A calculated column's expression.
    CalculatedColumn,
    /// The DAX partition expression that materializes a calculated table.
    CalculatedTable,
    /// A table's default detail-rows (drillthrough) expression.
    TableDetailRows,
    /// A role's row-level-security filter on one table.
    RlsFilter,
    /// A calculation item's expression.
    CalculationItem,
    /// A calculation item's dynamic format string.
    CalculationItemFormatString,
}

/// Borrowed view of one DAX expression owned by a model object.
#[derive(Debug, Clone)]
pub struct DaxExpressionRef<'a> {
    /// The object the expression belongs to — the source node of any edge derived
    /// from references found in `text`.
    pub owner: ObjectId,
    /// Which property of `owner` this expression is.
    pub kind: DaxExpressionKind,
    /// Context table for unqualified-column resolution by the lexer.
    pub home_table: Option<&'a str>,
    /// The expression text, borrowed from the model.
    pub text: &'a str,
}

/// Borrowed view of one M expression owned by a model object.
#[derive(Debug, Clone)]
pub struct MExpressionRef<'a> {
    /// The object the expression belongs to: a partition or a shared expression.
    pub owner: ObjectId,
    /// The expression text, borrowed from the model.
    pub text: &'a str,
}

impl TabularDatabase {
    /// Every DAX expression in the model, with owner identity and home-table context.
    ///
    /// Order follows model order (tables, then each table's measures, columns,
    /// partitions, table-level expressions and calculation items, then roles), so the
    /// result is deterministic for a given model and diffable across runs.
    pub fn dax_expressions(&self) -> Vec<DaxExpressionRef<'_>> {
        let mut out = Vec::new();

        for table in &self.tables {
            let home = Some(table.name.as_str());

            for measure in &table.measures {
                let owner = ObjectId::Measure {
                    table: NameKey::new(table.name.clone()),
                    measure: NameKey::new(measure.name.clone()),
                };
                out.push(DaxExpressionRef {
                    owner: owner.clone(),
                    kind: DaxExpressionKind::Measure,
                    home_table: home,
                    text: measure.expression.as_str(),
                });
                if let Some(text) = &measure.format_string_expression {
                    out.push(DaxExpressionRef {
                        owner: owner.clone(),
                        kind: DaxExpressionKind::MeasureFormatString,
                        home_table: home,
                        text,
                    });
                }
                if let Some(text) = &measure.detail_rows_expression {
                    out.push(DaxExpressionRef {
                        owner: owner.clone(),
                        kind: DaxExpressionKind::MeasureDetailRows,
                        home_table: home,
                        text,
                    });
                }
                if let Some(kpi) = &measure.kpi {
                    for (kind, text) in [
                        (DaxExpressionKind::KpiTarget, &kpi.target_expression),
                        (DaxExpressionKind::KpiStatus, &kpi.status_expression),
                        (DaxExpressionKind::KpiTrend, &kpi.trend_expression),
                    ] {
                        if let Some(text) = text {
                            out.push(DaxExpressionRef {
                                owner: owner.clone(),
                                kind,
                                home_table: home,
                                text,
                            });
                        }
                    }
                }
            }

            for column in &table.columns {
                if let ColumnKind::Calculated { expression } = &column.kind {
                    out.push(DaxExpressionRef {
                        owner: ObjectId::Column {
                            table: NameKey::new(table.name.clone()),
                            column: NameKey::new(column.name.clone()),
                        },
                        kind: DaxExpressionKind::CalculatedColumn,
                        home_table: home,
                        text: expression,
                    });
                }
            }

            for partition in &table.partitions {
                if let PartitionSource::Calculated { expression } = &partition.source {
                    // The home table is the calculated table itself. Unqualified columns
                    // in a calculated-table expression usually belong to the source
                    // table, so this is conservative: it can only add candidate edges.
                    out.push(DaxExpressionRef {
                        owner: ObjectId::Partition {
                            table: NameKey::new(table.name.clone()),
                            partition: NameKey::new(partition.name.clone()),
                        },
                        kind: DaxExpressionKind::CalculatedTable,
                        home_table: home,
                        text: expression,
                    });
                }
            }

            if let Some(text) = &table.detail_rows_expression {
                out.push(DaxExpressionRef {
                    owner: ObjectId::Table {
                        table: NameKey::new(table.name.clone()),
                    },
                    kind: DaxExpressionKind::TableDetailRows,
                    home_table: home,
                    text,
                });
            }

            if let Some(group) = &table.calculation_group {
                for item in &group.items {
                    let owner = ObjectId::CalculationItem {
                        table: NameKey::new(table.name.clone()),
                        item: NameKey::new(item.name.clone()),
                    };
                    out.push(DaxExpressionRef {
                        owner: owner.clone(),
                        kind: DaxExpressionKind::CalculationItem,
                        home_table: home,
                        text: item.expression.as_str(),
                    });
                    if let Some(text) = &item.format_string_expression {
                        out.push(DaxExpressionRef {
                            owner,
                            kind: DaxExpressionKind::CalculationItemFormatString,
                            home_table: home,
                            text,
                        });
                    }
                }
            }
        }

        for role in &self.roles {
            for permission in &role.table_permissions {
                if let Some(text) = &permission.filter_expression {
                    // The row context of an RLS filter is the table it is applied to,
                    // not anything owned by the role.
                    out.push(DaxExpressionRef {
                        owner: ObjectId::Role {
                            role: NameKey::new(role.name.clone()),
                        },
                        kind: DaxExpressionKind::RlsFilter,
                        home_table: Some(permission.table.as_str()),
                        text,
                    });
                }
            }
        }

        out
    }

    /// Every M expression: M partitions plus shared model expressions.
    ///
    /// `Query` and `Other` partition sources are not M and are excluded.
    pub fn m_expressions(&self) -> Vec<MExpressionRef<'_>> {
        let mut out = Vec::new();

        for table in &self.tables {
            for partition in &table.partitions {
                if let PartitionSource::M { expression } = &partition.source {
                    out.push(MExpressionRef {
                        owner: ObjectId::Partition {
                            table: NameKey::new(table.name.clone()),
                            partition: NameKey::new(partition.name.clone()),
                        },
                        text: expression,
                    });
                }
            }
        }

        for expression in &self.expressions {
            out.push(MExpressionRef {
                owner: ObjectId::Expression {
                    name: NameKey::new(expression.name.clone()),
                },
                text: expression.expression.as_str(),
            });
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_id(table: &str) -> ObjectId {
        ObjectId::Table {
            table: NameKey::new(table),
        }
    }

    fn column_id(table: &str, column: &str) -> ObjectId {
        ObjectId::Column {
            table: NameKey::new(table),
            column: NameKey::new(column),
        }
    }

    fn measure_id(table: &str, measure: &str) -> ObjectId {
        ObjectId::Measure {
            table: NameKey::new(table),
            measure: NameKey::new(measure),
        }
    }

    fn partition_id(table: &str, partition: &str) -> ObjectId {
        ObjectId::Partition {
            table: NameKey::new(table),
            partition: NameKey::new(partition),
        }
    }

    fn calc_item_id(table: &str, item: &str) -> ObjectId {
        ObjectId::CalculationItem {
            table: NameKey::new(table),
            item: NameKey::new(item),
        }
    }

    /// `(kind, owner, home_table, text)` for every DAX expression, in order.
    fn dax_tuples(db: &TabularDatabase) -> Vec<(DaxExpressionKind, ObjectId, Option<&str>, &str)> {
        db.dax_expressions()
            .into_iter()
            .map(|e| (e.kind, e.owner, e.home_table, e.text))
            .collect()
    }

    /// `(owner, text)` for every M expression, in order.
    fn m_tuples(db: &TabularDatabase) -> Vec<(ObjectId, &str)> {
        db.m_expressions()
            .into_iter()
            .map(|e| (e.owner, e.text))
            .collect()
    }

    fn partition(name: &str, source: PartitionSource) -> Partition {
        Partition {
            name: name.to_string(),
            source,
        }
    }

    // --- Task 1: is_calculated -------------------------------------------------

    #[test]
    fn is_calculated_is_true_only_for_a_dax_partition() {
        let calculated = Table {
            name: "Top Products".to_string(),
            partitions: vec![partition(
                "Top Products",
                PartitionSource::Calculated {
                    expression: "TOPN(10, 'Sales')".to_string(),
                },
            )],
            ..Default::default()
        };
        assert!(calculated.is_calculated());

        let m_sourced = Table {
            name: "Sales".to_string(),
            partitions: vec![partition(
                "Sales-Part1",
                PartitionSource::M {
                    expression: "let Source = Sql.Database() in Source".to_string(),
                },
            )],
            ..Default::default()
        };
        assert!(!m_sourced.is_calculated());

        let direct_lake = Table {
            name: "Facts".to_string(),
            partitions: vec![partition(
                "Facts",
                PartitionSource::Other {
                    kind: Some("entity".to_string()),
                },
            )],
            ..Default::default()
        };
        assert!(!direct_lake.is_calculated());

        let partitionless = Table {
            name: "Time Intelligence".to_string(),
            ..Default::default()
        };
        assert!(!partitionless.is_calculated());
    }

    #[test]
    fn is_calculated_is_true_when_only_one_of_several_partitions_is_dax() {
        let mixed = Table {
            name: "Sales".to_string(),
            partitions: vec![
                partition(
                    "Sales-2023",
                    PartitionSource::Query {
                        query: "SELECT * FROM dbo.Sales".to_string(),
                    },
                ),
                partition(
                    "Sales-2024",
                    PartitionSource::Calculated {
                        expression: "FILTER('Raw', TRUE())".to_string(),
                    },
                ),
            ],
            ..Default::default()
        };
        assert!(mixed.is_calculated());
    }

    // --- Tasks 2 and 3: defaults -----------------------------------------------

    #[test]
    fn relationship_default_is_active() {
        let relationship = Relationship::default();
        assert!(relationship.is_active);
        assert_eq!(relationship.name, None);
        assert_eq!(relationship.from_table, "");
        assert_eq!(relationship.from_column, "");
        assert_eq!(relationship.to_table, "");
        assert_eq!(relationship.to_column, "");
    }

    #[test]
    fn partition_source_default_is_other_with_no_kind() {
        assert_eq!(
            PartitionSource::default(),
            PartitionSource::Other { kind: None }
        );
        assert_eq!(
            Partition::default().source,
            PartitionSource::Other { kind: None }
        );
    }

    #[test]
    fn column_kind_default_is_data() {
        assert_eq!(ColumnKind::default(), ColumnKind::Data);
        assert_eq!(Column::default().kind, ColumnKind::Data);
    }

    // --- Task 4: dax_expressions -----------------------------------------------

    /// Exercises every [`DaxExpressionKind`] exactly once, plus three objects that
    /// must contribute nothing: a `Data` column, an M partition, and a
    /// metadata-only table permission.
    fn every_kind_fixture() -> TabularDatabase {
        TabularDatabase {
            name: Some("Contoso".to_string()),
            tables: vec![
                Table {
                    name: "Sales".to_string(),
                    columns: vec![
                        Column {
                            name: "Amount".to_string(),
                            kind: ColumnKind::Data,
                            ..Default::default()
                        },
                        Column {
                            name: "Margin".to_string(),
                            kind: ColumnKind::Calculated {
                                expression: "'Sales'[Amount] * 0.2".to_string(),
                            },
                            ..Default::default()
                        },
                    ],
                    measures: vec![Measure {
                        name: "Total Sales".to_string(),
                        expression: "SUM('Sales'[Amount])".to_string(),
                        is_hidden: false,
                        format_string_expression: Some("\"#,##0\"".to_string()),
                        detail_rows_expression: Some("SELECTCOLUMNS('Sales')".to_string()),
                        kpi: Some(Kpi {
                            target_expression: Some("[Budget]".to_string()),
                            status_expression: Some("IF([Total Sales] > 0, 1, -1)".to_string()),
                            trend_expression: Some("[Total Sales] - [Prior]".to_string()),
                        }),
                    }],
                    partitions: vec![partition(
                        "Sales-Part1",
                        PartitionSource::M {
                            expression: "let Source = Sql.Database() in Source".to_string(),
                        },
                    )],
                    detail_rows_expression: Some(
                        "SELECTCOLUMNS('Sales', \"A\", [Amount])".to_string(),
                    ),
                    ..Default::default()
                },
                Table {
                    name: "Top Products".to_string(),
                    partitions: vec![partition(
                        "Top Products",
                        PartitionSource::Calculated {
                            expression: "TOPN(10, 'Product', [Total Sales])".to_string(),
                        },
                    )],
                    ..Default::default()
                },
                Table {
                    name: "Time Intelligence".to_string(),
                    calculation_group: Some(CalculationGroup {
                        items: vec![CalculationItem {
                            name: "YTD".to_string(),
                            expression: "TOTALYTD(SELECTEDMEASURE(), 'Date'[Date])".to_string(),
                            format_string_expression: Some("\"#,##0;;\"".to_string()),
                        }],
                    }),
                    ..Default::default()
                },
            ],
            roles: vec![Role {
                name: "Reader".to_string(),
                table_permissions: vec![
                    TablePermission {
                        table: "Sales".to_string(),
                        filter_expression: Some("'Sales'[Amount] > 0".to_string()),
                    },
                    TablePermission {
                        table: "Top Products".to_string(),
                        filter_expression: None,
                    },
                ],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn dax_expressions_enumerates_every_kind_with_exact_owner_home_and_text() {
        let db = every_kind_fixture();
        let found = dax_tuples(&db);

        assert_eq!(found.len(), 12);
        assert_eq!(
            found,
            vec![
                (
                    DaxExpressionKind::Measure,
                    measure_id("Sales", "Total Sales"),
                    Some("Sales"),
                    "SUM('Sales'[Amount])",
                ),
                (
                    DaxExpressionKind::MeasureFormatString,
                    measure_id("Sales", "Total Sales"),
                    Some("Sales"),
                    "\"#,##0\"",
                ),
                (
                    DaxExpressionKind::MeasureDetailRows,
                    measure_id("Sales", "Total Sales"),
                    Some("Sales"),
                    "SELECTCOLUMNS('Sales')",
                ),
                (
                    DaxExpressionKind::KpiTarget,
                    measure_id("Sales", "Total Sales"),
                    Some("Sales"),
                    "[Budget]",
                ),
                (
                    DaxExpressionKind::KpiStatus,
                    measure_id("Sales", "Total Sales"),
                    Some("Sales"),
                    "IF([Total Sales] > 0, 1, -1)",
                ),
                (
                    DaxExpressionKind::KpiTrend,
                    measure_id("Sales", "Total Sales"),
                    Some("Sales"),
                    "[Total Sales] - [Prior]",
                ),
                (
                    DaxExpressionKind::CalculatedColumn,
                    column_id("Sales", "Margin"),
                    Some("Sales"),
                    "'Sales'[Amount] * 0.2",
                ),
                (
                    DaxExpressionKind::TableDetailRows,
                    table_id("Sales"),
                    Some("Sales"),
                    "SELECTCOLUMNS('Sales', \"A\", [Amount])",
                ),
                (
                    DaxExpressionKind::CalculatedTable,
                    partition_id("Top Products", "Top Products"),
                    Some("Top Products"),
                    "TOPN(10, 'Product', [Total Sales])",
                ),
                (
                    DaxExpressionKind::CalculationItem,
                    calc_item_id("Time Intelligence", "YTD"),
                    Some("Time Intelligence"),
                    "TOTALYTD(SELECTEDMEASURE(), 'Date'[Date])",
                ),
                (
                    DaxExpressionKind::CalculationItemFormatString,
                    calc_item_id("Time Intelligence", "YTD"),
                    Some("Time Intelligence"),
                    "\"#,##0;;\"",
                ),
                (
                    DaxExpressionKind::RlsFilter,
                    ObjectId::Role {
                        role: NameKey::new("Reader")
                    },
                    Some("Sales"),
                    "'Sales'[Amount] > 0",
                ),
            ]
        );
    }

    /// `ObjectId` equality is case-insensitive, so the tuple assertion above cannot
    /// catch an owner built from a lowercased or otherwise rewritten name.
    #[test]
    fn dax_expression_owners_preserve_source_casing() {
        let db = every_kind_fixture();
        let displayed: Vec<String> = db
            .dax_expressions()
            .iter()
            .map(|e| e.owner.to_string())
            .collect();

        assert_eq!(
            displayed,
            vec![
                "'Sales'[Total Sales]",
                "'Sales'[Total Sales]",
                "'Sales'[Total Sales]",
                "'Sales'[Total Sales]",
                "'Sales'[Total Sales]",
                "'Sales'[Total Sales]",
                "'Sales'[Margin]",
                "table 'Sales'",
                "partition 'Top Products'[Top Products]",
                "calculation item 'Time Intelligence'[YTD]",
                "calculation item 'Time Intelligence'[YTD]",
                "role 'Reader'",
            ]
        );
    }

    #[test]
    fn dax_expressions_excludes_data_columns_m_partitions_and_metadata_permissions() {
        let db = every_kind_fixture();
        let found = dax_tuples(&db);

        assert!(!found
            .iter()
            .any(|(_, owner, _, _)| *owner == column_id("Sales", "Amount")));
        assert!(!found
            .iter()
            .any(|(_, owner, _, _)| *owner == partition_id("Sales", "Sales-Part1")));
        assert!(!found
            .iter()
            .any(|(_, _, _, text)| text.starts_with("let Source")));
        assert_eq!(
            found
                .iter()
                .filter(|(kind, _, _, _)| *kind == DaxExpressionKind::RlsFilter)
                .count(),
            1
        );
        assert!(!found
            .iter()
            .any(|(kind, _, home, _)| *kind == DaxExpressionKind::RlsFilter
                && *home == Some("Top Products")));
    }

    #[test]
    fn dax_expressions_is_empty_for_a_model_with_no_dax() {
        let db = TabularDatabase {
            tables: vec![Table {
                name: "Sales".to_string(),
                columns: vec![Column {
                    name: "Amount".to_string(),
                    ..Default::default()
                }],
                partitions: vec![partition(
                    "Sales",
                    PartitionSource::M {
                        expression: "let Source = 1 in Source".to_string(),
                    },
                )],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(db.dax_expressions().len(), 0);
    }

    // --- Task 5: m_expressions -------------------------------------------------

    fn m_fixture() -> TabularDatabase {
        TabularDatabase {
            tables: vec![Table {
                name: "Sales".to_string(),
                partitions: vec![
                    partition(
                        "Sales-M",
                        PartitionSource::M {
                            expression: "let Source = Sql.Database(Server) in Source".to_string(),
                        },
                    ),
                    partition(
                        "Sales-Native",
                        PartitionSource::Query {
                            query: "SELECT * FROM dbo.Sales".to_string(),
                        },
                    ),
                    partition(
                        "Sales-Lake",
                        PartitionSource::Other {
                            kind: Some("entity".to_string()),
                        },
                    ),
                ],
                ..Default::default()
            }],
            expressions: vec![
                SharedExpression {
                    name: "Server".to_string(),
                    expression: "\"contoso.database.windows.net\"".to_string(),
                },
                SharedExpression {
                    name: "Database".to_string(),
                    expression: "\"AdventureWorks\"".to_string(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn m_expressions_covers_m_partitions_and_shared_expressions_only() {
        let db = m_fixture();
        let found = m_tuples(&db);

        assert_eq!(found.len(), 3);
        assert_eq!(
            found,
            vec![
                (
                    partition_id("Sales", "Sales-M"),
                    "let Source = Sql.Database(Server) in Source",
                ),
                (
                    ObjectId::Expression {
                        name: NameKey::new("Server")
                    },
                    "\"contoso.database.windows.net\"",
                ),
                (
                    ObjectId::Expression {
                        name: NameKey::new("Database")
                    },
                    "\"AdventureWorks\"",
                ),
            ]
        );
    }

    #[test]
    fn query_and_other_partitions_appear_in_neither_enumeration() {
        let db = m_fixture();

        for (owner, text) in m_tuples(&db) {
            assert_ne!(owner, partition_id("Sales", "Sales-Native"));
            assert_ne!(owner, partition_id("Sales", "Sales-Lake"));
            assert_ne!(text, "SELECT * FROM dbo.Sales");
        }
        assert_eq!(db.dax_expressions().len(), 0);
    }

    #[test]
    fn m_expressions_is_empty_for_a_model_with_no_m() {
        let db = TabularDatabase {
            tables: vec![Table {
                name: "Top Products".to_string(),
                partitions: vec![partition(
                    "Top Products",
                    PartitionSource::Calculated {
                        expression: "TOPN(10, 'Product')".to_string(),
                    },
                )],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(db.m_expressions().len(), 0);
    }

    // --- Task 6: deterministic order -------------------------------------------

    #[test]
    fn dax_expressions_preserves_model_order() {
        let db = every_kind_fixture();
        let kinds: Vec<DaxExpressionKind> = db.dax_expressions().iter().map(|e| e.kind).collect();

        assert_eq!(
            kinds,
            vec![
                DaxExpressionKind::Measure,
                DaxExpressionKind::MeasureFormatString,
                DaxExpressionKind::MeasureDetailRows,
                DaxExpressionKind::KpiTarget,
                DaxExpressionKind::KpiStatus,
                DaxExpressionKind::KpiTrend,
                DaxExpressionKind::CalculatedColumn,
                DaxExpressionKind::TableDetailRows,
                DaxExpressionKind::CalculatedTable,
                DaxExpressionKind::CalculationItem,
                DaxExpressionKind::CalculationItemFormatString,
                DaxExpressionKind::RlsFilter,
            ]
        );
    }

    #[test]
    fn dax_expressions_follows_table_and_measure_declaration_order() {
        let measure = |name: &str, expression: &str| Measure {
            name: name.to_string(),
            expression: expression.to_string(),
            ..Default::default()
        };
        let db = TabularDatabase {
            tables: vec![
                Table {
                    name: "Zebra".to_string(),
                    measures: vec![measure("M2", "2"), measure("M1", "1")],
                    ..Default::default()
                },
                Table {
                    name: "Apple".to_string(),
                    measures: vec![measure("M3", "3")],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let found: Vec<(ObjectId, &str)> = db
            .dax_expressions()
            .into_iter()
            .map(|e| (e.owner, e.text))
            .collect();
        assert_eq!(
            found,
            vec![
                (measure_id("Zebra", "M2"), "2"),
                (measure_id("Zebra", "M1"), "1"),
                (measure_id("Apple", "M3"), "3"),
            ]
        );
    }
}
