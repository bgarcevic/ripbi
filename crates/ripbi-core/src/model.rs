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
    ColumnHandle, ExpressionHandle, FunctionHandle, HierarchyHandle, MeasureHandle, Resolved,
    TableHandle,
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
    /// User-defined DAX functions (TOM functions). Names are model-global.
    pub functions: Vec<Function>,
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
    /// Calendars (TOM calendars) binding groups of the table's columns.
    pub calendars: Vec<Calendar>,
    /// In TOM a calculation group is a property of a table.
    pub calculation_group: Option<CalculationGroup>,
    /// DAX defaultDetailRowsDefinition (drillthrough detail rows).
    pub detail_rows_expression: Option<String>,
    /// Hidden from report authors; hidden objects are still live if referenced.
    pub is_hidden: bool,
}

impl Table {
    /// A calculated table is a table whose partition source is DAX.
    ///
    /// There is no flag for this in TOM, and none here: the partition decides.
    ///
    /// # Examples
    ///
    /// ```
    /// use ripbi_core::{Partition, PartitionSource, Table};
    ///
    /// let top_products = Table {
    ///     name: "Top Products".to_string(),
    ///     partitions: vec![Partition {
    ///         name: "Top Products".to_string(),
    ///         source: PartitionSource::Calculated {
    ///             expression: "TOPN(10, Products, Products[Sales])".to_string(),
    ///         },
    ///     }],
    ///     ..Default::default()
    /// };
    /// assert!(top_products.is_calculated());
    ///
    /// // An imported table is not, however it was loaded.
    /// let imported = Table {
    ///     name: "Products".to_string(),
    ///     partitions: vec![Partition {
    ///         name: "Products".to_string(),
    ///         source: PartitionSource::M { expression: "Sql.Database(...)".to_string() },
    ///     }],
    ///     ..Default::default()
    /// };
    /// assert!(!imported.is_calculated());
    /// ```
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
    /// Names of other columns in the same table (TOM groupByColumns).
    /// Liveness edge: a used column keeps its group-by columns alive.
    pub group_by_columns: Vec<String>,
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
    /// DAX evaluated when no calculation item is selected (TOM noSelectionExpression).
    pub no_selection_expression: Option<String>,
    /// Dynamic format string (DAX) for the no-selection case.
    pub no_selection_format_string_expression: Option<String>,
    /// DAX evaluated when multiple items are selected or the selection is empty
    /// (TOM multipleOrEmptySelectionExpression).
    pub multiple_or_empty_selection_expression: Option<String>,
    /// Dynamic format string (DAX) for the multiple-or-empty-selection case.
    pub multiple_or_empty_selection_format_string_expression: Option<String>,
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

/// A user-defined DAX function (TOM function). Referenced from DAX by name;
/// its body can be the sole reference keeping another object alive — and the
/// function itself can be dead.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Function {
    /// Function name; model-global, as referenced from DAX.
    pub name: String,
    /// The function's DAX body.
    pub expression: String,
    /// Hidden from report authors; hidden objects are still live if referenced.
    pub is_hidden: bool,
}

/// A calendar (TOM calendar) defined on a table, binding groups of its columns.
///
/// Modeled minimally — the name and the bound column names — which is all a static
/// source file can contribute to liveness: a referenced calendar keeps its bound
/// columns alive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Calendar {
    /// Calendar name.
    pub name: String,
    /// Names of the columns (in the owning table) the calendar binds.
    pub columns: Vec<String>,
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

    /// The user-defined function a handle points at, or `None` if the handle is stale.
    pub fn function(&self, h: FunctionHandle) -> Option<&Function> {
        self.functions.get(h.0)
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
                    table: NameKey::new(table.name.as_str()),
                    column: NameKey::new(column.name.as_str()),
                })
            }
            Resolved::Measure(h) => {
                let table = self.tables.get(h.table)?;
                let measure = table.measures.get(h.measure)?;
                Some(ObjectId::Measure {
                    table: NameKey::new(table.name.as_str()),
                    measure: NameKey::new(measure.name.as_str()),
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
    /// A calculation group's no-selection expression.
    CalculationGroupNoSelection,
    /// A calculation group's no-selection dynamic format string.
    CalculationGroupNoSelectionFormatString,
    /// A calculation group's multiple-or-empty-selection expression.
    CalculationGroupMultipleOrEmptySelection,
    /// A calculation group's multiple-or-empty-selection dynamic format string.
    CalculationGroupMultipleOrEmptySelectionFormatString,
    /// A user-defined function's body.
    Function,
}

/// The model object an enumerated expression belongs to.
///
/// Names are borrowed from the model, so enumerating a model's expressions
/// allocates nothing. Call [`to_object_id`](ExpressionOwner::to_object_id) to
/// materialize a graph node key — once per node the graph actually creates, rather
/// than once per expression.
///
/// The variants are exactly the objects that can own an expression, which is why
/// there is no hierarchy here: hierarchies reference columns but define no DAX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExpressionOwner<'a> {
    /// A table, owning its detail-rows expression.
    Table {
        /// Table name.
        table: &'a str,
    },
    /// A calculated column.
    Column {
        /// Owning table.
        table: &'a str,
        /// Column name.
        column: &'a str,
    },
    /// A measure, owning its expression, format string, detail rows, and KPI.
    Measure {
        /// Home table.
        table: &'a str,
        /// Measure name.
        measure: &'a str,
    },
    /// A partition, owning its M or DAX source query.
    Partition {
        /// Owning table.
        table: &'a str,
        /// Partition name.
        partition: &'a str,
    },
    /// A security role, owning its row-level-security filters.
    Role {
        /// Role name.
        role: &'a str,
    },
    /// A calculation item.
    CalculationItem {
        /// Calculation group table.
        table: &'a str,
        /// Calculation item name.
        item: &'a str,
    },
    /// A model-level shared M expression.
    Expression {
        /// Expression name.
        name: &'a str,
    },
    /// A user-defined DAX function.
    Function {
        /// Function name.
        name: &'a str,
    },
}

impl ExpressionOwner<'_> {
    /// The owner's stable graph-node identity, allocating the owned name keys.
    #[must_use]
    pub fn to_object_id(&self) -> ObjectId {
        match *self {
            ExpressionOwner::Table { table } => ObjectId::Table {
                table: NameKey::new(table),
            },
            ExpressionOwner::Column { table, column } => ObjectId::Column {
                table: NameKey::new(table),
                column: NameKey::new(column),
            },
            ExpressionOwner::Measure { table, measure } => ObjectId::Measure {
                table: NameKey::new(table),
                measure: NameKey::new(measure),
            },
            ExpressionOwner::Partition { table, partition } => ObjectId::Partition {
                table: NameKey::new(table),
                partition: NameKey::new(partition),
            },
            ExpressionOwner::Role { role } => ObjectId::Role {
                role: NameKey::new(role),
            },
            ExpressionOwner::CalculationItem { table, item } => ObjectId::CalculationItem {
                table: NameKey::new(table),
                item: NameKey::new(item),
            },
            ExpressionOwner::Expression { name } => ObjectId::Expression {
                name: NameKey::new(name),
            },
            ExpressionOwner::Function { name } => ObjectId::Function {
                name: NameKey::new(name),
            },
        }
    }
}

/// Borrowed view of one DAX expression owned by a model object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DaxExpressionRef<'a> {
    /// The object the expression belongs to — the source node of any edge derived
    /// from references found in `text`.
    pub owner: ExpressionOwner<'a>,
    /// Which property of `owner` this expression is.
    pub kind: DaxExpressionKind,
    /// Context table for unqualified-column resolution by the lexer.
    pub home_table: Option<&'a str>,
    /// The expression text, borrowed from the model.
    pub text: &'a str,
}

/// Borrowed view of one M expression owned by a model object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MExpressionRef<'a> {
    /// The object the expression belongs to: a partition or a shared expression.
    pub owner: ExpressionOwner<'a>,
    /// The expression text, borrowed from the model.
    pub text: &'a str,
}

impl TabularDatabase {
    /// Every DAX expression in the model, with its owner and home-table context.
    ///
    /// Order follows model order (tables, then each table's measures, columns,
    /// partitions, table-level expressions, calculation items and their group's
    /// selection expressions, then roles, then functions), so the result is
    /// deterministic for a given model and diffable across runs.
    ///
    /// Owners borrow their names, so this allocates only the returned `Vec`.
    #[must_use]
    pub fn dax_expressions(&self) -> Vec<DaxExpressionRef<'_>> {
        let mut out = Vec::new();

        for table in &self.tables {
            let home = Some(table.name.as_str());

            for measure in &table.measures {
                let owner = ExpressionOwner::Measure {
                    table: &table.name,
                    measure: &measure.name,
                };
                let kpi = measure.kpi.as_ref();
                let sources = [
                    (DaxExpressionKind::Measure, Some(&measure.expression)),
                    (
                        DaxExpressionKind::MeasureFormatString,
                        measure.format_string_expression.as_ref(),
                    ),
                    (
                        DaxExpressionKind::MeasureDetailRows,
                        measure.detail_rows_expression.as_ref(),
                    ),
                    (
                        DaxExpressionKind::KpiTarget,
                        kpi.and_then(|kpi| kpi.target_expression.as_ref()),
                    ),
                    (
                        DaxExpressionKind::KpiStatus,
                        kpi.and_then(|kpi| kpi.status_expression.as_ref()),
                    ),
                    (
                        DaxExpressionKind::KpiTrend,
                        kpi.and_then(|kpi| kpi.trend_expression.as_ref()),
                    ),
                ];
                for (kind, text) in sources {
                    if let Some(text) = text {
                        out.push(DaxExpressionRef {
                            owner,
                            kind,
                            home_table: home,
                            text,
                        });
                    }
                }
            }

            for column in &table.columns {
                if let ColumnKind::Calculated { expression } = &column.kind {
                    out.push(DaxExpressionRef {
                        owner: ExpressionOwner::Column {
                            table: &table.name,
                            column: &column.name,
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
                        owner: ExpressionOwner::Partition {
                            table: &table.name,
                            partition: &partition.name,
                        },
                        kind: DaxExpressionKind::CalculatedTable,
                        home_table: home,
                        text: expression,
                    });
                }
            }

            if let Some(text) = &table.detail_rows_expression {
                out.push(DaxExpressionRef {
                    owner: ExpressionOwner::Table { table: &table.name },
                    kind: DaxExpressionKind::TableDetailRows,
                    home_table: home,
                    text,
                });
            }

            if let Some(group) = &table.calculation_group {
                for item in &group.items {
                    let owner = ExpressionOwner::CalculationItem {
                        table: &table.name,
                        item: &item.name,
                    };
                    out.push(DaxExpressionRef {
                        owner,
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

                // Group-level selection expressions. The calc group is a property of
                // its table in TOM, so — like detail rows — the table is the owner and
                // the kind is what discriminates.
                let group_owner = ExpressionOwner::Table { table: &table.name };
                let sources = [
                    (
                        DaxExpressionKind::CalculationGroupNoSelection,
                        group.no_selection_expression.as_ref(),
                    ),
                    (
                        DaxExpressionKind::CalculationGroupNoSelectionFormatString,
                        group.no_selection_format_string_expression.as_ref(),
                    ),
                    (
                        DaxExpressionKind::CalculationGroupMultipleOrEmptySelection,
                        group.multiple_or_empty_selection_expression.as_ref(),
                    ),
                    (
                        DaxExpressionKind::CalculationGroupMultipleOrEmptySelectionFormatString,
                        group
                            .multiple_or_empty_selection_format_string_expression
                            .as_ref(),
                    ),
                ];
                for (kind, text) in sources {
                    if let Some(text) = text {
                        out.push(DaxExpressionRef {
                            owner: group_owner,
                            kind,
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
                        owner: ExpressionOwner::Role { role: &role.name },
                        kind: DaxExpressionKind::RlsFilter,
                        home_table: Some(permission.table.as_str()),
                        text,
                    });
                }
            }
        }

        for function in &self.functions {
            // A function body has no row context of its own: unqualified `[Name]`
            // references inside it can only be measures.
            out.push(DaxExpressionRef {
                owner: ExpressionOwner::Function {
                    name: &function.name,
                },
                kind: DaxExpressionKind::Function,
                home_table: None,
                text: &function.expression,
            });
        }

        out
    }

    /// Every M expression: M partitions plus shared model expressions.
    ///
    /// `Query` and `Other` partition sources are not M and are excluded.
    #[must_use]
    pub fn m_expressions(&self) -> Vec<MExpressionRef<'_>> {
        let mut out = Vec::new();

        for table in &self.tables {
            for partition in &table.partitions {
                if let PartitionSource::M { expression } = &partition.source {
                    out.push(MExpressionRef {
                        owner: ExpressionOwner::Partition {
                            table: &table.name,
                            partition: &partition.name,
                        },
                        text: expression,
                    });
                }
            }
        }

        for expression in &self.expressions {
            out.push(MExpressionRef {
                owner: ExpressionOwner::Expression {
                    name: &expression.name,
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
    use rstest::rstest;

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

    fn expression_id(name: &str) -> ObjectId {
        ObjectId::Expression {
            name: NameKey::new(name),
        }
    }

    fn function_id(name: &str) -> ObjectId {
        ObjectId::Function {
            name: NameKey::new(name),
        }
    }

    fn role_id(role: &str) -> ObjectId {
        ObjectId::Role {
            role: NameKey::new(role),
        }
    }

    fn partition(name: &str, source: PartitionSource) -> Partition {
        Partition {
            name: name.to_string(),
            source,
        }
    }

    /// `(kind, owner, home_table, text)` for every DAX expression, in order.
    fn dax_tuples(db: &TabularDatabase) -> Vec<(DaxExpressionKind, ObjectId, Option<&str>, &str)> {
        db.dax_expressions()
            .into_iter()
            .map(|e| (e.kind, e.owner.to_object_id(), e.home_table, e.text))
            .collect()
    }

    /// `(owner, text)` for every M expression, in order.
    fn m_tuples(db: &TabularDatabase) -> Vec<(ObjectId, &str)> {
        db.m_expressions()
            .into_iter()
            .map(|e| (e.owner.to_object_id(), e.text))
            .collect()
    }

    fn owners(db: &TabularDatabase) -> Vec<ObjectId> {
        dax_tuples(db)
            .into_iter()
            .map(|(_, owner, _, _)| owner)
            .collect()
    }

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
                        no_selection_expression: Some("SELECTEDMEASURE()".to_string()),
                        no_selection_format_string_expression: Some(
                            "SELECTEDMEASUREFORMATSTRING()".to_string(),
                        ),
                        multiple_or_empty_selection_expression: Some(
                            "ERROR(\"Pick one item\")".to_string(),
                        ),
                        multiple_or_empty_selection_format_string_expression: Some(
                            "\"General\"".to_string(),
                        ),
                    }),
                    ..Default::default()
                },
            ],
            functions: vec![Function {
                name: "Sales.NetPrice".to_string(),
                expression: "(price: SCALAR) => price * (1 - [Discount Pct])".to_string(),
                is_hidden: false,
            }],
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

    mod expression_views {
        use super::*;

        /// Both expression views must stay `Copy`, which is only possible while every
        /// field borrows. It is the structural guarantee that enumerating a model's
        /// expressions allocates nothing but the returned `Vec` — adding an owned
        /// field (an `ObjectId`, a `String`) breaks this and reintroduces a
        /// per-expression allocation on the graph layer's hot path.
        #[test]
        fn are_copy_so_enumeration_borrows_everything() {
            fn assert_copy<T: Copy>() {}
            assert_copy::<DaxExpressionRef<'_>>();
            assert_copy::<MExpressionRef<'_>>();
            assert_copy::<ExpressionOwner<'_>>();
        }
    }

    /// A calculated table is one whose partition source is DAX. There is no flag in
    /// TOM and none here, so every other source kind — including ones this crate does
    /// not recognize — must read as not calculated.
    mod is_calculated {
        use super::*;

        fn table_with(source: Option<PartitionSource>) -> Table {
            Table {
                name: "Anything".to_string(),
                partitions: source.into_iter().map(|s| partition("P", s)).collect(),
                ..Default::default()
            }
        }

        #[rstest]
        #[case::dax_partition(
            Some(PartitionSource::Calculated { expression: "TOPN(10, 'Sales')".to_string() }),
            true
        )]
        #[case::m_partition(
            Some(PartitionSource::M { expression: "let Source = Sql.Database() in Source".to_string() }),
            false
        )]
        #[case::native_query(
            Some(PartitionSource::Query { query: "SELECT * FROM dbo.Sales".to_string() }),
            false
        )]
        #[case::direct_lake_entity(
            Some(PartitionSource::Other { kind: Some("entity".to_string()) }),
            false
        )]
        #[case::unknown_future_source(Some(PartitionSource::Other { kind: None }), false)]
        #[case::no_partitions(None, false)]
        fn follows_the_partition_source(
            #[case] source: Option<PartitionSource>,
            #[case] expected: bool,
        ) {
            assert_eq!(table_with(source).is_calculated(), expected);
        }

        #[test]
        fn is_true_when_only_one_of_several_partitions_is_dax() {
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

            assert!(
                mixed.is_calculated(),
                "one DAX partition makes the table calculated"
            );
        }
    }

    mod defaults {
        use super::*;

        /// TMDL omits the flag for active relationships, so a relationship built
        /// field-by-field must come out active. A derived `Default` would silently
        /// make every one of them inactive.
        #[test]
        fn a_relationship_is_active() {
            assert!(Relationship::default().is_active);
        }

        #[test]
        fn a_relationship_has_no_other_content() {
            assert_eq!(
                Relationship::default(),
                Relationship {
                    name: None,
                    from_table: String::new(),
                    from_column: String::new(),
                    to_table: String::new(),
                    to_column: String::new(),
                    is_active: true,
                }
            );
        }

        /// An unparsed source must never be mistaken for a query language, or schema
        /// drift would feed junk to the DAX lexer.
        #[test]
        fn a_partition_source_is_other_with_no_kind() {
            assert_eq!(
                PartitionSource::default(),
                PartitionSource::Other { kind: None }
            );
        }

        #[test]
        fn a_partition_carries_the_default_source() {
            assert_eq!(
                Partition::default().source,
                PartitionSource::Other { kind: None }
            );
        }

        #[test]
        fn a_column_kind_is_data() {
            assert_eq!(ColumnKind::default(), ColumnKind::Data);
        }

        #[test]
        fn a_column_carries_the_default_kind() {
            assert_eq!(Column::default().kind, ColumnKind::Data);
        }
    }

    mod dax_expressions {
        use super::*;

        #[test]
        fn enumerates_every_kind_with_exact_owner_home_and_text() {
            let db = every_kind_fixture();

            assert_eq!(
                dax_tuples(&db),
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
                        DaxExpressionKind::CalculationGroupNoSelection,
                        table_id("Time Intelligence"),
                        Some("Time Intelligence"),
                        "SELECTEDMEASURE()",
                    ),
                    (
                        DaxExpressionKind::CalculationGroupNoSelectionFormatString,
                        table_id("Time Intelligence"),
                        Some("Time Intelligence"),
                        "SELECTEDMEASUREFORMATSTRING()",
                    ),
                    (
                        DaxExpressionKind::CalculationGroupMultipleOrEmptySelection,
                        table_id("Time Intelligence"),
                        Some("Time Intelligence"),
                        "ERROR(\"Pick one item\")",
                    ),
                    (
                        DaxExpressionKind::CalculationGroupMultipleOrEmptySelectionFormatString,
                        table_id("Time Intelligence"),
                        Some("Time Intelligence"),
                        "\"General\"",
                    ),
                    (
                        DaxExpressionKind::RlsFilter,
                        role_id("Reader"),
                        Some("Sales"),
                        "'Sales'[Amount] > 0",
                    ),
                    (
                        DaxExpressionKind::Function,
                        function_id("Sales.NetPrice"),
                        None,
                        "(price: SCALAR) => price * (1 - [Discount Pct])",
                    ),
                ]
            );
        }

        #[test]
        fn enumerates_one_expression_per_populated_site() {
            assert_eq!(dax_tuples(&every_kind_fixture()).len(), 17);
        }

        /// `ObjectId` equality is case-insensitive, so the tuple assertion above
        /// cannot catch an owner built from a lowercased or rewritten name.
        #[test]
        fn owners_preserve_source_casing() {
            let db = every_kind_fixture();
            let displayed: Vec<String> = owners(&db).iter().map(ObjectId::to_string).collect();

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
                    "table 'Time Intelligence'",
                    "table 'Time Intelligence'",
                    "table 'Time Intelligence'",
                    "table 'Time Intelligence'",
                    "role 'Reader'",
                    "function 'Sales.NetPrice'",
                ]
            );
        }

        #[rstest]
        #[case::a_data_column(column_id("Sales", "Amount"))]
        #[case::an_m_partition(partition_id("Sales", "Sales-Part1"))]
        fn excludes(#[case] unwanted: ObjectId) {
            let db = every_kind_fixture();

            assert!(
                !owners(&db).contains(&unwanted),
                "{unwanted} owns no DAX and must not be enumerated"
            );
        }

        #[test]
        fn excludes_m_partition_text() {
            let db = every_kind_fixture();

            assert!(
                !dax_tuples(&db)
                    .iter()
                    .any(|(_, _, _, text)| text.starts_with("let Source")),
                "an M query must never be handed to the DAX lexer"
            );
        }

        /// The fixture's role filters one table and holds metadata-only permission on
        /// another; only the filtered one is an expression.
        #[test]
        fn emits_one_filter_for_a_role_with_one_filtered_permission() {
            let db = every_kind_fixture();

            assert_eq!(
                dax_tuples(&db)
                    .iter()
                    .filter(|(kind, _, _, _)| *kind == DaxExpressionKind::RlsFilter)
                    .count(),
                1
            );
        }

        #[test]
        fn excludes_metadata_only_permissions() {
            let db = every_kind_fixture();

            assert!(
                !dax_tuples(&db).iter().any(|(kind, _, home, _)| {
                    *kind == DaxExpressionKind::RlsFilter && *home == Some("Top Products")
                }),
                "a permission with no filter expression contributes nothing"
            );
        }

        #[test]
        fn is_empty_for_a_model_with_no_dax() {
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

        /// Order is model order, so a run is deterministic and diffable.
        #[test]
        fn preserves_model_order_within_a_table() {
            let db = every_kind_fixture();
            let kinds: Vec<DaxExpressionKind> =
                db.dax_expressions().iter().map(|e| e.kind).collect();

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
                    DaxExpressionKind::CalculationGroupNoSelection,
                    DaxExpressionKind::CalculationGroupNoSelectionFormatString,
                    DaxExpressionKind::CalculationGroupMultipleOrEmptySelection,
                    DaxExpressionKind::CalculationGroupMultipleOrEmptySelectionFormatString,
                    DaxExpressionKind::RlsFilter,
                    DaxExpressionKind::Function,
                ]
            );
        }

        #[test]
        fn follows_table_and_measure_declaration_order() {
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
                .map(|e| (e.owner.to_object_id(), e.text))
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

    mod m_expressions {
        use super::*;

        #[test]
        fn covers_m_partitions_and_shared_expressions_with_exact_owner_and_text() {
            let db = m_fixture();

            assert_eq!(
                m_tuples(&db),
                vec![
                    (
                        partition_id("Sales", "Sales-M"),
                        "let Source = Sql.Database(Server) in Source",
                    ),
                    (expression_id("Server"), "\"contoso.database.windows.net\""),
                    (expression_id("Database"), "\"AdventureWorks\""),
                ]
            );
        }

        #[rstest]
        #[case::a_native_query_partition(partition_id("Sales", "Sales-Native"))]
        #[case::an_unrecognized_source_partition(partition_id("Sales", "Sales-Lake"))]
        fn excludes(#[case] unwanted: ObjectId) {
            let db = m_fixture();

            assert!(
                !m_tuples(&db)
                    .into_iter()
                    .any(|(owner, _)| owner == unwanted),
                "{unwanted} holds no M and must not be enumerated"
            );
        }

        /// A native query is neither M nor DAX, so it reaches no lexer at all.
        #[test]
        fn leaves_native_query_partitions_out_of_the_dax_enumeration_too() {
            assert_eq!(m_fixture().dax_expressions().len(), 0);
        }

        #[test]
        fn is_empty_for_a_model_with_no_m() {
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
    }
}
