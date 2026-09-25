//! `metadata.sqlitedb` (the engine's TMSCHEMA catalog) to the shared model AST.
//!
//! Each TOM object type is one SQLite table keyed by `ID`, and parents are
//! joined by `…ID` columns. Columns are read by name, so a column that an
//! older or newer schema lacks reads as absent instead of failing, and an
//! optional table that does not exist yet (`Function`, `Calendar`, …) reads
//! as empty. Engine-internal tables (`SystemFlags` bit 0: attribute-hierarchy
//! and relationship storage such as `H$…`/`R$…`) and `RowNumber` columns are
//! not part of the authored model and are dropped silently, as are the
//! storage, culture, perspective and translation tables. Values the mapping
//! does not know, and foreign keys that point nowhere, become skip notices.
//!
//! Enum codes follow `Microsoft.AnalysisServices.Tabular`: `ColumnType`,
//! `PartitionSourceType`, `MetadataPermission`, `CalculationGroupSelectionMode`,
//! `RefreshPolicyType`, and `ObjectType` (3 = table) for annotations.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};

use super::container::corrupt;
use crate::Result;
use crate::ingest::{SkipKind, SkipNotice};
use crate::model::{
    CalculationGroup, CalculationItem, Calendar, Column, ColumnKind, ColumnPermission, Function,
    Hierarchy, HierarchyLevel, HierarchyRef, Kpi, Measure, MetadataPermission,
    ParameterValuesColumn, Partition, PartitionSource, RefreshPolicy, Relationship, Role,
    SharedExpression, Table, TablePermission, TabularDatabase, Variation,
};

/// `ObjectType` of a table in `Annotation` rows.
const OBJECT_TYPE_TABLE: i64 = 3;

/// Maps the bytes of a `metadata.sqlitedb` into a [`TabularDatabase`].
pub(super) fn load(
    bytes: &[u8],
    path: &Path,
    skips: &mut Vec<SkipNotice>,
) -> Result<TabularDatabase> {
    let mut connection = Connection::open_with_flags(
        ":memory:",
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite)?;
    connection
        .deserialize_read_exact("main", bytes, bytes.len(), true)
        .map_err(sqlite)?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(sqlite)?;
    Catalog::read(&connection)?.build(path, skips)
}

fn sqlite(error: rusqlite::Error) -> crate::Error {
    corrupt(format!("metadata.sqlitedb cannot be read: {error}"))
}

/// One SQLite row, with columns addressed by name.
struct Row {
    columns: Rc<HashMap<String, usize>>,
    values: Vec<Value>,
}

impl Row {
    fn value(&self, name: &str) -> Option<&Value> {
        self.columns
            .get(name)
            .and_then(|&index| self.values.get(index))
    }

    fn int(&self, name: &str) -> Option<i64> {
        match self.value(name)? {
            Value::Integer(value) => Some(*value),
            Value::Real(value) => Some(*value as i64),
            Value::Text(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    fn id(&self) -> i64 {
        self.int("ID").unwrap_or(-1)
    }

    fn text(&self, name: &str) -> Option<String> {
        match self.value(name)? {
            Value::Text(text) => Some(text.clone()),
            Value::Integer(value) => Some(value.to_string()),
            _ => None,
        }
    }

    /// A non-empty text value.
    fn some_text(&self, name: &str) -> Option<String> {
        self.text(name).filter(|text| !text.trim().is_empty())
    }

    fn flag(&self, name: &str) -> bool {
        self.int(name).is_some_and(|value| value != 0)
    }

    /// `ExplicitName`, else `InferredName` (columns), else `Name`.
    fn name(&self) -> Option<String> {
        self.some_text("ExplicitName")
            .or_else(|| self.some_text("InferredName"))
            .or_else(|| self.some_text("Name"))
    }
}

/// Every catalog table the mapping reads, loaded once.
struct Catalog {
    tables: Vec<Row>,
    columns: Vec<Row>,
    measures: Vec<Row>,
    partitions: Vec<Row>,
    hierarchies: Vec<Row>,
    levels: Vec<Row>,
    relationships: Vec<Row>,
    roles: Vec<Row>,
    table_permissions: Vec<Row>,
    column_permissions: Vec<Row>,
    calculation_groups: Vec<Row>,
    calculation_items: Vec<Row>,
    calculation_expressions: Vec<Row>,
    format_strings: Vec<Row>,
    detail_rows: Vec<Row>,
    kpis: Vec<Row>,
    refresh_policies: Vec<Row>,
    expressions: Vec<Row>,
    functions: Vec<Row>,
    annotations: Vec<Row>,
    variations: Vec<Row>,
    related_column_details: Vec<Row>,
    group_by_columns: Vec<Row>,
    calendars: Vec<Row>,
    calendar_groups: Vec<Row>,
    calendar_columns: Vec<Row>,
}

impl Catalog {
    fn read(connection: &Connection) -> Result<Self> {
        let existing: HashSet<String> = connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<_>>()
            })
            .map_err(sqlite)?;
        for required in ["Model", "Table", "Column"] {
            if !existing.contains(required) {
                return Err(corrupt(format!(
                    "metadata.sqlitedb has no {required} table"
                )));
            }
        }
        let rows = |name: &str| -> Result<Vec<Row>> {
            if !existing.contains(name) {
                return Ok(Vec::new());
            }
            let sql = format!("SELECT * FROM \"{name}\"");
            let mut statement = connection.prepare(&sql).map_err(sqlite)?;
            let columns: Rc<HashMap<String, usize>> = Rc::new(
                statement
                    .column_names()
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| (name.to_string(), index))
                    .collect(),
            );
            let count = columns.len();
            let mut result: Vec<Row> = statement
                .query_map([], |row| {
                    (0..count)
                        .map(|index| row.get::<_, Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map_err(sqlite)?
                .map(|values| {
                    values.map(|values| Row {
                        columns: Rc::clone(&columns),
                        values,
                    })
                })
                .collect::<rusqlite::Result<_>>()
                .map_err(sqlite)?;
            result.sort_by_key(Row::id);
            Ok(result)
        };
        Ok(Self {
            tables: rows("Table")?,
            columns: rows("Column")?,
            measures: rows("Measure")?,
            partitions: rows("Partition")?,
            hierarchies: rows("Hierarchy")?,
            levels: rows("Level")?,
            relationships: rows("Relationship")?,
            roles: rows("Role")?,
            table_permissions: rows("TablePermission")?,
            column_permissions: rows("ColumnPermission")?,
            calculation_groups: rows("CalculationGroup")?,
            calculation_items: rows("CalculationItem")?,
            calculation_expressions: rows("CalculationExpression")?,
            format_strings: rows("FormatStringDefinition")?,
            detail_rows: rows("DetailRowsDefinition")?,
            kpis: rows("KPI")?,
            refresh_policies: rows("RefreshPolicy")?,
            expressions: rows("Expression")?,
            functions: rows("Function")?,
            annotations: rows("Annotation")?,
            variations: rows("Variation")?,
            related_column_details: rows("RelatedColumnDetails")?,
            group_by_columns: rows("GroupByColumn")?,
            calendars: rows("Calendar")?,
            calendar_groups: rows("CalendarColumnGroup")?,
            calendar_columns: rows("CalendarColumnReference")?,
        })
    }

    fn build(&self, path: &Path, skips: &mut Vec<SkipNotice>) -> Result<TabularDatabase> {
        let mut notes = Notes { path, skips };
        let mut result = TabularDatabase::default();

        // Tables, keyed by ID into `result.tables`.
        let mut table_index: HashMap<i64, usize> = HashMap::new();
        for row in &self.tables {
            if row.int("SystemFlags").unwrap_or(0) & 1 != 0 {
                continue;
            }
            let Some(name) = row.some_text("Name") else {
                notes.push(row, "Table", SkipKind::MalformedValue, "table has no name");
                continue;
            };
            let annotated = |annotation: &str| {
                self.annotations.iter().any(|a| {
                    a.int("ObjectType") == Some(OBJECT_TYPE_TABLE)
                        && a.int("ObjectID") == Some(row.id())
                        && a.text("Name").as_deref() == Some(annotation)
                })
            };
            let table = Table {
                is_hidden: row.flag("IsHidden"),
                is_private: row.flag("IsPrivate"),
                is_local_date_table: name.starts_with("LocalDateTable_")
                    || annotated("__PBI_LocalDateTable"),
                is_template_date_table: name.starts_with("DateTableTemplate_")
                    || annotated("__PBI_TemplateDateTable"),
                detail_rows_expression: row
                    .int("DefaultDetailRowsDefinitionID")
                    .and_then(|id| expression_by_id(&self.detail_rows, id)),
                name,
                ..Default::default()
            };
            table_index.insert(row.id(), result.tables.len());
            result.tables.push(table);
        }
        let names: Vec<String> = result
            .tables
            .iter()
            .map(|table| table.name.clone())
            .collect();
        let table_name = |id: Option<i64>| {
            id.and_then(|id| table_index.get(&id))
                .map(|&index| names[index].clone())
        };

        // Columns: kept as (table index, name) for every lookup by ID.
        let mut column_ref: HashMap<i64, (usize, String)> = HashMap::new();
        let mut columns: Vec<(usize, i64, Column)> = Vec::new();
        for row in &self.columns {
            let Some(&table) = row.int("TableID").and_then(|id| table_index.get(&id)) else {
                continue; // a column of an engine-internal table
            };
            let kind = match row.int("Type") {
                Some(3) => continue, // RowNumber: engine-internal
                Some(1) | None => ColumnKind::Data,
                Some(2) => ColumnKind::Calculated {
                    expression: row.text("Expression").unwrap_or_default(),
                },
                Some(4) => ColumnKind::CalculatedTableColumn,
                Some(other) => {
                    notes.push(
                        row,
                        "Column",
                        SkipKind::MalformedValue,
                        &format!("column type {other} is unknown; read as a data column"),
                    );
                    ColumnKind::Data
                }
            };
            let Some(name) = row.name() else {
                notes.push(
                    row,
                    "Column",
                    SkipKind::MalformedValue,
                    "column has no name",
                );
                continue;
            };
            column_ref.insert(row.id(), (table, name.clone()));
            columns.push((
                table,
                row.id(),
                Column {
                    name,
                    kind,
                    is_hidden: row.flag("IsHidden"),
                    ..Default::default()
                },
            ));
        }
        let column_name = |id: Option<i64>| {
            id.and_then(|id| column_ref.get(&id))
                .map(|(_, name)| name.clone())
        };

        // Hierarchies, before columns so variations can name them.
        let mut hierarchy_ref: HashMap<i64, HierarchyRef> = HashMap::new();
        for row in &self.hierarchies {
            let Some(&table) = row.int("TableID").and_then(|id| table_index.get(&id)) else {
                continue;
            };
            let Some(name) = row.some_text("Name") else {
                notes.push(
                    row,
                    "Hierarchy",
                    SkipKind::MalformedValue,
                    "hierarchy has no name",
                );
                continue;
            };
            let mut levels: Vec<&Row> = self
                .levels
                .iter()
                .filter(|level| level.int("HierarchyID") == Some(row.id()))
                .collect();
            levels.sort_by_key(|level| (level.int("Ordinal").unwrap_or(0), level.id()));
            let levels = levels
                .into_iter()
                .filter_map(|level| {
                    let column = column_name(level.int("ColumnID"));
                    if column.is_none() {
                        notes.push(
                            level,
                            "Level",
                            SkipKind::MalformedValue,
                            "hierarchy level names no known column",
                        );
                    }
                    Some(HierarchyLevel {
                        name: level.some_text("Name").unwrap_or_default(),
                        column: column?,
                    })
                })
                .collect();
            hierarchy_ref.insert(
                row.id(),
                HierarchyRef {
                    table: names[table].clone(),
                    hierarchy: name.clone(),
                },
            );
            result.tables[table].hierarchies.push(Hierarchy {
                name,
                levels,
                is_hidden: row.flag("IsHidden"),
            });
        }

        // Relationships, before columns so variations can name them.
        let mut relationship_name: HashMap<i64, String> = HashMap::new();
        for row in &self.relationships {
            let from = row.int("FromColumnID").and_then(|id| column_ref.get(&id));
            let to = row.int("ToColumnID").and_then(|id| column_ref.get(&id));
            let (Some((from_table, from_column)), Some((to_table, to_column))) = (from, to) else {
                notes.push(
                    row,
                    "Relationship",
                    SkipKind::MalformedValue,
                    "relationship endpoint names no known column",
                );
                continue;
            };
            let name = row.some_text("Name");
            if let Some(name) = &name {
                relationship_name.insert(row.id(), name.clone());
            }
            result.relationships.push(Relationship {
                name,
                from_table: names[*from_table].clone(),
                from_column: from_column.clone(),
                to_table: names[*to_table].clone(),
                to_column: to_column.clone(),
                is_active: row.int("IsActive").is_none_or(|value| value != 0),
            });
        }

        // Column details that reference other objects.
        let columns_by_id: HashMap<i64, &Row> =
            self.columns.iter().map(|row| (row.id(), row)).collect();
        for (table, id, mut column) in columns {
            let row = columns_by_id[&id];
            column.sort_by_column = column_name(row.int("SortByColumnID").filter(|id| *id != 0));
            let details: HashSet<i64> = self
                .related_column_details
                .iter()
                .filter(|details| details.int("ColumnID") == Some(id))
                .map(Row::id)
                .chain(row.int("RelatedColumnDetailsID"))
                .collect();
            column.group_by_columns = self
                .group_by_columns
                .iter()
                .filter(|group| {
                    group
                        .int("RelatedColumnDetailsID")
                        .is_some_and(|id| details.contains(&id))
                })
                .filter_map(|group| column_name(group.int("GroupingColumnID")))
                .collect();
            column.variations = self
                .variations
                .iter()
                .filter(|variation| variation.int("ColumnID") == Some(id))
                .map(|variation| Variation {
                    name: variation.some_text("Name").unwrap_or_default(),
                    is_default: variation.flag("IsDefault"),
                    relationship: variation
                        .int("RelationshipID")
                        .and_then(|id| relationship_name.get(&id).cloned()),
                    default_hierarchy: variation
                        .int("DefaultHierarchyID")
                        .and_then(|id| hierarchy_ref.get(&id).cloned()),
                })
                .collect();
            result.tables[table].columns.push(column);
        }

        // Measures.
        for row in &self.measures {
            let Some(&table) = row.int("TableID").and_then(|id| table_index.get(&id)) else {
                notes.push(
                    row,
                    "Measure",
                    SkipKind::MalformedValue,
                    "measure names no known table",
                );
                continue;
            };
            let Some(name) = row.some_text("Name") else {
                notes.push(
                    row,
                    "Measure",
                    SkipKind::MalformedValue,
                    "measure has no name",
                );
                continue;
            };
            let kpi = row
                .int("KPIID")
                .and_then(|id| self.kpis.iter().find(|kpi| kpi.id() == id))
                .or_else(|| {
                    self.kpis
                        .iter()
                        .find(|kpi| kpi.int("MeasureID") == Some(row.id()))
                })
                .map(|kpi| Kpi {
                    target_expression: kpi.some_text("TargetExpression"),
                    status_expression: kpi.some_text("StatusExpression"),
                    trend_expression: kpi.some_text("TrendExpression"),
                });
            result.tables[table].measures.push(Measure {
                name,
                expression: row.text("Expression").unwrap_or_default(),
                is_hidden: row.flag("IsHidden"),
                format_string_expression: row
                    .int("FormatStringDefinitionID")
                    .and_then(|id| expression_by_id(&self.format_strings, id)),
                detail_rows_expression: row
                    .int("DetailRowsDefinitionID")
                    .and_then(|id| expression_by_id(&self.detail_rows, id)),
                kpi,
            });
        }

        // Partitions.
        for row in &self.partitions {
            let Some(&table) = row.int("TableID").and_then(|id| table_index.get(&id)) else {
                continue;
            };
            let Some(name) = row.some_text("Name") else {
                notes.push(
                    row,
                    "Partition",
                    SkipKind::MalformedValue,
                    "partition has no name",
                );
                continue;
            };
            let text = row.text("QueryDefinition").unwrap_or_default();
            let other = |kind: &str| PartitionSource::Other {
                kind: Some(kind.to_string()),
            };
            let source = match row.int("Type") {
                Some(1) => PartitionSource::Query { query: text },
                Some(2) => PartitionSource::Calculated { expression: text },
                Some(3) => other("none"),
                Some(4) => PartitionSource::M { expression: text },
                Some(5) => other("entity"),
                Some(6) => other("policyRange"),
                Some(7) => other("calculationGroup"),
                Some(8) => other("inferred"),
                code => {
                    notes.push(
                        row,
                        "Partition",
                        SkipKind::MalformedValue,
                        &format!(
                            "partition source type {} is unknown",
                            code.map_or_else(|| "(missing)".to_string(), |code| code.to_string())
                        ),
                    );
                    PartitionSource::Other {
                        kind: code.map(|code| code.to_string()),
                    }
                }
            };
            result.tables[table]
                .partitions
                .push(Partition { name, source });
        }

        // Calculation groups.
        for row in &self.calculation_groups {
            let table = row
                .int("TableID")
                .or_else(|| {
                    self.tables
                        .iter()
                        .find(|table| table.int("CalculationGroupID") == Some(row.id()))
                        .map(Row::id)
                })
                .and_then(|id| table_index.get(&id));
            let Some(&table) = table else {
                notes.push(
                    row,
                    "CalculationGroup",
                    SkipKind::MalformedValue,
                    "calculation group names no known table",
                );
                continue;
            };
            let mut items: Vec<&Row> = self
                .calculation_items
                .iter()
                .filter(|item| item.int("CalculationGroupID") == Some(row.id()))
                .collect();
            items.sort_by_key(|item| (item.int("Ordinal").unwrap_or(0), item.id()));
            let format_string = |row: &Row| {
                row.int("FormatStringDefinitionID")
                    .and_then(|id| expression_by_id(&self.format_strings, id))
            };
            let mut group = CalculationGroup {
                items: items
                    .into_iter()
                    .map(|item| CalculationItem {
                        name: item.some_text("Name").unwrap_or_default(),
                        expression: item.text("Expression").unwrap_or_default(),
                        format_string_expression: format_string(item),
                    })
                    .collect(),
                ..Default::default()
            };
            for expression in self
                .calculation_expressions
                .iter()
                .filter(|expression| expression.int("CalculationGroupID") == Some(row.id()))
            {
                let text = expression.some_text("Expression");
                let format = format_string(expression);
                match expression.int("SelectionMode") {
                    Some(1) => {
                        group.multiple_or_empty_selection_expression = text;
                        group.multiple_or_empty_selection_format_string_expression = format;
                    }
                    Some(2) => {
                        group.no_selection_expression = text;
                        group.no_selection_format_string_expression = format;
                    }
                    code => notes.push(
                        expression,
                        "CalculationExpression",
                        SkipKind::MalformedValue,
                        &format!("calculation expression selection mode {code:?} is unknown"),
                    ),
                }
            }
            result.tables[table].calculation_group = Some(group);
        }

        // Refresh policies.
        for row in &self.refresh_policies {
            let table = row
                .int("TableID")
                .or_else(|| {
                    self.tables
                        .iter()
                        .find(|table| table.int("RefreshPolicyID") == Some(row.id()))
                        .map(Row::id)
                })
                .and_then(|id| table_index.get(&id));
            let Some(&table) = table else {
                continue;
            };
            let policy_type = match row.int("PolicyType") {
                Some(0) | None => Some("basic".to_string()),
                Some(code) => {
                    notes.push(
                        row,
                        "RefreshPolicy",
                        SkipKind::MalformedValue,
                        &format!("refresh policy type {code} is unknown"),
                    );
                    Some(code.to_string())
                }
            };
            result.tables[table].refresh_policy = Some(RefreshPolicy {
                policy_type,
                source_expression: row.some_text("SourceExpression"),
                change_detection: row.some_text("PollingExpression"),
            });
        }

        // Calendars.
        for row in &self.calendars {
            let Some(&table) = row.int("TableID").and_then(|id| table_index.get(&id)) else {
                continue;
            };
            let groups: HashSet<i64> = self
                .calendar_groups
                .iter()
                .filter(|group| group.int("CalendarID") == Some(row.id()))
                .map(Row::id)
                .collect();
            let columns = self
                .calendar_columns
                .iter()
                .filter(|column| {
                    column
                        .int("CalendarColumnGroupID")
                        .is_some_and(|id| groups.contains(&id))
                })
                .filter_map(|column| column_name(column.int("ColumnID")))
                .collect();
            result.tables[table].calendars.push(Calendar {
                name: row.some_text("Name").unwrap_or_default(),
                columns,
            });
        }

        // Roles.
        for row in &self.roles {
            let Some(name) = row.some_text("Name") else {
                continue;
            };
            let mut role = Role {
                name,
                ..Default::default()
            };
            for permission in self
                .table_permissions
                .iter()
                .filter(|permission| permission.int("RoleID") == Some(row.id()))
            {
                let Some(table) = table_name(permission.int("TableID")) else {
                    notes.push(
                        permission,
                        "TablePermission",
                        SkipKind::MalformedValue,
                        "table permission names no known table",
                    );
                    continue;
                };
                role.table_permissions.push(TablePermission {
                    table: table.clone(),
                    filter_expression: permission.some_text("FilterExpression"),
                });
                for column in self
                    .column_permissions
                    .iter()
                    .filter(|column| column.int("TablePermissionID") == Some(permission.id()))
                {
                    let Some(name) = column_name(column.int("ColumnID")) else {
                        continue;
                    };
                    let metadata_permission = match column.int("MetadataPermission") {
                        Some(1) => Some(MetadataPermission::Denied),
                        Some(2) => Some(MetadataPermission::Granted),
                        Some(0) | None => None,
                        Some(code) => {
                            notes.push(
                                column,
                                "ColumnPermission",
                                SkipKind::MalformedValue,
                                &format!("metadata permission {code} is unknown"),
                            );
                            None
                        }
                    };
                    role.column_permissions.push(ColumnPermission {
                        table: table.clone(),
                        column: name,
                        metadata_permission,
                    });
                }
            }
            result.roles.push(role);
        }

        // Shared expressions and functions.
        for row in &self.expressions {
            let Some(name) = row.some_text("Name") else {
                continue;
            };
            let parameter_values_column = row
                .int("ParameterValuesColumnID")
                .and_then(|id| column_ref.get(&id))
                .map(|(table, column)| ParameterValuesColumn {
                    table: names[*table].clone(),
                    column: column.clone(),
                });
            result.expressions.push(SharedExpression {
                name,
                expression: row.text("Expression").unwrap_or_default(),
                parameter_values_column,
            });
        }
        for row in &self.functions {
            if let Some(name) = row.some_text("Name") {
                result.functions.push(Function {
                    name,
                    expression: row.text("Expression").unwrap_or_default(),
                    is_hidden: row.flag("IsHidden"),
                });
            }
        }
        Ok(result)
    }
}

fn expression_by_id(rows: &[Row], id: i64) -> Option<String> {
    rows.iter()
        .find(|row| row.id() == id)
        .and_then(|row| row.some_text("Expression"))
}

/// Skip-notice sink naming rows as `Table#ID`.
struct Notes<'a> {
    path: &'a Path,
    skips: &'a mut Vec<SkipNotice>,
}

impl Notes<'_> {
    fn push(&mut self, row: &Row, table: &str, kind: SkipKind, detail: &str) {
        self.skips.push(SkipNotice {
            path: self.path.to_path_buf(),
            location: Some(format!("{table}#{}", row.id())),
            kind,
            detail: detail.to_string(),
        });
    }
}
