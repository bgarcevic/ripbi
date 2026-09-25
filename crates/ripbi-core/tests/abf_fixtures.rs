//! Synthetic ABF backups, standalone and inside PBIX archives: every framing
//! decodes to the same model, and malformed input fails with a typed error
//! or a skip notice, never a panic.

mod common;

use std::path::{Path, PathBuf};

use common::{
    Backup, archive, data_mashup, decoded_backup, metadata_db, multithreaded, single_threaded,
};
use ripbi_core::ingest::{SkipKind, semantic_model};
use ripbi_core::model::{
    Column, ColumnKind, MetadataPermission, PartitionSource, SizeBasis, StorageStats,
    TabularDatabase,
};
use ripbi_core::{Error, Ingested};

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn ingest(name: &str, bytes: &[u8]) -> ripbi_core::Result<Ingested<TabularDatabase>> {
    let dir = tempfile::tempdir().unwrap();
    semantic_model(&write(dir.path(), name, bytes))
}

fn assert_data_model_error(result: ripbi_core::Result<Ingested<TabularDatabase>>, needle: &str) {
    match result {
        Err(Error::DataModel(detail)) => {
            assert!(detail.contains(needle), "expected '{needle}' in '{detail}'");
        }
        other => panic!("expected a DataModel error containing '{needle}', got {other:?}"),
    }
}

fn stream() -> Vec<u8> {
    decoded_backup(&metadata_db(""), &Backup::default())
}

fn assert_fixture_model(model: &TabularDatabase) {
    let names: Vec<_> = model.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Sales", "Product", "Time Intelligence"]);

    let sales = &model.tables[0];
    let columns: Vec<_> = sales.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        columns,
        ["Amount", "ProductKey", "Double"],
        "RowNumber is dropped"
    );
    assert!(sales.columns[1].is_hidden);
    assert_eq!(
        sales.columns[2].kind,
        ColumnKind::Calculated {
            expression: "Sales[Amount] * 2".to_string()
        }
    );
    assert_eq!(sales.columns[2].sort_by_column.as_deref(), Some("Amount"));
    assert_eq!(sales.measures[0].name, "Total");
    assert_eq!(
        sales.measures[0].format_string_expression.as_deref(),
        Some("\"#,0\"")
    );
    assert!(matches!(&sales.partitions[0].source,
        PartitionSource::M { expression } if expression == "let Source = Raw in Source"));

    let product = &model.tables[1];
    let levels: Vec<_> = product.hierarchies[0]
        .levels
        .iter()
        .map(|l| l.column.as_str())
        .collect();
    assert_eq!(levels, ["Category", "Key"], "levels follow Ordinal");

    let group = model.tables[2].calculation_group.as_ref().unwrap();
    assert_eq!(group.items[0].name, "YTD");
    assert_eq!(
        group.items[0].format_string_expression.as_deref(),
        Some("\"0.0%\"")
    );
    assert_eq!(
        group.no_selection_expression.as_deref(),
        Some("SELECTEDMEASURE()")
    );

    let relationship = &model.relationships[0];
    assert_eq!(
        (
            relationship.from_table.as_str(),
            relationship.from_column.as_str(),
            relationship.to_table.as_str(),
            relationship.to_column.as_str()
        ),
        ("Sales", "ProductKey", "Product", "Key")
    );
    assert_eq!(model.roles[0].table_permissions[0].table, "Product");
    assert_eq!(
        model.roles[0].column_permissions[0].metadata_permission,
        Some(MetadataPermission::Denied)
    );
    assert_eq!(model.expressions[0].name, "Raw");
    assert_eq!(model.functions[0].name, "Double");
}

#[test]
fn every_framing_decodes_to_the_same_model() {
    let stream = stream();
    let framings = [
        ("stream-storage.abf", stream.clone()),
        ("single.abf", single_threaded(&stream, 4096)),
        ("multi.abf", multithreaded(&stream, 4096, 3)),
    ];
    let mut models = Vec::new();
    for (name, bytes) in framings {
        let ingested = ingest(name, &bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(ingested.skips.is_empty(), "{name}: {:#?}", ingested.skips);
        assert_fixture_model(&ingested.value);
        models.push(ingested.value);
    }
    assert!(models.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn backup_is_detected_by_contents_whatever_its_extension() {
    let bytes = single_threaded(&stream(), 8192);
    assert!(ingest("model.bak", &bytes).is_ok());
}

#[test]
fn pbix_data_model_is_decoded_when_no_schema_is_present() {
    let abf = single_threaded(&stream(), 4096);
    let pbix = archive(&[("DataModel", &abf[..]), ("Report/Layout", &b"{}"[..])]);
    let ingested = ingest("model.pbix", &pbix).unwrap();
    assert_fixture_model(&ingested.value);
}

#[test]
fn schema_json_wins_over_data_model() {
    let schema = br#"{"model": {"tables": [{"name": "FromJson",
        "partitions": [{"name": "p", "source": {"type": "calculated", "expression": "{1}"}}]}]}}"#;
    let pbix = archive(&[
        ("DataModelSchema", &schema[..]),
        ("DataModel", &b"not an abf"[..]),
    ]);
    let ingested = ingest("both.pbix", &pbix).unwrap();
    assert_eq!(ingested.value.tables[0].name, "FromJson");
}

#[test]
fn empty_partition_m_is_recovered_from_data_mashup() {
    let db = metadata_db(r#"UPDATE "Partition" SET QueryDefinition = '' WHERE ID = 17;"#);
    let abf = single_threaded(&decoded_backup(&db, &Backup::default()), 4096);
    let mashup = data_mashup("section Section1; shared Sales = let Source = Raw in Source;");
    let pbix = archive(&[("DataModel", &abf[..]), ("DataMashup", &mashup[..])]);
    let model = ingest("mashup.pbix", &pbix).unwrap().value;
    assert!(matches!(&model.tables[0].partitions[0].source,
        PartitionSource::M { expression } if expression == "let Source = Raw in Source"));

    // Without a DataMashup, a bare backup with blank M fails the coverage check.
    let bare = single_threaded(&decoded_backup(&db, &Backup::default()), 4096);
    let error = ingest("blank.abf", &bare).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incomplete M expression coverage")
    );
}

#[test]
fn thin_report_pbix_names_the_missing_model() {
    let pbix = archive(&[("Report/Layout", &b"{}"[..])]);
    match ingest("thin.pbix", &pbix) {
        Err(Error::UnsupportedFormat(detail)) => {
            assert!(detail.contains("no embedded semantic model"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn truncated_chunk_is_an_error() {
    let mut bytes = single_threaded(&stream(), 4096);
    bytes.truncate(bytes.len() - 10);
    assert_data_model_error(ingest("cut.abf", &bytes), "truncated");
}

#[test]
fn truncated_chunk_header_is_an_error() {
    let mut bytes = single_threaded(&stream(), 4096);
    bytes.extend_from_slice(&[1, 2, 3]);
    assert_data_model_error(ingest("cut-header.abf", &bytes), "truncated");
}

#[test]
fn compressed_size_past_the_end_is_an_error() {
    let mut bytes = single_threaded(&stream(), 4096);
    bytes[106..110].copy_from_slice(&0x00ff_ffffu32.to_le_bytes());
    assert_data_model_error(ingest("long.abf", &bytes), "truncated");
}

#[test]
fn oversized_chunk_is_rejected_before_allocation() {
    let mut bytes = single_threaded(&stream(), 4096);
    bytes[102..106].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_data_model_error(ingest("huge.abf", &bytes), "out of range");
}

#[test]
fn corrupt_compressed_payload_is_an_error() {
    let mut bytes = single_threaded(&stream(), 4096);
    for byte in &mut bytes[120..200] {
        *byte ^= 0x5a;
    }
    assert!(matches!(
        ingest("garbled.abf", &bytes),
        Err(Error::DataModel(_))
    ));
}

#[test]
fn multithreaded_group_count_beyond_the_input_is_an_error() {
    let mut bytes = multithreaded(&stream(), 4096, 2);
    // `main_thread_count`: more groups than the input holds.
    bytes[126..134].copy_from_slice(&1_000_000u64.to_le_bytes());
    assert_data_model_error(ingest("groups.abf", &bytes), "declared count");
}

#[test]
fn directory_offset_outside_the_stream_is_an_error() {
    let options = Backup {
        directory_offset_shift: 1 << 40,
        ..Backup::default()
    };
    let bytes = decoded_backup(&metadata_db(""), &options);
    assert_data_model_error(ingest("offset.abf", &bytes), "outside");
}

#[test]
fn backup_without_metadata_db_is_an_error() {
    let options = Backup {
        metadata_name: Some("other.db".to_string()),
        ..Backup::default()
    };
    let bytes = decoded_backup(&metadata_db(""), &options);
    assert_data_model_error(ingest("nometa.abf", &bytes), "metadata.sqlitedb");
}

#[test]
fn corrupt_sqlite_is_an_error() {
    let mut db = metadata_db("");
    for byte in &mut db[100..] {
        *byte = 0xa5;
    }
    let bytes = decoded_backup(&db, &Backup::default());
    assert_data_model_error(ingest("corrupt.abf", &bytes), "metadata.sqlitedb");
}

#[test]
fn catalog_without_table_table_is_an_error() {
    let db = metadata_db(r#"DROP TABLE "Table";"#);
    let bytes = decoded_backup(&db, &Backup::default());
    assert_data_model_error(ingest("notable.abf", &bytes), "no Table table");
}

#[test]
fn unknown_values_and_dangling_keys_become_skip_notices() {
    let db = metadata_db(
        r#"UPDATE "Partition" SET Type = 99 WHERE ID = 23;
           INSERT INTO "Relationship" VALUES (51, 1, 'dangling', 1, 10, 999, 20, 21);"#,
    );
    let bytes = decoded_backup(&db, &Backup::default());
    let ingested = ingest("drift.abf", &bytes).unwrap();
    let skips: Vec<_> = ingested
        .skips
        .iter()
        .map(|skip| (skip.kind, skip.location.as_deref().unwrap_or_default()))
        .collect();
    assert_eq!(
        skips,
        [
            (SkipKind::MalformedValue, "Relationship#51"),
            (SkipKind::MalformedValue, "Partition#23"),
        ]
    );
    assert_eq!(ingested.value.relationships.len(), 1);
    assert!(matches!(&ingested.value.tables[1].partitions[0].source,
        PartitionSource::Other { kind: Some(kind) } if kind == "99"));
}

#[test]
fn random_bytes_after_a_valid_signature_never_panic() {
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    for (index, header) in [
        single_threaded(b"x", 1)[..102].to_vec(),
        multithreaded(b"x", 1, 1)[..102].to_vec(),
    ]
    .into_iter()
    .enumerate()
    {
        for len in [0usize, 7, 8, 40, 200, 5000] {
            let mut bytes = header.clone();
            bytes.extend((0..len).map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            }));
            let result = ingest(&format!("fuzz-{index}-{len}.abf"), &bytes);
            assert!(result.is_err(), "fuzz {index}/{len} unexpectedly parsed");
        }
    }
}

/// Issue #122: the storage catalog of the fixture model, with only the
/// columns the mapping reads. `Amount` (11) owns a dictionary, a segment and
/// its metadata, an `.hidx`, and the `H$` table (30) serving its attribute
/// hierarchy; `rel-1` (50) owns the `R$` table (35). `RowNumber` (14) and a
/// file no storage row names land on the table alone.
const STORAGE: &str = r#"
INSERT INTO "Table" VALUES
    (35, 1, 'R$Sales (10)$rel-1 (50)', 1, 0, 1, NULL, NULL, NULL);
INSERT INTO "Column" VALUES (36, 35, 'INDEX', NULL, 1, NULL, 1, NULL, NULL);
CREATE TABLE "StorageFolder" (ID INTEGER, OwnerID INTEGER, OwnerType INTEGER, Path TEXT);
INSERT INTO "StorageFolder" VALUES
    (100, 0, 18, 'Sales (10).tbl'), (101, 0, 20, 'Sales (10).tbl\17.prt'),
    (102, 0, 18, 'Product (20).tbl'), (103, 0, 20, 'Product (20).tbl\23.prt'),
    (104, 0, 18, 'H$Sales (10)$Amount (11)$(30).tbl'),
    (105, 0, 20, 'H$Sales (10)$Amount (11)$(30).tbl\32.prt'),
    (106, 0, 18, 'R$Sales (10)$rel-1 (50)$(35).tbl');
CREATE TABLE "TableStorage" (ID INTEGER, TableID INTEGER, StorageFolderID INTEGER);
INSERT INTO "TableStorage" VALUES (110, 10, 100), (111, 20, 102), (112, 30, 104), (113, 35, 106);
CREATE TABLE "PartitionStorage" (ID INTEGER, PartitionID INTEGER, SegmentMapStorageID INTEGER,
    StorageFolderID INTEGER);
INSERT INTO "PartitionStorage" VALUES (120, 17, 130, 101), (121, 23, 131, 103), (122, 32, 132, 105);
CREATE TABLE "SegmentMapStorage" (ID INTEGER, PartitionStorageID INTEGER, RecordCount INTEGER);
INSERT INTO "SegmentMapStorage" VALUES (130, 120, 1000), (131, 121, 10), (132, 122, 5);
CREATE TABLE "ColumnStorage" (ID INTEGER, ColumnID INTEGER, DictionaryStorageID INTEGER,
    Statistics_DistinctStates INTEGER, Statistics_RowCount INTEGER);
INSERT INTO "ColumnStorage" VALUES
    (140, 11, 150, 900, 1000), (141, 12, 151, 10, 1000), (142, 13, NULL, 2, 1000),
    (143, 14, NULL, 1000, 1000), (144, 21, NULL, 10, 10), (145, 22, NULL, 3, 10),
    (146, 31, NULL, 5, 5), (147, 36, NULL, 1, 1);
CREATE TABLE "DictionaryStorage" (ID INTEGER, ColumnStorageID INTEGER, StorageFileID INTEGER,
    Size INTEGER);
INSERT INTO "DictionaryStorage" VALUES (150, 140, 200, 4500), (151, 141, 201, NULL);
CREATE TABLE "ColumnPartitionStorage" (ID INTEGER, ColumnStorageID INTEGER,
    PartitionStorageID INTEGER, StorageFileID INTEGER);
INSERT INTO "ColumnPartitionStorage" VALUES
    (160, 140, 120, 210), (161, 141, 120, 211), (162, 142, 120, 212), (163, 143, 120, 213),
    (164, 144, 121, 214), (165, 145, 121, 215), (166, 146, 122, 216), (167, 147, NULL, 217);
CREATE TABLE "SegmentStorage" (ID INTEGER, ColumnPartitionStorageID INTEGER,
    StorageFileID INTEGER);
INSERT INTO "SegmentStorage" VALUES (170, 160, 220);
CREATE TABLE "AttributeHierarchy" (ID INTEGER, ColumnID INTEGER);
INSERT INTO "AttributeHierarchy" VALUES (180, 11);
CREATE TABLE "AttributeHierarchyStorage" (ID INTEGER, AttributeHierarchyID INTEGER,
    StorageFileID INTEGER, SystemTableID INTEGER);
INSERT INTO "AttributeHierarchyStorage" VALUES (181, 180, 230, 30);
CREATE TABLE "RelationshipStorage" (ID INTEGER, RelationshipID INTEGER);
INSERT INTO "RelationshipStorage" VALUES (190, 50);
CREATE TABLE "RelationshipIndexStorage" (ID INTEGER, RelationshipStorageID INTEGER,
    StorageFileID INTEGER, SystemTableID INTEGER, SecondarySystemTableID INTEGER);
INSERT INTO "RelationshipIndexStorage" VALUES (191, 190, 0, 35, 0);
CREATE TABLE "StorageFile" (ID INTEGER, OwnerID INTEGER, OwnerType INTEGER,
    StorageFolderID INTEGER, FileName TEXT);
INSERT INTO "StorageFile" VALUES
    (200, 150, 22, 100, '0.Sales (10).Amount (11).dictionary'),
    (201, 151, 22, 100, '0.Sales (10).ProductKey (12).dictionary'),
    (210, 160, 23, 101, '0.Sales (10).Amount (11).0.idf'),
    (211, 161, 23, 101, '0.Sales (10).ProductKey (12).0.idf'),
    (212, 162, 23, 101, '0.Sales (10).Double (13).0.idf'),
    (213, 163, 23, 101, '0.Sales (10).RowNumber (14).0.idf'),
    (214, 164, 23, 103, '0.Product (20).Key (21).0.idf'),
    (215, 165, 23, 103, '0.Product (20).Category (22).0.idf'),
    (216, 166, 23, 105, '0.H$Sales (10)$Amount (11).POS_TO_ID.0.idf'),
    (217, 167, 23, 106, '0.R$Sales (10)$rel-1 (50).INDEX.0.idf'),
    (220, 170, 24, 101, '0.Sales (10).Amount (11).0.idfmeta'),
    (230, 181, 27, 100, '1.H$Sales (10)$Amount (11).hidx'),
    (240, 0, 0, 100, 'table.bin');
"#;

/// The logged data files, each with its size.
const STORAGE_FILES: &[(&str, usize)] = &[
    (r"Sales (10).tbl\0.Sales (10).Amount (11).dictionary", 5000),
    (
        r"Sales (10).tbl\0.Sales (10).ProductKey (12).dictionary",
        50,
    ),
    (
        r"Sales (10).tbl\17.prt\0.Sales (10).Amount (11).0.idf",
        1000,
    ),
    (
        r"Sales (10).tbl\17.prt\0.Sales (10).ProductKey (12).0.idf",
        200,
    ),
    (r"Sales (10).tbl\17.prt\0.Sales (10).Double (13).0.idf", 300),
    (
        r"Sales (10).tbl\17.prt\0.Sales (10).RowNumber (14).0.idf",
        40,
    ),
    (r"Product (20).tbl\23.prt\0.Product (20).Key (21).0.idf", 20),
    (
        r"Product (20).tbl\23.prt\0.Product (20).Category (22).0.idf",
        30,
    ),
    (
        r"H$Sales (10)$Amount (11)$(30).tbl\32.prt\0.H$Sales (10)$Amount (11).POS_TO_ID.0.idf",
        400,
    ),
    (
        r"R$Sales (10)$rel-1 (50)$(35).tbl\0.R$Sales (10)$rel-1 (50).INDEX.0.idf",
        70,
    ),
    // Paths match case-insensitively.
    (
        r"SALES (10).TBL\17.prt\0.Sales (10).Amount (11).0.idfmeta",
        100,
    ),
    (r"Sales (10).tbl\1.H$Sales (10)$Amount (11).hidx", 8),
    (r"Sales (10).tbl\table.bin", 3),
];

/// The storage fixture as a backup whose log lists every file but `omit`.
fn storage_backup(omit: Option<&str>) -> Vec<u8> {
    let options = Backup {
        files: STORAGE_FILES
            .iter()
            .filter(|(path, _)| Some(*path) != omit)
            .map(|(path, size)| (path.to_string(), *size))
            .collect(),
        ..Backup::default()
    };
    single_threaded(&decoded_backup(&metadata_db(STORAGE), &options), 4096)
}

fn column<'a>(model: &'a TabularDatabase, table: &str, column: &str) -> &'a Column {
    model
        .tables
        .iter()
        .find(|t| t.name == table)
        .and_then(|t| t.columns.iter().find(|c| c.name == column))
        .unwrap()
}

fn stats(
    bytes: u64,
    basis: SizeBasis,
    rows: Option<u64>,
    cardinality: Option<u64>,
) -> Option<StorageStats> {
    Some(StorageStats {
        bytes: Some(bytes),
        basis,
        rows,
        cardinality,
    })
}

#[test]
fn storage_files_are_attributed_to_columns_tables_and_relationships() {
    let ingested = ingest("storage.abf", &storage_backup(None)).unwrap();
    assert!(ingested.skips.is_empty(), "{:#?}", ingested.skips);
    let model = &ingested.value;
    let files = SizeBasis::Files;
    // Dictionary + segment + segment metadata + .hidx + the H$ table.
    assert_eq!(
        column(model, "Sales", "Amount").storage,
        stats(5000 + 1000 + 100 + 8 + 400, files, Some(1000), Some(900))
    );
    assert_eq!(
        column(model, "Sales", "ProductKey").storage,
        stats(250, files, Some(1000), Some(10))
    );
    assert_eq!(
        column(model, "Sales", "Double").storage,
        stats(300, files, Some(1000), Some(2))
    );
    assert_eq!(
        column(model, "Product", "Category").storage,
        stats(30, files, Some(10), Some(3))
    );
    assert_eq!(model.relationships[0].storage, stats(70, files, None, None));
    // Columns, RowNumber, the R$ index, and the file no storage row names.
    assert_eq!(
        model.tables[0].storage,
        stats(6508 + 250 + 300 + 40 + 70 + 3, files, Some(1000), None)
    );
    assert_eq!(model.tables[1].storage, stats(50, files, Some(10), None));
    assert_eq!(model.tables[2].storage, None, "no storage rows");
    assert_eq!(model.storage_bytes, Some(7171 + 50));
    // Keyed by graph identity for the CLI.
    let by_object = model.storage_by_object();
    assert_eq!(by_object.len(), 2 + 5 + 1);
}

#[test]
fn a_column_missing_files_falls_back_to_its_dictionary() {
    let omit = r"Sales (10).tbl\17.prt\0.Sales (10).Amount (11).0.idf";
    let ingested = ingest("storage.abf", &storage_backup(Some(omit))).unwrap();
    assert!(ingested.skips.is_empty(), "{:#?}", ingested.skips);
    let model = &ingested.value;
    let lower = SizeBasis::LowerBound;
    assert_eq!(
        column(model, "Sales", "Amount").storage,
        stats(4500, lower, Some(1000), Some(900)),
        "DictionaryStorage.Size"
    );
    assert_eq!(
        model.tables[0].storage,
        stats(7171 - 1000, lower, Some(1000), None)
    );
    assert_eq!(model.tables[1].storage.unwrap().basis, SizeBasis::Files);
    assert_eq!(model.storage_bytes, Some(7221 - 1000));
}

#[test]
fn a_catalog_without_storage_tables_has_no_statistics() {
    let ingested = ingest("plain.abf", &single_threaded(&stream(), 4096)).unwrap();
    assert!(ingested.skips.is_empty());
    let model = &ingested.value;
    assert_eq!(model.storage_bytes, None);
    assert!(model.storage_by_object().is_empty());
    assert!(model.tables.iter().all(|table| table.storage.is_none()));
}
