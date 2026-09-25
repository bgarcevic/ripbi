//! Synthetic ABF backups, standalone and inside PBIX archives: every framing
//! decodes to the same model, and malformed input fails with a typed error
//! or a skip notice, never a panic.

mod common;

use std::path::{Path, PathBuf};

use common::{
    Backup, archive, data_mashup, decoded_backup, metadata_db, multithreaded, single_threaded,
};
use ripbi_core::ingest::{SkipKind, semantic_model};
use ripbi_core::model::{ColumnKind, MetadataPermission, PartitionSource, TabularDatabase};
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
