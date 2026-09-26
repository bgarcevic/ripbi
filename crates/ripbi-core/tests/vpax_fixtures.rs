//! `.vpax` storage sources (issue #108), written by the real `Dax.Vpax`
//! library — see `fixtures/vpax/generate`.

use std::path::PathBuf;

use ripbi_core::{SizeBasis, StorageStats, TabularDatabase, ingest};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vpax")
        .join(name)
}

fn engine(bytes: u64, rows: u64, cardinality: Option<u64>) -> Option<StorageStats> {
    Some(StorageStats {
        bytes: Some(bytes),
        basis: SizeBasis::Engine,
        rows: Some(rows),
        cardinality,
    })
}

fn column(model: &TabularDatabase, table: &str, column: &str) -> Option<StorageStats> {
    model
        .tables
        .iter()
        .find(|t| t.name == table)
        .and_then(|t| t.columns.iter().find(|c| c.name == column))
        .and_then(|c| c.storage)
}

#[test]
fn both_package_shapes_yield_the_hand_computed_sizes() {
    for name in ["tiny.vpax", "tiny-model-only.vpax"] {
        let model = ingest::storage_source(&fixture(name)).unwrap().value;
        // Column: dictionary + data segments + attribute hierarchies.
        assert_eq!(
            column(&model, "Sales", "Amount"),
            engine(400 + 900 + 50, 1000, Some(900)),
            "{name}"
        );
        assert_eq!(
            column(&model, "Sales", "ProductKey"),
            engine(100 + 200 + 40, 1000, Some(100))
        );
        assert_eq!(
            column(&model, "Product", "Category"),
            engine(30 + 20, 100, Some(5))
        );
        let sales = &model.tables[0];
        assert_eq!(
            sales.columns.len(),
            2,
            "the RowNumber column is not a model object"
        );
        // Table: its columns (RowNumber's 16 included) + its relationship's 24.
        assert_eq!(sales.storage, engine(1350 + 340 + 16 + 24, 1000, None));
        assert_eq!(model.tables[1].storage, engine(130 + 50, 100, None));
        let relationship = &model.relationships[0];
        assert_eq!(
            [
                &relationship.from_table,
                &relationship.from_column,
                &relationship.to_table,
                &relationship.to_column
            ],
            ["Sales", "ProductKey", "Product", "ProductKey"]
        );
        assert_eq!(relationship.storage.unwrap().bytes, Some(24));
        assert_eq!(model.storage_bytes, Some(1730 + 180));
    }
}

#[test]
fn a_vpax_attaches_to_a_model_by_identity() {
    let source = ingest::storage_source(&fixture("tiny.vpax")).unwrap().value;
    let mut model: TabularDatabase = source.clone();
    for table in &mut model.tables {
        table.storage = None;
        for column in &mut table.columns {
            column.storage = None;
            column.name = column.name.to_uppercase();
        }
    }
    model.tables[1].columns[1].name = "Renamed".to_string();
    let coverage = model.attach_storage(&source);
    assert_eq!((coverage.matched, coverage.total), (5, 6));
    assert_eq!(model.tables[1].columns[1].storage, None);
}

#[test]
fn a_zip_without_dax_model_json_is_not_a_storage_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.vpax");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("DaxVpaView.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.finish().unwrap();
    let error = ingest::storage_source(&path).unwrap_err().to_string();
    assert!(
        error.contains("no storage catalog") || error.contains("not"),
        "{error}"
    );
}
