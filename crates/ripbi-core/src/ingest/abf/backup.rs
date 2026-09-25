//! The decoded backup: a header page, a virtual directory of stored files,
//! and a backup log naming them. Only `metadata.sqlitedb` is extracted; every
//! other file contributes its path and size, never its contents.
//!
//! Layout of the decoded stream:
//! - bytes `0..72` — the UTF-16 `STREAM_STORAGE` signature;
//! - bytes `72..4096` — the `BackupLog` header XML (UTF-16, NUL-padded), whose
//!   `m_cbOffsetHeader`/`DataSize` locate the virtual directory;
//! - the `VirtualDirectory` XML lists every stored file by `Path` (a storage
//!   key), `m_cbOffsetHeader` and `Size`; its last entry is the backup log;
//! - the `BackupLog` XML maps each original file `Path` to its `StoragePath`.

use std::io::{Read, Seek, SeekFrom};

use quick_xml::Reader;
use quick_xml::events::Event;

use super::container::{STREAM_STORAGE_LEN, corrupt, stream_storage_signature};
use crate::Result;

/// The header page is always one 4 KiB page.
const HEADER_PAGE: u64 = 4096;
/// XML documents in a backup are small; anything larger is corruption.
const MAX_XML: u64 = 256 * 1024 * 1024;
/// The metadata database of even a very large model is far below this.
const MAX_METADATA: u64 = 2 * 1024 * 1024 * 1024;

/// What the ingest reads out of a decoded backup.
pub(super) struct Contents {
    /// The bytes of `metadata.sqlitedb`.
    pub metadata: Vec<u8>,
    /// Every logged file inside the database folder, with the size the
    /// virtual directory records for it.
    pub files: Vec<LoggedFile>,
}

/// One file of the backed-up database folder.
pub(super) struct LoggedFile {
    /// The path relative to the database folder, e.g.
    /// `Sales (12).tbl\0.Sales (12).Amount (20).dictionary` — the
    /// `StorageFolder.Path` + `StorageFile.FileName` the catalog records.
    pub path: String,
    /// Stored size in bytes.
    pub size: u64,
}

/// Reads `metadata.sqlitedb` and the logged file sizes out of a decoded backup.
pub(super) fn read(file: &mut (impl Read + Seek), len: u64) -> Result<Contents> {
    if len < HEADER_PAGE {
        return Err(corrupt(format!(
            "the decoded backup is {len} bytes, shorter than its header page"
        )));
    }
    let page = read_range(file, len, 0, HEADER_PAGE, "header page")?;
    if !page.starts_with(&stream_storage_signature()) {
        return Err(corrupt(
            "the decoded backup has no STREAM_STORAGE signature",
        ));
    }
    let header = parse(&page[STREAM_STORAGE_LEN..], "backup log header")?;
    let directory_offset = number(&header, "m_cbOffsetHeader", "backup log header")?;
    let directory_size = number(&header, "DataSize", "backup log header")?;
    let error_code = header.child("ErrorCode").is_some_and(|e| e.text == "true");

    let directory = read_range(
        file,
        len,
        directory_offset,
        directory_size,
        "virtual directory",
    )?;
    let directory = parse(&directory, "virtual directory")?;
    let entries: Vec<StoredFile> = directory
        .children("BackupFile")
        .map(|entry| {
            Ok(StoredFile {
                path: entry.child_text("Path").unwrap_or_default().to_string(),
                offset: number(entry, "m_cbOffsetHeader", "virtual directory entry")?,
                size: number(entry, "Size", "virtual directory entry")?,
            })
        })
        .collect::<Result<_>>()?;
    let log_entry = entries
        .last()
        .ok_or_else(|| corrupt("the virtual directory is empty"))?;
    let mut log = read_range(file, len, log_entry.offset, log_entry.size, "backup log")?;
    if error_code {
        log.truncate(log.len().saturating_sub(4));
    }
    let log = parse(&log, "backup log")?;

    let logged = || {
        log.children("FileGroups")
            .flat_map(|groups| groups.children("FileGroup"))
            .flat_map(|group| {
                group
                    .children("FileList")
                    .flat_map(|list| list.children("BackupFile"))
                    .map(move |file| (group, file))
            })
    };
    // Sizes are best effort: a file that does not join the virtual directory,
    // or lies outside the database folder, simply has no size.
    let files = logged()
        .filter_map(|(group, file)| {
            let path = relative_path(
                group.child_text("PersistLocationPath"),
                file.child_text("Path")?,
            )?;
            let storage = file.child_text("StoragePath")?;
            let size = entries.iter().find(|entry| entry.path == storage)?.size;
            Some(LoggedFile { path, size })
        })
        .collect();

    let stored = logged()
        .map(|(_, file)| file)
        .find(|file| {
            file.child_text("Path").is_some_and(|path| {
                path.rsplit(['\\', '/'])
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case("metadata.sqlitedb"))
            })
        })
        .ok_or_else(|| corrupt("the backup log lists no metadata.sqlitedb"))?;
    let storage = stored
        .child_text("StoragePath")
        .ok_or_else(|| corrupt("metadata.sqlitedb has no StoragePath"))?;
    let entry = entries
        .iter()
        .find(|entry| entry.path == storage)
        .ok_or_else(|| {
            corrupt(format!(
                "metadata.sqlitedb storage '{storage}' is missing from the virtual directory"
            ))
        })?;
    if entry.size > MAX_METADATA {
        return Err(corrupt("metadata.sqlitedb is implausibly large"));
    }
    let metadata = read_range(file, len, entry.offset, entry.size, "metadata.sqlitedb")?;
    Ok(Contents { metadata, files })
}

/// `path` relative to its file group's database folder: below
/// `PersistLocationPath` when the log records one, else below the first
/// `….db` folder. `None` for files outside the database folder.
fn relative_path(folder: Option<&str>, path: &str) -> Option<String> {
    let lower = path.to_lowercase();
    let start = match folder.map(|folder| folder.trim_end_matches(['\\', '/']).to_lowercase()) {
        Some(folder) if !folder.is_empty() => lower.starts_with(&folder).then_some(folder.len())?,
        _ => lower.find(".db\\").map(|index| index + ".db".len())?,
    };
    let rest = path.get(start..)?;
    let rest = rest.strip_prefix(['\\', '/'])?;
    (!rest.is_empty()).then(|| rest.to_string())
}

struct StoredFile {
    path: String,
    offset: u64,
    size: u64,
}

fn read_range(
    file: &mut (impl Read + Seek),
    len: u64,
    offset: u64,
    size: u64,
    what: &str,
) -> Result<Vec<u8>> {
    let end = offset
        .checked_add(size)
        .filter(|end| *end <= len)
        .ok_or_else(|| {
            corrupt(format!(
                "{what} at {offset}+{size} lies outside the {len}-byte backup"
            ))
        })?;
    if what != "metadata.sqlitedb" && size > MAX_XML {
        return Err(corrupt(format!("{what} is implausibly large")));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; usize::try_from(end - offset).map_err(|_| corrupt("range overflow"))?];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn number(element: &Element, name: &str, what: &str) -> Result<u64> {
    element
        .child_text(name)
        .and_then(|text| text.trim().parse().ok())
        .ok_or_else(|| corrupt(format!("{what} has no valid {name}")))
}

/// A parsed XML element: name, direct text, and child elements. Attributes
/// are never used by the backup format and are ignored.
#[derive(Debug, Default)]
struct Element {
    name: String,
    text: String,
    children: Vec<Element>,
}

impl Element {
    fn children<'a, 'n>(
        &'a self,
        name: &'n str,
    ) -> impl Iterator<Item = &'a Element> + use<'a, 'n> {
        self.children.iter().filter(move |child| child.name == name)
    }

    fn child(&self, name: &str) -> Option<&Element> {
        self.children(name).next()
    }

    fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name).map(|child| child.text.as_str())
    }
}

/// Decodes (UTF-16 with or without BOM, or UTF-8) and parses one document,
/// returning its root element.
fn parse(bytes: &[u8], what: &str) -> Result<Element> {
    let text = decode_text(bytes).ok_or_else(|| corrupt(format!("{what} is not valid text")))?;
    let text = text.trim_end_matches('\0');
    let mut reader = Reader::from_str(text);
    let mut stack = vec![Element::default()];
    loop {
        let event = reader
            .read_event()
            .map_err(|error| corrupt(format!("{what} is not well-formed XML: {error}")))?;
        match event {
            Event::Start(start) => stack.push(Element {
                name: start.local_name().as_ref().to_string(),
                ..Element::default()
            }),
            Event::Empty(empty) => {
                let element = Element {
                    name: empty.local_name().as_ref().to_string(),
                    ..Element::default()
                };
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element);
                }
            }
            Event::End(_) => {
                let element = stack
                    .pop()
                    .filter(|_| !stack.is_empty())
                    .ok_or_else(|| corrupt(format!("{what} has an unbalanced end tag")))?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element);
                }
            }
            Event::Text(text) => {
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(&text);
                }
            }
            Event::CData(data) => {
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(&data);
                }
            }
            Event::GeneralRef(reference) => {
                let resolved = match reference.resolve_char_ref() {
                    Ok(Some(ch)) => ch,
                    _ => match &*reference {
                        "amp" => '&',
                        "lt" => '<',
                        "gt" => '>',
                        "quot" => '"',
                        "apos" => '\'',
                        other => {
                            return Err(corrupt(format!("{what} uses unknown entity &{other};")));
                        }
                    },
                };
                if let Some(current) = stack.last_mut() {
                    current.text.push(resolved);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut document = stack
        .pop()
        .filter(|_| stack.is_empty())
        .ok_or_else(|| corrupt(format!("{what} ends before its root element closes")))?;
    if document.children.len() != 1 {
        return Err(corrupt(format!("{what} has no single root element")));
    }
    Ok(document.children.remove(0))
}

fn decode_text(bytes: &[u8]) -> Option<String> {
    let utf16 = |bytes: &[u8]| {
        let (chunks, _) = bytes.as_chunks::<2>();
        let units: Vec<u16> = chunks
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16(&units).ok()
    };
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        utf16(rest)
    } else if bytes.get(1) == Some(&0) {
        utf16(bytes)
    } else {
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        String::from_utf8(bytes.to_vec()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, relative_path};

    #[test]
    fn logged_paths_are_relative_to_the_database_folder() {
        let folder = Some(r"\\?\C:\Data\m.2.db");
        assert_eq!(
            relative_path(
                folder,
                r"\\?\C:\DATA\m.2.db\T (1).tbl\0.T (1).C (2).dictionary"
            )
            .as_deref(),
            Some(r"T (1).tbl\0.T (1).C (2).dictionary")
        );
        assert_eq!(relative_path(folder, r"\\?\C:\Data\m.3.db.xml"), None);
        assert_eq!(
            relative_path(None, r"C:\Data\m.0.db\metadata.sqlitedb").as_deref(),
            Some("metadata.sqlitedb")
        );
    }

    #[test]
    fn xml_entities_and_nesting_parse() {
        let doc = parse(b"<a><b>x &amp; &#x41;</b><c/><d><e>1</e></d></a>", "t").unwrap();
        assert_eq!(doc.child_text("b"), Some("x & A"));
        assert!(doc.child("c").is_some());
        assert_eq!(doc.child("d").and_then(|d| d.child_text("e")), Some("1"));
    }

    #[test]
    fn malformed_xml_is_an_error() {
        assert!(parse(b"<a><b></a>", "t").is_err());
        assert!(parse(b"<a>", "t").is_err());
        assert!(parse(b"", "t").is_err());
    }
}
