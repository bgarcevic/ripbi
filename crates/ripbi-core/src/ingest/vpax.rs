//! VertiPaq Analyzer `.vpax` exports as a storage source (issue #108).
//!
//! A `.vpax` is a ZIP written by the `Dax.Vpax` library (DAX Studio, Tabular
//! Editor, semantic-link-labs). `DaxModel.json` is the one part every writer
//! includes: the engine's per-object statistics, serialized by Newtonsoft with
//! `$id`/`$ref` object references. `DaxVpaView.json` and `Model.bim` are
//! optional and not read.
//!
//! Only names and statistics come out: tables, columns, and relationships
//! (by endpoint columns), each with [`StorageStats`] measured on the
//! [`SizeBasis::Engine`] basis. Sizes are the engine's in-memory figures at
//! export time, so they differ from an `.abf`'s file sizes for the same model.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use serde_json::Value;
use zip::ZipArchive;

use super::Ingested;
use super::archive::read_member;
use crate::model::{Column, Relationship, SizeBasis, StorageStats, Table, TabularDatabase};
use crate::{Error, Result};

/// The part every `.vpax` writer includes.
pub(super) const MODEL_PART: &str = "DaxModel.json";

/// Reads a `.vpax`'s statistics into a names-and-storage-only model.
pub(super) fn load(path: &Path) -> Result<Ingested<TabularDatabase>> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let Some(bytes) = read_member(&mut zip, MODEL_PART)? else {
        return Err(Error::UnsupportedFormat(format!(
            "{} has no {MODEL_PART}; it is not a VertiPaq Analyzer export",
            path.display()
        )));
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Error::UnsupportedFormat(format!("{MODEL_PART} is not UTF-8: {error}")))?;
    let document: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))?;
    Ok(Ingested {
        value: parse(&document),
        skips: Vec::new(),
    })
}

/// Builds the model from a parsed `DaxModel.json`. Missing or unexpected
/// fields degrade to absent statistics, never an error.
fn parse(document: &Value) -> TabularDatabase {
    // Newtonsoft writes each column once, with `$id`, and refers to it from
    // relationships by `$ref`.
    let mut columns_by_id: HashMap<&str, (&str, &str)> = HashMap::new();
    let mut tables = Vec::new();
    let mut table_bytes: HashMap<&str, u64> = HashMap::new();
    for table in array(document, "Tables") {
        let Some(table_name) = text(table, "TableName") else {
            continue;
        };
        let rows = number(table, "RowsCount");
        let mut bytes = 0u64;
        let mut columns = Vec::new();
        for column in array(table, "Columns") {
            let Some(column_name) = text(column, "ColumnName") else {
                continue;
            };
            if let Some(id) = text(column, "$id") {
                columns_by_id.insert(id, (table_name, column_name));
            }
            let size = column_size(column);
            bytes = bytes.saturating_add(size);
            // The engine's RowNumber column is no model object; its bytes
            // still count toward the table.
            if column.get("IsRowNumber").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            columns.push(Column {
                name: column_name.to_string(),
                storage: Some(StorageStats {
                    bytes: Some(size),
                    basis: SizeBasis::Engine,
                    rows,
                    cardinality: number(column, "ColumnCardinality"),
                }),
                ..Default::default()
            });
        }
        for hierarchy in array(table, "UserHierarchies") {
            bytes = bytes.saturating_add(number(hierarchy, "UsedSize").unwrap_or(0));
        }
        table_bytes.insert(table_name, bytes);
        tables.push((table_name, rows, columns));
    }

    let mut relationships = Vec::new();
    for relationship in array(document, "Relationships") {
        let endpoint = |key: &str| {
            let reference = relationship.get(key)?;
            let id = text(reference, "$ref").or_else(|| text(reference, "$id"))?;
            columns_by_id.get(id).copied()
        };
        let (Some((from_table, from_column)), Some((to_table, to_column))) =
            (endpoint("FromColumn"), endpoint("ToColumn"))
        else {
            continue;
        };
        let size = match (
            number(relationship, "UsedSizeFrom"),
            number(relationship, "UsedSizeTo"),
        ) {
            (None, None) => number(relationship, "UsedSize").unwrap_or(0),
            (from, to) => from.unwrap_or(0).saturating_add(to.unwrap_or(0)),
        };
        // VertiPaq Analyzer counts a relationship toward its many side.
        if let Some(bytes) = table_bytes.get_mut(from_table) {
            *bytes = bytes.saturating_add(size);
        }
        relationships.push(Relationship {
            from_table: from_table.to_string(),
            from_column: from_column.to_string(),
            to_table: to_table.to_string(),
            to_column: to_column.to_string(),
            is_active: relationship
                .get("IsActive")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            storage: Some(StorageStats {
                bytes: Some(size),
                basis: SizeBasis::Engine,
                rows: None,
                cardinality: None,
            }),
            ..Default::default()
        });
    }

    let tables: Vec<Table> = tables
        .into_iter()
        .map(|(name, rows, columns)| Table {
            name: name.to_string(),
            storage: Some(StorageStats {
                bytes: table_bytes.get(name).copied(),
                basis: SizeBasis::Engine,
                rows,
                cardinality: None,
            }),
            columns,
            ..Default::default()
        })
        .collect();
    let storage_bytes = tables
        .iter()
        .filter_map(|table| table.storage.and_then(|stats| stats.bytes))
        .fold(0u64, u64::saturating_add);
    TabularDatabase {
        name: text(document, "ModelName").map(str::to_string),
        tables,
        relationships,
        storage_bytes: Some(storage_bytes),
        ..Default::default()
    }
}

/// A column's `TotalSize`, else the sum of its dictionary, data, and
/// hierarchies — each from its own field, else from its segments.
fn column_size(column: &Value) -> u64 {
    if let Some(total) = number(column, "TotalSize") {
        return total;
    }
    let used = |key: &str| {
        array(column, key)
            .iter()
            .filter_map(|part| number(part, "UsedSize"))
            .fold(0u64, u64::saturating_add)
    };
    let data = number(column, "DataSize").unwrap_or_else(|| used("ColumnSegments"));
    let hierarchies =
        number(column, "HierarchiesSize").unwrap_or_else(|| used("ColumnHierarchies"));
    number(column, "DictionarySize")
        .unwrap_or(0)
        .saturating_add(data)
        .saturating_add(hierarchies)
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// A non-negative count or size; the library writes integers, but a float is
/// rounded rather than dropped.
fn number(value: &Value, key: &str) -> Option<u64> {
    let field = value.get(key)?;
    field.as_u64().or_else(|| {
        field
            .as_f64()
            .filter(|number| number.is_finite() && *number >= 0.0)
            .map(|number| number.round() as u64)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn stats(bytes: u64, rows: Option<u64>, cardinality: Option<u64>) -> Option<StorageStats> {
        Some(StorageStats {
            bytes: Some(bytes),
            basis: SizeBasis::Engine,
            rows,
            cardinality,
        })
    }

    #[test]
    fn columns_tables_and_relationships_carry_engine_sizes() {
        let model = parse(&json!({
            "ModelName": "Tiny",
            "Tables": [
                {"$id": "1", "TableName": "Sales", "RowsCount": 1000,
                 "UserHierarchies": [{"UsedSize": 6}],
                 "Columns": [
                    {"$id": "2", "ColumnName": "Amount", "ColumnCardinality": 900, "TotalSize": 1350},
                    {"$id": "3", "ColumnName": "Key", "ColumnCardinality": 100,
                     "DictionarySize": 100,
                     "ColumnSegments": [{"UsedSize": 150}, {"UsedSize": 50}],
                     "ColumnHierarchies": [{"UsedSize": 40}]},
                    {"$id": "4", "ColumnName": "RowNumber-2662979B", "IsRowNumber": true, "TotalSize": 16}
                 ]},
                {"$id": "5", "TableName": "Product", "RowsCount": 100,
                 "Columns": [{"$id": "6", "ColumnName": "Key", "TotalSize": 130}]}
            ],
            "Relationships": [
                {"FromColumn": {"$ref": "3"}, "ToColumn": {"$ref": "6"}, "UsedSizeFrom": 24},
                {"FromColumn": {"$ref": "404"}, "ToColumn": {"$ref": "6"}, "UsedSizeFrom": 9}
            ]
        }));

        assert_eq!(model.name.as_deref(), Some("Tiny"));
        let sales = &model.tables[0];
        let names: Vec<_> = sales.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Amount", "Key"], "RowNumber is no model object");
        assert_eq!(sales.columns[0].storage, stats(1350, Some(1000), Some(900)));
        assert_eq!(sales.columns[1].storage, stats(340, Some(1000), Some(100)));
        // Columns (incl. RowNumber) + user hierarchy + the relationship it is the many side of.
        assert_eq!(
            sales.storage,
            stats(1350 + 340 + 16 + 6 + 24, Some(1000), None)
        );
        assert_eq!(model.tables[1].storage, stats(130, Some(100), None));
        assert_eq!(
            model.relationships.len(),
            1,
            "an unresolved endpoint is dropped"
        );
        assert_eq!(
            (
                model.relationships[0].from_column.as_str(),
                model.relationships[0].to_table.as_str()
            ),
            ("Key", "Product")
        );
        assert_eq!(model.relationships[0].storage, stats(24, None, None));
        assert_eq!(model.storage_bytes, Some(1736 + 130));
    }

    #[test]
    fn unexpected_shapes_degrade_to_absent_statistics() {
        let model = parse(&json!({
            "Tables": [
                {"TableName": "T", "Columns": [{"ColumnName": "C", "TotalSize": -3}, {"Nope": 1}]},
                {"Columns": []}
            ],
            "Relationships": "not an array"
        }));
        assert_eq!(model.tables.len(), 1);
        assert_eq!(model.tables[0].columns[0].storage, stats(0, None, None));
        assert_eq!(model.storage_bytes, Some(0));
    }
}
