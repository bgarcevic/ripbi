//! Analysis Services backups (`.abf`, and the PBIX `DataModel` member) to the
//! shared model AST.
//!
//! Only metadata is read: the container is decoded into a temporary file
//! (deleted on drop, including on every error path), `metadata.sqlitedb` is
//! extracted from it, and that catalog is mapped. VertiPaq row data and
//! storage statistics are never interpreted.

mod backup;
mod container;
mod metadata;

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::Result;
use crate::ingest::SkipNotice;
use crate::model::TabularDatabase;

/// Whether `header` (the first bytes of a file or member) opens an ABF
/// backup in any supported framing.
pub(super) fn is_abf(header: &[u8]) -> bool {
    container::framing(header).is_some()
}

/// How many leading bytes [`is_abf`] needs.
pub(super) const SIGNATURE_LEN: usize = container::COMPRESSED_HEADER_LEN;

/// Decodes a backup read from `reader` and maps its metadata. `path` names
/// the source in skip notices.
pub(super) fn load(
    reader: impl Read,
    path: &Path,
    skips: &mut Vec<SkipNotice>,
) -> Result<TabularDatabase> {
    load_with_temp(reader, path, skips, tempfile::tempfile)
}

/// [`load`] with the scratch file supplied by the caller, so tests can
/// observe that it is released.
pub(super) fn load_with_temp(
    reader: impl Read,
    path: &Path,
    skips: &mut Vec<SkipNotice>,
    temp: impl FnOnce() -> std::io::Result<File>,
) -> Result<TabularDatabase> {
    let mut scratch = BufWriter::new(temp()?);
    let len = container::decode(reader, &mut scratch)?;
    scratch.flush()?;
    let mut scratch = scratch.into_inner().map_err(|error| error.into_error())?;
    scratch.seek(SeekFrom::Start(0))?;
    let db = backup::metadata_db(&mut scratch, len)?;
    drop(scratch);
    metadata::load(&db, path, skips)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{container, load_with_temp};

    fn assert_released(input: Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let mut skips = Vec::new();
        let result = load_with_temp(&input[..], Path::new("t.abf"), &mut skips, || {
            tempfile::tempfile_in(dir.path())
        });
        assert!(result.is_err());
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(left.is_empty(), "scratch file left behind: {left:?}");
    }

    #[test]
    fn scratch_file_is_released_when_the_backup_is_malformed() {
        // Decodes fine, then fails on the missing header page.
        let mut input = container::stream_storage_signature();
        input.resize(200, 0);
        assert_released(input);
    }

    #[test]
    fn scratch_file_is_released_when_decoding_fails() {
        let mut input: Vec<u8> = "This backup was created using XPress9 compression."
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        input.resize(container::COMPRESSED_HEADER_LEN, 0);
        input.extend_from_slice(&[16, 0, 0, 0, 4, 0, 0, 0, 1, 2, 3, 4]);
        assert_released(input);
    }
}
