//! Builds synthetic ABF backups — a TMSCHEMA `metadata.sqlitedb` wrapped in
//! the backup header, virtual directory and backup log, framed as
//! `STREAM_STORAGE` or single-/multithreaded XPress9 — and PBIX archives that
//! carry them. The layout mirrors what Power BI Desktop writes (checked
//! against Microsoft's public `Revenue Opportunities.pbix`).

#![allow(dead_code)]

use std::io::{Cursor, Write};

use ripbi_xpress9::Encoder;
use rusqlite::Connection;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

/// The catalog tables the fixtures use, with only the columns read.
const SCHEMA: &str = r#"
CREATE TABLE "Model" (ID INTEGER, Name TEXT);
CREATE TABLE "Table" (ID INTEGER, ModelID INTEGER, Name TEXT, IsHidden INTEGER,
    IsPrivate INTEGER, SystemFlags INTEGER, DefaultDetailRowsDefinitionID INTEGER,
    CalculationGroupID INTEGER, RefreshPolicyID INTEGER);
CREATE TABLE "Column" (ID INTEGER, TableID INTEGER, ExplicitName TEXT, InferredName TEXT,
    Type INTEGER, Expression TEXT, IsHidden INTEGER, SortByColumnID INTEGER,
    RelatedColumnDetailsID INTEGER);
CREATE TABLE "Measure" (ID INTEGER, TableID INTEGER, Name TEXT, Expression TEXT,
    IsHidden INTEGER, KPIID INTEGER, DetailRowsDefinitionID INTEGER,
    FormatStringDefinitionID INTEGER);
CREATE TABLE "Partition" (ID INTEGER, TableID INTEGER, Name TEXT, Type INTEGER,
    QueryDefinition TEXT);
CREATE TABLE "Relationship" (ID INTEGER, ModelID INTEGER, Name TEXT, IsActive INTEGER,
    FromTableID INTEGER, FromColumnID INTEGER, ToTableID INTEGER, ToColumnID INTEGER);
CREATE TABLE "Hierarchy" (ID INTEGER, TableID INTEGER, Name TEXT, IsHidden INTEGER);
CREATE TABLE "Level" (ID INTEGER, HierarchyID INTEGER, Ordinal INTEGER, Name TEXT,
    ColumnID INTEGER);
CREATE TABLE "Role" (ID INTEGER, ModelID INTEGER, Name TEXT);
CREATE TABLE "TablePermission" (ID INTEGER, RoleID INTEGER, TableID INTEGER,
    FilterExpression TEXT);
CREATE TABLE "ColumnPermission" (ID INTEGER, TablePermissionID INTEGER, ColumnID INTEGER,
    MetadataPermission INTEGER);
CREATE TABLE "Expression" (ID INTEGER, ModelID INTEGER, Name TEXT, Kind INTEGER,
    Expression TEXT, ParameterValuesColumnID INTEGER);
CREATE TABLE "FormatStringDefinition" (ID INTEGER, ObjectID INTEGER, ObjectType INTEGER,
    Expression TEXT);
CREATE TABLE "CalculationGroup" (ID INTEGER, TableID INTEGER);
CREATE TABLE "CalculationItem" (ID INTEGER, CalculationGroupID INTEGER, Name TEXT,
    Expression TEXT, Ordinal INTEGER, FormatStringDefinitionID INTEGER);
CREATE TABLE "CalculationExpression" (ID INTEGER, CalculationGroupID INTEGER,
    Expression TEXT, SelectionMode INTEGER, FormatStringDefinitionID INTEGER);
CREATE TABLE "Function" (ID INTEGER, ModelID INTEGER, Name TEXT, Expression TEXT,
    IsHidden INTEGER);
"#;

/// A small model: `Sales` (data, calculated and RowNumber columns, a
/// measure with a format string, an M partition) related to `Product`, a
/// hierarchy, a role, a calculation group, a shared expression, a function,
/// and an engine-internal `H$` table that must be dropped.
const ROWS: &str = r##"
INSERT INTO "Model" VALUES (1, 'Model');
INSERT INTO "Table" VALUES
    (10, 1, 'Sales', 0, 0, 0, NULL, NULL, NULL),
    (20, 1, 'Product', 0, 0, 0, NULL, NULL, NULL),
    (30, 1, 'H$Sales (10)$Amount (11)', 1, 0, 1, NULL, NULL, NULL),
    (40, 1, 'Time Intelligence', 0, 0, 0, NULL, 400, NULL);
INSERT INTO "Column" VALUES
    (11, 10, 'Amount', NULL, 1, NULL, 0, NULL, NULL),
    (12, 10, NULL, 'ProductKey', 1, NULL, 1, NULL, NULL),
    (13, 10, 'Double', NULL, 2, 'Sales[Amount] * 2', 0, 11, NULL),
    (14, 10, 'RowNumber-2662979B-1795-4F74-8F37-6A1BA8059B61', NULL, 3, NULL, 1, NULL, NULL),
    (21, 20, 'Key', NULL, 1, NULL, 0, NULL, NULL),
    (22, 20, 'Category', NULL, 1, NULL, 0, NULL, NULL),
    (31, 30, 'POS_TO_ID', NULL, 1, NULL, 1, NULL, NULL),
    (41, 40, 'Name', NULL, 1, NULL, 0, NULL, NULL);
INSERT INTO "Measure" VALUES
    (15, 10, 'Total', 'SUM(Sales[Amount])', 0, NULL, NULL, 16);
INSERT INTO "FormatStringDefinition" VALUES
    (16, 15, 8, '"#,0"'),
    (403, 402, 47, '"0.0%"');
INSERT INTO "Partition" VALUES
    (17, 10, 'Sales', 4, 'let Source = Raw in Source'),
    (23, 20, 'Product', 4, 'let Source = #table({"Key"}, {}) in Source'),
    (32, 30, 'H$Sales (10)$Amount (11)', 3, NULL),
    (42, 40, 'Time Intelligence', 7, NULL);
INSERT INTO "Relationship" VALUES (50, 1, 'rel-1', 1, 10, 12, 20, 21);
INSERT INTO "Hierarchy" VALUES (60, 20, 'Products', 0);
INSERT INTO "Level" VALUES (62, 60, 1, 'Key', 21), (61, 60, 0, 'Category', 22);
INSERT INTO "Role" VALUES (70, 1, 'Reader');
INSERT INTO "TablePermission" VALUES (71, 70, 20, '[Category] <> "Hidden"');
INSERT INTO "ColumnPermission" VALUES (72, 71, 22, 1);
INSERT INTO "Expression" VALUES (80, 1, 'Raw', 0, 'let Source = 1 in Source', NULL);
INSERT INTO "Function" VALUES (90, 1, 'Double', '(x) => x * 2', 0);
INSERT INTO "CalculationGroup" VALUES (400, 40);
INSERT INTO "CalculationItem" VALUES (402, 400, 'YTD', 'TOTALYTD(SELECTEDMEASURE(), ''Sales''[Amount])', 0, 403);
INSERT INTO "CalculationExpression" VALUES (404, 400, 'SELECTEDMEASURE()', 2, NULL);
"##;

/// The fixture model's `metadata.sqlitedb` bytes, after `extra` SQL runs.
pub fn metadata_db(extra: &str) -> Vec<u8> {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(SCHEMA).unwrap();
    connection.execute_batch(ROWS).unwrap();
    connection.execute_batch(extra).unwrap();
    connection.serialize("main").unwrap().to_vec()
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn utf16_bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(utf16(text));
    bytes
}

/// Knobs for corrupting the decoded stream.
#[derive(Default, Clone)]
pub struct Backup {
    /// Added to the header's virtual-directory offset.
    pub directory_offset_shift: u64,
    /// The file name the backup log records for the metadata database.
    pub metadata_name: Option<String>,
}

/// The decoded (`STREAM_STORAGE`) backup holding `db` as `metadata.sqlitedb`.
pub fn decoded_backup(db: &[u8], options: &Backup) -> Vec<u8> {
    let mut stream = utf16_bom("STREAM_STORAGE_SIGNATURE_)!@#$%^&*(");
    assert_eq!(stream.len(), 72);
    stream.resize(4096, 0); // header page, filled in below

    let db_offset = stream.len() as u64;
    stream.extend_from_slice(db);

    let name = options
        .metadata_name
        .clone()
        .unwrap_or_else(|| "metadata.sqlitedb".to_string());
    let log = utf16_bom(&format!(
        "<BackupLog><BackupRestoreSyncVersion>11.53</BackupRestoreSyncVersion>\
         <ObjectName>Model</ObjectName><FileGroups><FileGroup><Class>100002</Class>\
         <ID>db</ID><FileList><BackupFile>\
         <Path>\\\\?\\C:\\Data\\db.0.db\\{name}</Path><StoragePath>K0</StoragePath>\
         <LastWriteTime>0</LastWriteTime><Size>{}</Size></BackupFile></FileList>\
         </FileGroup></FileGroups></BackupLog>",
        db.len()
    ));
    let log_offset = stream.len() as u64;
    stream.extend_from_slice(&log);

    let directory = format!(
        "<VirtualDirectory><BackupFile><Path>K0</Path><Size>{}</Size>\
         <m_cbOffsetHeader>{db_offset}</m_cbOffsetHeader><Delete>false</Delete></BackupFile>\
         <BackupFile><Path>LOG</Path><Size>{}</Size>\
         <m_cbOffsetHeader>{log_offset}</m_cbOffsetHeader><Delete>false</Delete></BackupFile>\
         </VirtualDirectory>",
        db.len(),
        log.len()
    );
    let directory_offset = stream.len() as u64;
    stream.extend_from_slice(directory.as_bytes());

    let header = utf16(&format!(
        "<BackupLog><BackupRestoreSyncVersion>140</BackupRestoreSyncVersion>\
         <Fault>false</Fault><ErrorCode>false</ErrorCode>\
         <ApplyCompression>false</ApplyCompression>\
         <m_cbOffsetHeader>{}</m_cbOffsetHeader><DataSize>{}</DataSize>\
         <Files>2</Files><m_cbOffsetData>4096</m_cbOffsetData></BackupLog>",
        directory_offset + options.directory_offset_shift,
        directory.len()
    ));
    stream[72..72 + header.len()].copy_from_slice(&header);
    stream
}

fn signature(text: &str) -> Vec<u8> {
    let mut header = utf16(text);
    header.resize(102, 0);
    header
}

fn chunk(encoder: &mut Encoder, data: &[u8], out: &mut Vec<u8>) {
    let compressed = encoder.encode_chunk(data).unwrap();
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
    out.extend_from_slice(&compressed);
}

/// Single-threaded XPress9 framing: one encoder across all chunks.
pub fn single_threaded(stream: &[u8], chunk_size: usize) -> Vec<u8> {
    let mut out = signature("This backup was created using XPress9 compression.");
    let mut encoder = Encoder::new().unwrap();
    for data in stream.chunks(chunk_size) {
        chunk(&mut encoder, data, &mut out);
    }
    out
}

/// Multithreaded XPress9 framing: one prefix group of one chunk, then
/// `main_threads` groups sharing the rest; each group has its own encoder.
/// The stream is zero-padded to fill the last group, as offsets are explicit.
pub fn multithreaded(stream: &[u8], chunk_size: usize, main_threads: usize) -> Vec<u8> {
    let mut chunks: Vec<Vec<u8>> = stream.chunks(chunk_size).map(<[u8]>::to_vec).collect();
    let rest = chunks.len().saturating_sub(1);
    let main_chunks = rest.div_ceil(main_threads).max(1);
    while chunks.len() < 1 + main_chunks * main_threads {
        chunks.push(vec![0; chunk_size]);
    }
    let mut out = signature("This backup was created using multithreaded XPrs9.");
    for field in [main_chunks, 1, 1, main_threads, chunk_size] {
        out.extend_from_slice(&(field as u64).to_le_bytes());
    }
    let mut groups = vec![&chunks[..1]];
    groups.extend(chunks[1..].chunks(main_chunks));
    for group in groups {
        let mut encoder = Encoder::new().unwrap();
        for data in group {
            chunk(&mut encoder, data, &mut out);
        }
    }
    out
}

/// A PBIX-shaped ZIP with the given members (`DataModel` stored, as Desktop
/// writes it).
pub fn archive(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in members {
        let options = if *name == "DataModel" {
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
        } else {
            SimpleFileOptions::default()
        };
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// An MS-QDEFF `DataMashup` whose single section holds `formulas`.
pub fn data_mashup(formulas: &str) -> Vec<u8> {
    let package = archive(&[("Formulas/Section1.m", formulas.as_bytes())]);
    let mut out = 0u32.to_le_bytes().to_vec();
    out.extend_from_slice(&(package.len() as u32).to_le_bytes());
    out.extend_from_slice(&package);
    out.extend_from_slice(&[0; 16]);
    out
}
