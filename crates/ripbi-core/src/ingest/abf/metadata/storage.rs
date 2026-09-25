//! The engine's storage catalog to per-object [`StorageStats`].
//!
//! Every data file is a `StorageFile` row (`StorageFolder.Path` +
//! `FileName`, relative to the database folder), and its size is what the
//! backup log records for that path. Files are attributed through the
//! storage rows that name them by `StorageFileID` — dictionaries, column
//! segments and their metadata, attribute-hierarchy indexes, relationship
//! indexes — so no `OwnerType` code is interpreted. A file no storage row
//! names falls back to the table owning its folder. Engine-internal tables
//! resolve to what they serve: `H$…` attribute-hierarchy tables to their
//! column (`AttributeHierarchyStorage.SystemTableID`), `R$…` relationship
//! tables to their relationship (`RelationshipIndexStorage.SystemTableID`).
//!
//! Row counts come from `ColumnStorage.Statistics_RowCount` and
//! `SegmentMapStorage.RecordCount`, cardinality from
//! `ColumnStorage.Statistics_DistinctStates`. A catalog without the storage
//! tables yields no statistics — never an error or a skip notice.

use std::collections::HashMap;

use super::{Catalog, Row};
use crate::ingest::abf::backup::LoggedFile;
use crate::model::{SizeBasis, StorageStats};

/// `Column.Type` of the engine's `RowNumber` column.
const ROW_NUMBER: i64 = 3;
/// How many engine-internal tables an owner may pass through before it is
/// treated as unresolvable (guards against cyclic catalogs).
const MAX_HOPS: usize = 8;

/// Statistics keyed by catalog `ID`.
#[derive(Default)]
pub(super) struct Storage {
    pub columns: HashMap<i64, StorageStats>,
    pub tables: HashMap<i64, StorageStats>,
    pub relationships: HashMap<i64, StorageStats>,
    /// Bytes of every catalog file found in the backup log.
    pub bytes: Option<u64>,
}

/// The storage catalog tables, loaded once.
pub(super) struct StorageCatalog {
    pub files: Vec<Row>,
    pub folders: Vec<Row>,
    pub table_storage: Vec<Row>,
    pub partition_storage: Vec<Row>,
    pub segment_maps: Vec<Row>,
    pub column_storage: Vec<Row>,
    pub dictionaries: Vec<Row>,
    pub column_partitions: Vec<Row>,
    pub segments: Vec<Row>,
    pub column_indexes: Vec<Row>,
    pub string_indexes: Vec<Row>,
    pub attribute_hierarchies: Vec<Row>,
    pub attribute_hierarchy_storage: Vec<Row>,
    pub relationship_storage: Vec<Row>,
    pub relationship_indexes: Vec<Row>,
}

/// The object a file's bytes belong to, by catalog ID.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Owner {
    Column(i64),
    Relationship(i64),
    Table(i64),
}

/// Bytes attributed to one object, and whether any of its files had no size.
#[derive(Default, Clone, Copy)]
struct Tally {
    bytes: u64,
    missing: bool,
}

impl Tally {
    fn add(&mut self, size: Option<u64>) {
        match size {
            Some(size) => self.bytes = self.bytes.saturating_add(size),
            None => self.missing = true,
        }
    }

    fn stats(self) -> StorageStats {
        StorageStats {
            bytes: Some(self.bytes),
            basis: if self.missing {
                SizeBasis::LowerBound
            } else {
                SizeBasis::Files
            },
            rows: None,
            cardinality: None,
        }
    }
}

/// Attributes every catalog file to a table, column, or relationship.
pub(super) fn attribute(catalog: &Catalog, logged: &[LoggedFile]) -> Storage {
    let storage = &catalog.storage;
    if storage.column_storage.is_empty() && storage.files.is_empty() {
        return Storage::default();
    }
    let columns = by_id(&catalog.columns);
    let partitions = by_id(&catalog.partitions);
    let relationships = by_id(&catalog.relationships);
    let column_storage = by_id(&storage.column_storage);
    let column_partitions = by_id(&storage.column_partitions);
    let attribute_hierarchies = by_id(&storage.attribute_hierarchies);
    let relationship_storage = by_id(&storage.relationship_storage);
    let partition_storage = by_id(&storage.partition_storage);
    let dictionaries = by_id(&storage.dictionaries);

    let column_of_storage = |id: Option<i64>| {
        id.and_then(|id| column_storage.get(&id))
            .and_then(|row| row.int("ColumnID"))
            .map(Owner::Column)
    };
    let relationship_of_index = |row: &Row| {
        row.int("RelationshipStorageID")
            .and_then(|id| relationship_storage.get(&id))
            .and_then(|row| row.int("RelationshipID"))
            .map(Owner::Relationship)
    };
    let column_of_hierarchy = |row: &Row| {
        row.int("AttributeHierarchyID")
            .and_then(|id| attribute_hierarchies.get(&id))
            .and_then(|row| row.int("ColumnID"))
            .map(Owner::Column)
    };

    // Files named by a storage row.
    let mut file_owner: HashMap<i64, Owner> = HashMap::new();
    let mut claim = |file: Option<i64>, owner: Option<Owner>| {
        if let (Some(file), Some(owner)) = (file.filter(|id| *id > 0), owner) {
            file_owner.entry(file).or_insert(owner);
        }
    };
    for rows in [
        &storage.dictionaries,
        &storage.column_partitions,
        &storage.column_indexes,
        &storage.string_indexes,
    ] {
        for row in rows {
            claim(
                row.int("StorageFileID"),
                column_of_storage(row.int("ColumnStorageID")),
            );
        }
    }
    for row in &storage.segments {
        let column = row
            .int("ColumnPartitionStorageID")
            .and_then(|id| column_partitions.get(&id))
            .and_then(|row| column_of_storage(row.int("ColumnStorageID")));
        claim(row.int("StorageFileID"), column);
    }
    for row in &storage.attribute_hierarchy_storage {
        claim(row.int("StorageFileID"), column_of_hierarchy(row));
    }
    for row in &storage.relationship_indexes {
        claim(row.int("StorageFileID"), relationship_of_index(row));
    }

    // Folders, for files no storage row names.
    let mut folder_owner: HashMap<i64, Owner> = HashMap::new();
    for row in &storage.table_storage {
        if let (Some(folder), Some(table)) = (row.int("StorageFolderID"), row.int("TableID")) {
            folder_owner.insert(folder, Owner::Table(table));
        }
    }
    for row in &storage.partition_storage {
        let table = row
            .int("PartitionID")
            .and_then(|id| partitions.get(&id))
            .and_then(|row| row.int("TableID"));
        if let (Some(folder), Some(table)) = (row.int("StorageFolderID"), table) {
            folder_owner.insert(folder, Owner::Table(table));
        }
    }

    // Engine-internal tables, to the object they serve.
    let mut system_owner: HashMap<i64, Owner> = HashMap::new();
    for row in &storage.attribute_hierarchy_storage {
        if let (Some(table), Some(owner)) = (
            row.int("SystemTableID").filter(|id| *id > 0),
            column_of_hierarchy(row),
        ) {
            system_owner.insert(table, owner);
        }
    }
    for row in &storage.relationship_indexes {
        for key in ["SystemTableID", "SecondarySystemTableID"] {
            if let (Some(table), Some(owner)) = (
                row.int(key).filter(|id| *id > 0),
                relationship_of_index(row),
            ) {
                system_owner.insert(table, owner);
            }
        }
    }

    let table_of_column = |id: i64| columns.get(&id).and_then(|row| row.int("TableID"));
    // The user-facing object an owner stands for, and the table carrying it.
    let resolve = |mut owner: Owner| -> Option<(Owner, i64)> {
        for _ in 0..MAX_HOPS {
            let table = match owner {
                Owner::Column(id) => table_of_column(id)?,
                Owner::Relationship(id) => table_of_column(
                    relationships
                        .get(&id)
                        .and_then(|row| row.int("FromColumnID"))?,
                )?,
                Owner::Table(id) => id,
            };
            if let Some(&served) = system_owner.get(&table) {
                owner = served;
                continue;
            }
            if let Owner::Column(id) = owner
                && columns.get(&id).and_then(|row| row.int("Type")) == Some(ROW_NUMBER)
            {
                owner = Owner::Table(table);
            }
            return Some((owner, table));
        }
        None
    };

    let sizes: HashMap<String, u64> = logged
        .iter()
        .map(|file| (file.path.to_lowercase(), file.size))
        .collect();
    let folder_paths: HashMap<i64, String> = storage
        .folders
        .iter()
        .filter_map(|row| Some((row.id(), row.text("Path")?)))
        .collect();
    let size_of = |file: &Row| -> Option<u64> {
        let name = file.text("FileName")?;
        let path = match file
            .int("StorageFolderID")
            .and_then(|id| folder_paths.get(&id))
        {
            Some(folder) if !folder.is_empty() => format!("{folder}\\{name}"),
            _ => name,
        };
        sizes.get(&path.to_lowercase()).copied()
    };

    let mut tallies: HashMap<Owner, Tally> = HashMap::new();
    let mut dictionary_file: HashMap<i64, u64> = HashMap::new();
    let mut found = None::<u64>;
    let dictionary_files: HashMap<i64, i64> = storage
        .dictionaries
        .iter()
        .filter_map(|row| Some((row.int("StorageFileID")?, row.int("ColumnStorageID")?)))
        .collect();
    for file in &storage.files {
        let size = size_of(file);
        if let Some(size) = size {
            found = Some(found.unwrap_or(0).saturating_add(size));
        }
        let owner = file_owner.get(&file.id()).copied().or_else(|| {
            file.int("StorageFolderID")
                .and_then(|id| folder_owner.get(&id))
                .copied()
        });
        let Some((owner, table)) = owner.and_then(resolve) else {
            continue;
        };
        if let (Some(size), Some(Owner::Column(column))) = (
            size,
            dictionary_files
                .get(&file.id())
                .and_then(|id| column_of_storage(Some(*id))),
        ) {
            dictionary_file.insert(column, size);
        }
        tallies.entry(owner).or_default().add(size);
        if owner != Owner::Table(table) {
            tallies.entry(Owner::Table(table)).or_default().add(size);
        }
    }
    let has_files = !storage.files.is_empty();

    let mut result = Storage {
        bytes: found,
        ..Storage::default()
    };
    // Columns: every column with a storage row.
    for row in &storage.column_storage {
        let Some(column) = row.int("ColumnID") else {
            continue;
        };
        let tally = tallies
            .get(&Owner::Column(column))
            .copied()
            .unwrap_or_default();
        let bytes = if !has_files {
            None
        } else if tally.missing {
            let dictionary = row
                .int("DictionaryStorageID")
                .and_then(|id| dictionaries.get(&id))
                .and_then(|d| d.int("Size"))
                .and_then(|size| u64::try_from(size).ok());
            dictionary.or_else(|| dictionary_file.get(&column).copied())
        } else {
            Some(tally.bytes)
        };
        result.columns.insert(
            column,
            StorageStats {
                bytes,
                basis: tally.stats().basis,
                rows: count(row, "Statistics_RowCount"),
                cardinality: count(row, "Statistics_DistinctStates"),
            },
        );
    }
    // Tables: rows over their partitions' segment maps.
    let mut table_rows: HashMap<i64, u64> = HashMap::new();
    for row in &storage.segment_maps {
        let table = row
            .int("PartitionStorageID")
            .and_then(|id| partition_storage.get(&id))
            .and_then(|row| row.int("PartitionID"))
            .and_then(|id| partitions.get(&id))
            .and_then(|row| row.int("TableID"));
        if let (Some(table), Some(records)) = (table, count(row, "RecordCount")) {
            let total = table_rows.entry(table).or_default();
            *total = total.saturating_add(records);
        }
    }
    for table in catalog.tables.iter().map(Row::id) {
        let tally = tallies.get(&Owner::Table(table)).copied();
        let rows = table_rows.get(&table).copied();
        if tally.is_none() && rows.is_none() {
            continue;
        }
        let mut stats = tally.unwrap_or_default().stats();
        if !has_files {
            stats.bytes = None;
        }
        stats.rows = rows;
        result.tables.insert(table, stats);
    }
    for (owner, tally) in tallies {
        if let Owner::Relationship(id) = owner {
            result.relationships.insert(id, tally.stats());
        }
    }
    result
}

fn by_id(rows: &[Row]) -> HashMap<i64, &Row> {
    rows.iter().map(|row| (row.id(), row)).collect()
}

/// A non-negative count column.
fn count(row: &Row, name: &str) -> Option<u64> {
    row.int(name).and_then(|value| u64::try_from(value).ok())
}
