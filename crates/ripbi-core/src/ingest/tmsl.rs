//! TMSL (`model.bim` and archive `DataModelSchema`) to the shared model AST.

use std::path::Path;

use serde_json::Value;

use crate::ingest::{SkipKind, SkipNotice};
use crate::model::{
    CalculationGroup, CalculationItem, Calendar, Column, ColumnKind, ColumnPermission, Function,
    Hierarchy, HierarchyLevel, HierarchyRef, Kpi, Measure, MetadataPermission,
    ParameterValuesColumn, Partition, PartitionSource, RefreshPolicy, Relationship, Role,
    SharedExpression, Table, TablePermission, TabularDatabase, Variation,
};
use crate::{Error, Result};

pub(super) fn load(
    bytes: &[u8],
    path: &Path,
    skips: &mut Vec<SkipNotice>,
) -> Result<TabularDatabase> {
    let root: Value = serde_json::from_str(&super::decode_json_text(bytes, "TMSL JSON")?)?;
    let database = root
        .get("createOrReplace")
        .and_then(|command| command.get("database"))
        .unwrap_or(&root);
    let model = database.get("model").ok_or_else(|| {
        Error::UnsupportedFormat(format!("{} has no TMSL model object", path.display()))
    })?;
    let mut result = TabularDatabase {
        name: string(database, "name"),
        ..Default::default()
    };
    for (index, item) in array(model, "tables").iter().enumerate() {
        if let Some(table) = table(item, path, skips, index) {
            result.tables.push(table);
        }
    }
    for item in array(model, "relationships") {
        result.relationships.push(Relationship {
            name: string(item, "name"),
            from_table: string(item, "fromTable").unwrap_or_default(),
            from_column: string(item, "fromColumn").unwrap_or_default(),
            to_table: string(item, "toTable").unwrap_or_default(),
            to_column: string(item, "toColumn").unwrap_or_default(),
            is_active: item
                .get("isActive")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            storage: None,
        });
    }
    for item in array(model, "roles") {
        let Some(name) = string(item, "name") else {
            continue;
        };
        let mut role = Role {
            name,
            ..Default::default()
        };
        for permission in array(item, "tablePermissions") {
            let Some(table) = string(permission, "name") else {
                continue;
            };
            role.table_permissions.push(TablePermission {
                table: table.clone(),
                filter_expression: expression(permission, "filterExpression"),
            });
            for column in array(permission, "columnPermissions") {
                if let Some(name) = string(column, "name") {
                    role.column_permissions.push(ColumnPermission {
                        table: table.clone(),
                        column: name,
                        metadata_permission: metadata_permission(column),
                    });
                }
            }
        }
        result.roles.push(role);
    }
    for item in array(model, "expressions") {
        if let Some(name) = string(item, "name") {
            let parameter_values_column = item
                .get("parameterValuesColumn")
                .and_then(parse_qualified_column);
            result.expressions.push(SharedExpression {
                name,
                expression: expression(item, "expression").unwrap_or_default(),
                parameter_values_column,
            });
        }
    }
    for item in array(model, "functions") {
        if let Some(name) = string(item, "name") {
            result.functions.push(Function {
                name,
                expression: expression(item, "expression").unwrap_or_default(),
                is_hidden: bool_value(item, "isHidden"),
            });
        }
    }
    Ok(result)
}

fn table(item: &Value, path: &Path, skips: &mut Vec<SkipNotice>, index: usize) -> Option<Table> {
    let Some(name) = string(item, "name") else {
        skips.push(SkipNotice {
            path: path.to_path_buf(),
            location: Some(format!("/model/tables/{index}/name")),
            kind: SkipKind::MalformedValue,
            detail: "table has no name".to_string(),
        });
        return None;
    };
    let mut table = Table {
        is_hidden: bool_value(item, "isHidden"),
        is_private: bool_value(item, "isPrivate"),
        is_local_date_table: name.starts_with("LocalDateTable_")
            || has_annotation(item, "__PBI_LocalDateTable"),
        is_template_date_table: name.starts_with("DateTableTemplate_")
            || has_annotation(item, "__PBI_TemplateDateTable"),
        detail_rows_expression: item
            .get("defaultDetailRowsDefinition")
            .and_then(|v| expression(v, "expression")),
        name,
        ..Default::default()
    };
    for column in array(item, "columns") {
        if let Some(name) = string(column, "name") {
            let kind = match column.get("type").and_then(Value::as_str).unwrap_or("data") {
                "calculated" => ColumnKind::Calculated {
                    expression: expression(column, "expression").unwrap_or_default(),
                },
                "calculatedTableColumn" => ColumnKind::CalculatedTableColumn,
                _ => ColumnKind::Data,
            };
            table.columns.push(Column {
                name,
                kind,
                is_hidden: bool_value(column, "isHidden"),
                storage: None,
                sort_by_column: string(column, "sortByColumn"),
                group_by_columns: column
                    .get("relatedColumnDetails")
                    .map(|details| array(details, "groupByColumns"))
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|v| string(v, "groupingColumn"))
                    .collect(),
                variations: array(column, "variations")
                    .iter()
                    .map(|v| Variation {
                        name: string(v, "name").unwrap_or_default(),
                        is_default: bool_value(v, "isDefault"),
                        relationship: string(v, "relationship"),
                        default_hierarchy: v.get("defaultHierarchy").and_then(parse_hierarchy_ref),
                    })
                    .collect(),
            });
        }
    }
    for measure in array(item, "measures") {
        if let Some(name) = string(measure, "name") {
            table.measures.push(Measure {
                name,
                expression: expression(measure, "expression").unwrap_or_default(),
                is_hidden: bool_value(measure, "isHidden"),
                format_string_expression: measure
                    .get("formatStringDefinition")
                    .and_then(|v| expression(v, "expression")),
                detail_rows_expression: measure
                    .get("detailRowsDefinition")
                    .and_then(|v| expression(v, "expression")),
                kpi: measure.get("kpi").map(|v| Kpi {
                    target_expression: expression(v, "targetExpression"),
                    status_expression: expression(v, "statusExpression"),
                    trend_expression: expression(v, "trendExpression"),
                }),
            });
        }
    }
    for partition in array(item, "partitions") {
        let Some(name) = string(partition, "name") else {
            continue;
        };
        let source = partition.get("source").unwrap_or(&Value::Null);
        let kind = source.get("type").and_then(Value::as_str).unwrap_or("");
        let text = expression(source, "expression").unwrap_or_default();
        let source = match kind {
            "m" => PartitionSource::M { expression: text },
            "calculated" => PartitionSource::Calculated { expression: text },
            "query" => PartitionSource::Query {
                query: string(source, "query").unwrap_or_default(),
            },
            _ => PartitionSource::Other {
                kind: Some(kind.to_string()),
            },
        };
        table.partitions.push(Partition { name, source });
    }
    for hierarchy in array(item, "hierarchies") {
        if let Some(name) = string(hierarchy, "name") {
            table.hierarchies.push(Hierarchy {
                name,
                is_hidden: bool_value(hierarchy, "isHidden"),
                levels: array(hierarchy, "levels")
                    .iter()
                    .map(|level| HierarchyLevel {
                        name: string(level, "name").unwrap_or_default(),
                        column: string(level, "column").unwrap_or_default(),
                    })
                    .collect(),
            });
        }
    }
    if let Some(group) = item.get("calculationGroup") {
        table.calculation_group = Some(CalculationGroup {
            items: array(group, "calculationItems")
                .iter()
                .map(|v| CalculationItem {
                    name: string(v, "name").unwrap_or_default(),
                    expression: expression(v, "expression").unwrap_or_default(),
                    format_string_expression: v
                        .get("formatStringDefinition")
                        .and_then(|v| expression(v, "expression")),
                })
                .collect(),
            no_selection_expression: group
                .get("noSelectionExpression")
                .and_then(|v| expression(v, "expression")),
            no_selection_format_string_expression: group
                .get("noSelectionExpression")
                .and_then(|v| v.get("formatStringDefinition"))
                .and_then(|v| expression(v, "expression")),
            multiple_or_empty_selection_expression: group
                .get("multipleOrEmptySelectionExpression")
                .and_then(|v| expression(v, "expression")),
            multiple_or_empty_selection_format_string_expression: group
                .get("multipleOrEmptySelectionExpression")
                .and_then(|v| v.get("formatStringDefinition"))
                .and_then(|v| expression(v, "expression")),
        });
    }
    if let Some(policy) = item.get("refreshPolicy") {
        table.refresh_policy = Some(RefreshPolicy {
            policy_type: string(policy, "policyType"),
            source_expression: expression(policy, "sourceExpression"),
            change_detection: expression(policy, "pollingExpression"),
        });
    }
    for calendar in array(item, "calendars") {
        if let Some(name) = string(calendar, "name") {
            let mut columns = Vec::new();
            for group in array(calendar, "calendarColumnGroups") {
                for key in ["columns", "primaryColumn", "associatedColumn"] {
                    if let Some(value) = group.get(key) {
                        if let Some(name) = value.as_str() {
                            columns.push(name.to_string());
                        }
                        if let Some(values) = value.as_array() {
                            columns.extend(
                                values.iter().filter_map(Value::as_str).map(str::to_string),
                            );
                        }
                    }
                }
            }
            table.calendars.push(Calendar { name, columns });
        }
    }
    Some(table)
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn bool_value(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn expression(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(text) => Some(text.clone()),
        Value::Array(lines) => Some(
            lines
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

fn has_annotation(value: &Value, name: &str) -> bool {
    array(value, "annotations")
        .iter()
        .any(|v| v.get("name").and_then(Value::as_str) == Some(name))
}

fn metadata_permission(value: &Value) -> Option<MetadataPermission> {
    match value.get("metadataPermission")?.as_str()? {
        "none" => Some(MetadataPermission::Denied),
        "read" => Some(MetadataPermission::Granted),
        _ => None,
    }
}

fn parse_qualified_column(value: &Value) -> Option<ParameterValuesColumn> {
    if value.is_object() {
        return Some(ParameterValuesColumn {
            table: string(value, "table")?,
            column: string(value, "column")?,
        });
    }
    let text = value.as_str()?;
    let (table, column) = text.rsplit_once('.')?;
    Some(ParameterValuesColumn {
        table: table.trim_matches('\'').to_string(),
        column: column.trim_matches('\'').to_string(),
    })
}

fn parse_hierarchy_ref(value: &Value) -> Option<HierarchyRef> {
    if let Some(text) = value.as_str() {
        let (table, hierarchy) = text.rsplit_once('.')?;
        return Some(HierarchyRef {
            table: table.trim_matches('\'').to_string(),
            hierarchy: hierarchy.trim_matches('\'').to_string(),
        });
    }
    Some(HierarchyRef {
        table: string(value, "table")?,
        hierarchy: string(value, "hierarchy").or_else(|| string(value, "name"))?,
    })
}
