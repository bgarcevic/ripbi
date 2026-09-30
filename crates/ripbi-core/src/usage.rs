//! Query-log usage: the DAX and MDX queries a semantic model actually served.
//!
//! Report bindings only cover the reports ripbi was given. Excel pivots, thin
//! reports elsewhere in the tenant, and XMLA clients query the model too, and
//! Fabric workspace monitoring (`SemanticModelLogs`, or the Log Analytics
//! `PowerBIDatasetsWorkspace` table) captures every one of those queries as a
//! `QueryEnd` event whose `EventText` is the query itself. This module parses an
//! export of those events into a [`QueryLog`] and resolves each query's object
//! references against the model — the graph then treats the referenced objects
//! as roots beside the report bindings and `ripbi_keep` annotations.
//!
//! Parsing is resilient in the same way as every other ingestion path: the
//! export is matched by column name, never by position; unknown columns are
//! ignored; engine noise (`VertiPaqSEQuery*`, `Discover*`, `ExecutionMetrics`, …)
//! is skipped silently; and a query row that cannot be used becomes a
//! [`SkipNotice`], not an error. Only a file with no `EventText` column at all
//! is an error — it is not a query log.
//!
//! Resolution follows the DAX binder's conservatism: a query reference keeps
//! every candidate it could name alive. Query-local definitions
//! (`DEFINE MEASURE`, `VAR __DS0Core`) resolve to nothing and are dropped.
//!
//! ```
//! use ripbi_core::usage::{self, QueryLanguage};
//!
//! let csv = "\u{feff}\"OperationName\",\"EventText\"\n\
//!            \"QueryEnd\",\"EVALUATE ROW(\"\"x\"\", [Total])  [WaitTime: 0 ms]\"\n\
//!            \"VertiPaqSEQueryEnd\",\"SELECT ...\"\n";
//! let log = usage::parse_query_log(csv, "export.csv".as_ref()).unwrap().value;
//! assert_eq!(log.queries.len(), 1);
//! assert_eq!(log.queries[0].text, "EVALUATE ROW(\"x\", [Total])");
//! assert_eq!(log.queries[0].language, QueryLanguage::Dax);
//! ```

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::Path;

use serde_json::Value;

use crate::dax::{self, RawRef};
use crate::identity::{NameKey, ObjectId};
use crate::ingest::{Ingested, SkipKind, SkipNotice};
use crate::model::TabularDatabase;
use crate::model::index::{ModelIndex, Resolved};
use crate::{Error, Result};

/// The query language a logged query is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryLanguage {
    /// A DAX query — Power BI visuals, DAX Studio, `EVALUATE` scripts.
    Dax,
    /// An MDX query — Excel pivot tables and other OLAP clients.
    Mdx,
}

/// One report visual a query was issued for, from the event's
/// `ApplicationContext` JSON. Service GUIDs, not names: the log never says
/// what the report is called.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QuerySource {
    /// The Power BI report's item id, when the client recorded one.
    pub report_id: Option<String>,
    /// The visual's id inside that report.
    pub visual_id: Option<String>,
}

/// One captured `QueryEnd` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedQuery {
    /// The query text, with the engine's trailing `[WaitTime: … ms]` removed.
    pub text: String,
    /// The language, from `OperationDetailName` or sniffed from the text.
    pub language: QueryLanguage,
    /// The event timestamp as exported (`2026-09-30 11:11:05.6593686`, or ISO 8601).
    pub timestamp: Option<String>,
    /// Who ran the query (`ExecutingUser`, else `User`). Kept for distinct
    /// counts only; callers must not print it — it is a user principal name.
    pub user: Option<String>,
    /// The client application (`ApplicationName`), e.g. `PowerBI`.
    pub application: Option<String>,
    /// The semantic model the query ran against (`ItemName`, or Log
    /// Analytics' `ArtifactName`). One export can cover a whole workspace.
    pub item: Option<String>,
    /// The model's service item id (`ItemId`, or `ArtifactId`).
    pub item_id: Option<String>,
    /// The report visuals the query was issued for, when recorded.
    pub sources: Vec<QuerySource>,
}

/// A parsed query-log export.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryLog {
    /// Every usable `QueryEnd` query, in file order.
    pub queries: Vec<LoggedQuery>,
    /// Rows read in total, noise included.
    pub rows: usize,
    /// The earliest query timestamp, as exported.
    pub first_seen: Option<String>,
    /// The latest query timestamp, as exported.
    pub last_seen: Option<String>,
    /// The distinct semantic-model names the queries were logged against
    /// (`ItemName`, or Log Analytics' `ArtifactName`), sorted, one entry per
    /// case-insensitive name. Lets a caller warn when the log belongs to a
    /// different model, or pick one model out of a workspace-wide export.
    pub item_names: Vec<String>,
}

impl QueryLog {
    /// True when the log holds no usable query.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
    }

    /// True when `key` names one of the log's models: an `ItemName` or an
    /// `ItemId`, compared ASCII case-insensitively.
    #[must_use]
    pub fn names_item(&self, key: &str) -> bool {
        self.queries.iter().any(|query| query.is_for(key))
    }

    /// Keeps only the queries logged against the model `key` names (see
    /// [`QueryLog::names_item`]), plus those that name no model at all, and
    /// narrows `first_seen`/`last_seen` to them. `item_names` still lists
    /// every model the export covered. An empty `key` names no model, so only
    /// the unattributed queries stay. Returns how many queries were dropped.
    pub fn retain_item(&mut self, key: &str) -> usize {
        let before = self.queries.len();
        self.queries
            .retain(|query| query.item.is_none() && query.item_id.is_none() || query.is_for(key));
        self.first_seen = None;
        self.last_seen = None;
        for stamp in self
            .queries
            .iter()
            .filter_map(|query| query.timestamp.as_ref())
        {
            widen_window(&mut self.first_seen, &mut self.last_seen, stamp);
        }
        before - self.queries.len()
    }
}

impl LoggedQuery {
    fn is_for(&self, key: &str) -> bool {
        self.item
            .as_deref()
            .is_some_and(|item| item.eq_ignore_ascii_case(key))
            || self
                .item_id
                .as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case(key))
    }
}

fn widen_window(first: &mut Option<String>, last: &mut Option<String>, stamp: &str) {
    if first.as_deref().is_none_or(|current| stamp < current) {
        *first = Some(stamp.to_string());
    }
    if last.as_deref().is_none_or(|current| stamp > current) {
        *last = Some(stamp.to_string());
    }
}

/// Column-name aliases, folded to lowercase. Workspace monitoring
/// (`SemanticModelLogs`) and Log Analytics (`PowerBIDatasetsWorkspace`) name a
/// few of them differently.
const EVENT_TEXT: &[&str] = &["eventtext"];
const OPERATION: &[&str] = &["operationname"];
const OPERATION_DETAIL: &[&str] = &["operationdetailname"];
const TIMESTAMP: &[&str] = &["timestamp", "timegenerated"];
const ITEM_NAME: &[&str] = &["itemname", "artifactname"];
const ITEM_ID: &[&str] = &["itemid", "artifactid"];
const APPLICATION: &[&str] = &["applicationname"];
const APPLICATION_CONTEXT: &[&str] = &["applicationcontext"];
const USER: &[&str] = &["executinguser", "user"];

/// Reads a query-log export: the CSV an Eventhouse / KQL queryset exports, a
/// JSON array of row objects, a Kusto REST response (v1 `Tables`, or the v2
/// frame array), or a Log Analytics query API response (lowercase `tables`).
/// Any of them may be gzip-compressed or the single file in a zip archive.
/// The format is detected from content, not the extension.
pub fn read_query_log(path: &Path) -> Result<Ingested<QueryLog>> {
    let bytes = decompress(std::fs::read(path)?, path)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::UnsupportedFormat(format!("{} is not UTF-8", path.display())))?;
    parse_query_log(&text, path)
}

/// Unwraps a gzip stream or a single-file zip archive, sniffed by magic bytes;
/// anything else passes through untouched.
fn decompress(bytes: Vec<u8>, path: &Path) -> Result<Vec<u8>> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(bytes.as_slice()).read_to_end(&mut out)?;
        return Ok(out);
    }
    if !bytes.starts_with(b"PK\x03\x04") {
        return Ok(bytes);
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let files: Vec<String> = archive
        .file_names()
        .filter(|name| {
            let base = name.rsplit('/').next().unwrap_or(name);
            !name.ends_with('/') && !name.starts_with("__MACOSX/") && !base.starts_with('.')
        })
        .map(str::to_string)
        .collect();
    let [name] = files.as_slice() else {
        return Err(Error::UnsupportedFormat(format!(
            "{} is a zip archive; expected exactly one query-log file inside, found {}",
            path.display(),
            if files.is_empty() {
                "none".to_string()
            } else {
                files.join(", ")
            }
        )));
    };
    let mut out = Vec::new();
    archive.by_name(name)?.read_to_end(&mut out)?;
    Ok(out)
}

/// Parses query-log text; `path` is only used to label errors and notices.
/// See [`read_query_log`].
pub fn parse_query_log(text: &str, path: &Path) -> Result<Ingested<QueryLog>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let table = match text.trim_start().chars().next() {
        Some('[' | '{') => json_table(text, path)?,
        _ => csv_table(text, path),
    };
    table.into_log(path)
}

/// A header plus rows, whatever the export format was.
struct Table {
    header: Vec<String>,
    rows: Vec<(usize, Vec<Option<String>>)>,
    skips: Vec<SkipNotice>,
}

impl Table {
    fn column(&self, aliases: &[&str]) -> Option<usize> {
        aliases.iter().find_map(|alias| {
            self.header
                .iter()
                .position(|name| name.trim().eq_ignore_ascii_case(alias))
        })
    }

    fn into_log(self, path: &Path) -> Result<Ingested<QueryLog>> {
        let Some(event_text) = self.column(EVENT_TEXT) else {
            return Err(Error::UnsupportedFormat(format!(
                "{} is not a query log: it has no EventText column",
                path.display()
            )));
        };
        let operation = self.column(OPERATION);
        let detail = self.column(OPERATION_DETAIL);
        let timestamp = self.column(TIMESTAMP);
        let item_name = self.column(ITEM_NAME);
        let item_id = self.column(ITEM_ID);
        let application = self.column(APPLICATION);
        let context = self.column(APPLICATION_CONTEXT);
        let user = self.column(USER);

        let mut log = QueryLog {
            rows: self.rows.len(),
            ..QueryLog::default()
        };
        let mut skips = self.skips;
        let mut items = BTreeSet::new();
        for (row_number, row) in &self.rows {
            let cell = |index: Option<usize>| -> Option<&str> {
                let value = row.get(index?)?.as_deref()?.trim();
                (!value.is_empty()).then_some(value)
            };
            // Engine noise is expected in an unfiltered export: skip silently.
            if operation.is_some()
                && !cell(operation).is_some_and(|op| op.eq_ignore_ascii_case("QueryEnd"))
            {
                continue;
            }
            let Some(raw_text) = cell(Some(event_text)) else {
                skips.push(notice(path, *row_number, "a QueryEnd row has no EventText"));
                continue;
            };
            let text = strip_wait_time(raw_text);
            if text.is_empty() {
                skips.push(notice(path, *row_number, "a QueryEnd row has no EventText"));
                continue;
            }
            let language = match cell(detail) {
                Some(d) if d.eq_ignore_ascii_case("MDXQuery") => QueryLanguage::Mdx,
                Some(d) if d.eq_ignore_ascii_case("DAXQuery") => QueryLanguage::Dax,
                _ => sniff_language(text),
            };
            let stamp = cell(timestamp).map(str::to_string);
            if let Some(stamp) = &stamp {
                widen_window(&mut log.first_seen, &mut log.last_seen, stamp);
            }
            if let Some(item) = cell(item_name) {
                items.insert(item.to_string());
            }
            log.queries.push(LoggedQuery {
                text: text.to_string(),
                language,
                timestamp: stamp,
                user: cell(user).map(str::to_string),
                application: cell(application).map(str::to_string),
                item: cell(item_name).map(str::to_string),
                item_id: cell(item_id).map(str::to_string),
                sources: cell(context).map(sources_of).unwrap_or_default(),
            });
        }
        // One model per name however its rows spell it: the service treats
        // item names case-insensitively.
        let mut seen = BTreeSet::new();
        log.item_names = items
            .into_iter()
            .filter(|item: &String| seen.insert(item.to_lowercase()))
            .collect();
        Ok(Ingested { value: log, skips })
    }
}

fn notice(path: &Path, row: usize, detail: &str) -> SkipNotice {
    SkipNotice {
        path: path.to_path_buf(),
        location: Some(format!("row {row}")),
        kind: SkipKind::MalformedValue,
        detail: detail.to_string(),
    }
}

/// Removes the ` [WaitTime: 0 ms]` suffix the engine appends to a query's
/// `EventText`. Left in, it lexes as an unqualified `[WaitTime: 0 ms]` reference.
fn strip_wait_time(text: &str) -> &str {
    let trimmed = text.trim_end();
    if let Some(start) = trimmed.rfind("[WaitTime:")
        && trimmed.ends_with("ms]")
    {
        return trimmed[..start].trim_end();
    }
    trimmed
}

/// Guesses the language from the query's first keyword: MDX statements open
/// with `SELECT`, `WITH`, or `DRILLTHROUGH`; everything else is taken as DAX.
fn sniff_language(text: &str) -> QueryLanguage {
    let head = text.trim_start();
    let word: String = head
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_uppercase();
    match word.as_str() {
        "SELECT" | "WITH" | "DRILLTHROUGH" => QueryLanguage::Mdx,
        _ => QueryLanguage::Dax,
    }
}

/// The report visuals named by an `ApplicationContext` JSON value. Malformed
/// or differently shaped context is ignored — it is attribution, not liveness.
fn sources_of(context: &str) -> Vec<QuerySource> {
    let Ok(value) = serde_json::from_str::<Value>(context) else {
        return Vec::new();
    };
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    value
        .get("Sources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|source| QuerySource {
            report_id: text(source, "ReportId"),
            visual_id: text(source, "VisualId"),
        })
        .filter(|source| source.report_id.is_some() || source.visual_id.is_some())
        .collect()
}

/// Parses RFC 4180 CSV: quoted fields may hold commas, `""` escapes, and line
/// breaks. An unterminated quote at end of input is a skip notice; the rows
/// before it stand.
fn csv_table(text: &str, path: &Path) -> Table {
    let mut records: Vec<(usize, Vec<Option<String>>)> = Vec::new();
    let mut skips = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut line = 1;
    let mut record_line = 1;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                _ => {
                    if c == '\n' {
                        line += 1;
                    }
                    field.push(c);
                }
            }
            continue;
        }
        match c {
            '"' => in_quotes = true,
            ',' => record.push(Some(std::mem::take(&mut field))),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                record.push(Some(std::mem::take(&mut field)));
                if !(record.len() == 1 && record[0].as_deref() == Some("")) {
                    records.push((record_line, std::mem::take(&mut record)));
                }
                record.clear();
                line += 1;
                record_line = line;
            }
            _ => field.push(c),
        }
    }
    if in_quotes {
        skips.push(notice(
            path,
            record_line,
            "the file ends inside a quoted field",
        ));
    } else if !field.is_empty() || !record.is_empty() {
        record.push(Some(field));
        records.push((record_line, record));
    }
    let mut records = records.into_iter();
    let header = records
        .next()
        .map(|(_, header)| header.into_iter().map(Option::unwrap_or_default).collect())
        .unwrap_or_default();
    Table {
        header,
        rows: records.collect(),
        skips,
    }
}

/// Reads the JSON shapes a query export can take: an array of row objects
/// (`[{"EventText": …}, …]`), a Kusto v1 response (`{"Tables": [{"Columns",
/// "Rows"}]}`), or a Kusto v2 frame array (the `PrimaryResult` `DataTable`).
fn json_table(text: &str, path: &Path) -> Result<Table> {
    let value: Value = serde_json::from_str(text)?;
    let unsupported = || {
        Error::UnsupportedFormat(format!(
            "{} is JSON, but not a query export this tool reads",
            path.display()
        ))
    };
    // Kusto v2: an array of frames; the first PrimaryResult DataTable holds the rows.
    if let Some(frames) = value.as_array()
        && frames.iter().any(|frame| frame.get("FrameType").is_some())
    {
        let table = frames
            .iter()
            .find(|frame| {
                frame.get("FrameType").and_then(Value::as_str) == Some("DataTable")
                    && frame.get("TableKind").and_then(Value::as_str) == Some("PrimaryResult")
            })
            .ok_or_else(unsupported)?;
        return columns_and_rows(table).ok_or_else(unsupported);
    }
    // Kusto v1: `Tables[0]` is the primary result.
    if let Some(table) = value
        .get("Tables")
        .or_else(|| value.get("tables"))
        .and_then(Value::as_array)
        .and_then(|tables| tables.first())
    {
        return columns_and_rows(table).ok_or_else(unsupported);
    }
    // An array of row objects; the header is the union of their keys.
    let rows = value.as_array().ok_or_else(unsupported)?;
    let mut header: Vec<String> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for row in rows.iter().filter_map(Value::as_object) {
        for key in row.keys() {
            positions.entry(key.clone()).or_insert_with(|| {
                header.push(key.clone());
                header.len() - 1
            });
        }
    }
    let rows = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let mut cells = vec![None; header.len()];
            if let Some(row) = row.as_object() {
                for (key, value) in row {
                    cells[positions[key]] = cell_text(value);
                }
            }
            (index + 1, cells)
        })
        .collect();
    Ok(Table {
        header,
        rows,
        skips: Vec::new(),
    })
}

/// A Kusto result table: `Columns: [{ColumnName}]`, `Rows: [[…]]`, or the Log
/// Analytics spelling `columns: [{name}]`, `rows: [[…]]`.
fn columns_and_rows(table: &Value) -> Option<Table> {
    let header = table
        .get("Columns")
        .or_else(|| table.get("columns"))?
        .as_array()?
        .iter()
        .map(|column| {
            column
                .get("ColumnName")
                .or_else(|| column.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let rows = table
        .get("Rows")
        .or_else(|| table.get("rows"))?
        .as_array()?
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let cells = row
                .as_array()
                .map(|cells| cells.iter().map(cell_text).collect())
                .unwrap_or_default();
            (index + 1, cells)
        })
        .collect();
    Some(Table {
        header,
        rows,
        skips: Vec::new(),
    })
}

/// A JSON cell as text: strings verbatim, other scalars and dynamic values
/// (`ApplicationContext` arrives as an object from Kusto) re-serialized.
fn cell_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// One model object the logged queries reference directly, with how often and
/// by whom — the query-log counterpart of a report binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueriedObject {
    /// The referenced object.
    pub id: ObjectId,
    /// How many logged queries reference it.
    pub count: usize,
    /// The latest timestamp among those queries, as exported.
    pub last_seen: Option<String>,
    /// How many distinct users ran those queries.
    pub users: usize,
    /// The distinct client applications, sorted.
    pub applications: Vec<String>,
    /// The distinct report ids the queries were issued for, sorted.
    pub reports: Vec<String>,
}

/// Every model object `query` references directly. Query-local names resolve
/// to nothing; an ambiguous reference yields every candidate.
#[must_use]
pub fn query_references(
    db: &TabularDatabase,
    index: &ModelIndex,
    query: &LoggedQuery,
) -> Vec<ObjectId> {
    let mut targets = match query.language {
        QueryLanguage::Dax => dax_references(db, index, &query.text),
        QueryLanguage::Mdx => mdx_references(db, index, &query.text),
    };
    targets.sort();
    targets.dedup();
    targets
}

fn dax_references(db: &TabularDatabase, index: &ModelIndex, text: &str) -> Vec<ObjectId> {
    let mut targets = Vec::new();
    for raw in dax::references(text) {
        // A qualified name can also address a hierarchy: `ISINSCOPE('Date'[Calendar])`.
        if let RawRef::Field {
            table: Some(table),
            name,
            ..
        } = raw
            && let Some(id) = hierarchy_id(db, index, &dax::unescape_name(table), name)
        {
            targets.push(id);
        }
        targets.extend(dax::bind(db, index, None, raw).targets().iter().cloned());
    }
    targets
}

/// Every model object an MDX query names. In the MDX view of a tabular model a
/// table is a dimension, its columns are attribute hierarchies, and user
/// hierarchies' levels are named after columns, so a bracketed chain
/// `[Table].[Column-or-Hierarchy].[Level]…` names the table and each segment
/// that matches one of its columns or hierarchies; `[Measures].[Name]` names a
/// measure. Member keys (`&[2024]`) are values and are skipped.
fn mdx_references(db: &TabularDatabase, index: &ModelIndex, text: &str) -> Vec<ObjectId> {
    let mut targets = Vec::new();
    for chain in mdx_chains(text) {
        let mut names = chain
            .iter()
            .filter(|segment| !segment.key)
            .map(|segment| unescape_bracket(segment.name));
        let Some(first) = names.next() else { continue };
        if first.eq_ignore_ascii_case("Measures") {
            if let Some(name) = names.next()
                && let Some(id) = index
                    .resolve_unqualified(&name, None)
                    .measure
                    .and_then(|h| db.object_id(Resolved::Measure(h)))
            {
                targets.push(id);
            }
            continue;
        }
        let Some(table) = index.resolve_table(&first).and_then(|h| db.table(h)) else {
            continue;
        };
        targets.push(ObjectId::Table {
            table: NameKey::new(table.name.as_str()),
        });
        for name in names {
            if let Some(handle) = index.resolve_column(&table.name, &name)
                && let Some(id) = db.object_id(Resolved::Column(handle))
            {
                targets.push(id);
            }
            if let Some(id) = hierarchy_id(db, index, &table.name, &name) {
                targets.push(id);
            }
        }
    }
    targets
}

fn hierarchy_id(
    db: &TabularDatabase,
    index: &ModelIndex,
    table: &str,
    name: &str,
) -> Option<ObjectId> {
    let handle = index.resolve_hierarchy(table, name)?;
    let table = db.tables.get(handle.table)?;
    let hierarchy = table.hierarchies.get(handle.hierarchy)?;
    Some(ObjectId::Hierarchy {
        table: NameKey::new(table.name.as_str()),
        hierarchy: NameKey::new(hierarchy.name.as_str()),
    })
}

/// One bracketed MDX name segment, as written (`]]` escapes intact).
#[derive(Debug, PartialEq, Eq)]
struct Segment<'a> {
    name: &'a str,
    /// A member key (`&[…]`): a value, not an object name.
    key: bool,
}

fn unescape_bracket(name: &str) -> Cow<'_, str> {
    if name.contains("]]") {
        Cow::Owned(name.replace("]]", "]"))
    } else {
        Cow::Borrowed(name)
    }
}

/// Splits MDX text into dotted chains of bracketed segments, skipping string
/// literals and comments. Unbracketed parts (`.Members`, `.Children`) end
/// nothing and name nothing; only bracketed segments are kept.
fn mdx_chains(text: &str) -> Vec<Vec<Segment<'_>>> {
    let bytes = text.as_bytes();
    let mut chains = Vec::new();
    let mut chain: Vec<Segment<'_>> = Vec::new();
    let mut i = 0;
    // Whether the previous significant character continues a chain (a `.`).
    let mut dotted = false;
    while i < bytes.len() {
        match bytes[i] {
            b'[' | b'&' if bytes[i] == b'[' || bytes.get(i + 1) == Some(&b'[') => {
                let key = bytes[i] == b'&';
                let start = i + if key { 2 } else { 1 };
                let mut end = start;
                loop {
                    match bytes.get(end) {
                        None => break,
                        Some(b']') if bytes.get(end + 1) == Some(&b']') => end += 2,
                        Some(b']') => break,
                        Some(_) => end += 1,
                    }
                }
                if !dotted && !chain.is_empty() {
                    chains.push(std::mem::take(&mut chain));
                }
                chain.push(Segment {
                    name: &text[start..end.min(bytes.len())],
                    key,
                });
                dotted = false;
                i = end + 1;
            }
            b'.' => {
                dotted = true;
                i += 1;
            }
            b'\'' | b'"' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        if bytes.get(i + 1) == Some(&quote) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
                dotted = false;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = line_end(bytes, i);
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = line_end(bytes, i);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |end| i + 2 + end + 2);
            }
            c if c.is_ascii_whitespace() => i += 1,
            c if c.is_ascii_alphanumeric() || c == b'_' => {
                // An unbracketed identifier: `Measures.[X]` starts a chain, and
                // `.Members` continues one without naming anything.
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                if !dotted && !chain.is_empty() {
                    chains.push(std::mem::take(&mut chain));
                }
                if text[start..i].eq_ignore_ascii_case("Measures") {
                    chain.push(Segment {
                        name: &text[start..i],
                        key: false,
                    });
                }
                dotted = false;
            }
            _ => {
                if !chain.is_empty() {
                    chains.push(std::mem::take(&mut chain));
                }
                dotted = false;
                i += 1;
            }
        }
    }
    if !chain.is_empty() {
        chains.push(chain);
    }
    chains
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |p| from + p)
}

/// Aggregates a query log into the objects its queries reference directly,
/// in identity order. Reachability does the rest: whatever a queried measure
/// uses stays live through ordinary edges.
#[must_use]
pub fn queried_objects(db: &TabularDatabase, log: &QueryLog) -> Vec<QueriedObject> {
    #[derive(Default)]
    struct Tally<'a> {
        count: usize,
        last_seen: Option<&'a str>,
        users: BTreeSet<&'a str>,
        applications: BTreeSet<&'a str>,
        reports: BTreeSet<&'a str>,
    }
    if log.queries.is_empty() {
        return Vec::new();
    }
    let index = ModelIndex::build(db);
    let mut tallies: BTreeMap<ObjectId, Tally<'_>> = BTreeMap::new();
    for query in &log.queries {
        for id in query_references(db, &index, query) {
            let tally = tallies.entry(id).or_default();
            tally.count += 1;
            if let Some(stamp) = query.timestamp.as_deref()
                && tally.last_seen.is_none_or(|last| stamp > last)
            {
                tally.last_seen = Some(stamp);
            }
            tally.users.extend(query.user.as_deref());
            tally.applications.extend(query.application.as_deref());
            tally
                .reports
                .extend(query.sources.iter().filter_map(|s| s.report_id.as_deref()));
        }
    }
    tallies
        .into_iter()
        .map(|(id, tally)| QueriedObject {
            id,
            count: tally.count,
            last_seen: tally.last_seen.map(str::to_string),
            users: tally.users.len(),
            applications: tally.applications.into_iter().map(str::to_string).collect(),
            reports: tally.reports.into_iter().map(str::to_string).collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, Hierarchy, Measure, Table};

    fn model() -> TabularDatabase {
        let column = |name: &str| Column {
            name: name.to_string(),
            ..Default::default()
        };
        TabularDatabase {
            tables: vec![
                Table {
                    name: "Sales".to_string(),
                    columns: vec![column("Amount")],
                    measures: vec![Measure {
                        name: "Total".to_string(),
                        expression: "SUM(Sales[Amount])".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                Table {
                    name: "Date".to_string(),
                    columns: vec![column("Year Offset"), column("Day"), column("Year")],
                    hierarchies: vec![Hierarchy {
                        name: "Calendar".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn query(text: &str, language: QueryLanguage) -> LoggedQuery {
        LoggedQuery {
            text: text.to_string(),
            language,
            timestamp: None,
            user: None,
            application: None,
            item: None,
            item_id: None,
            sources: Vec::new(),
        }
    }

    fn ids(text: &str, language: QueryLanguage) -> Vec<String> {
        let db = model();
        let index = ModelIndex::build(&db);
        query_references(&db, &index, &query(text, language))
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn dax_query_names_columns_measures_and_tables() {
        let found = ids(
            "DEFINE VAR __DS0FilterTable = FILTER(KEEPFILTERS(VALUES('Date'[Year Offset])), \
             'Date'[Year Offset] < 1) EVALUATE SUMMARIZECOLUMNS('Date'[Day], \
             __DS0FilterTable, \"x\", [Total], \"n\", COUNTROWS(Sales))",
            QueryLanguage::Dax,
        );
        let mut found = found;
        found.sort();
        assert_eq!(
            found,
            [
                "'Date'[Day]",
                "'Date'[Year Offset]",
                "'Sales'[Total]",
                "table 'Sales'",
            ]
        );
    }

    #[test]
    fn query_local_definitions_resolve_to_nothing() {
        let found = ids(
            "DEFINE MEASURE Sales[Local] = 1 VAR __DS0Core = ROW(\"a\", [Local]) \
             EVALUATE __DS0Core",
            QueryLanguage::Dax,
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn dax_hierarchy_reference_is_found() {
        let found = ids(
            "EVALUATE ROW(\"x\", ISINSCOPE('Date'[Calendar]))",
            QueryLanguage::Dax,
        );
        assert_eq!(found, ["hierarchy 'Date'[Calendar]"]);
    }

    #[test]
    fn mdx_chains_name_measures_columns_and_hierarchies() {
        let mut found = ids(
            "SELECT NON EMPTY {[Measures].[Total]} ON COLUMNS, \
             NON EMPTY [Date].[Year].[Year].Members ON ROWS \
             FROM [Model] WHERE ([Date].[Calendar].&[2024]) -- [Sales].[Amount]",
            QueryLanguage::Mdx,
        );
        found.sort();
        assert_eq!(
            found,
            [
                "'Date'[Year]",
                "'Sales'[Total]",
                "hierarchy 'Date'[Calendar]",
                "table 'Date'",
            ]
        );
    }

    #[test]
    fn mdx_member_keys_and_strings_name_nothing() {
        let found = ids(
            "WITH MEMBER [Measures].[X] AS '[Sales].[Amount]' SELECT &[Sales] ON 0 FROM [Model]",
            QueryLanguage::Mdx,
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn unknown_names_are_dropped() {
        assert!(ids("EVALUATE ROW(\"x\", [Nope])", QueryLanguage::Dax).is_empty());
    }

    #[test]
    fn wait_time_suffix_is_stripped() {
        assert_eq!(
            strip_wait_time("EVALUATE Sales    [WaitTime: 0 ms]"),
            "EVALUATE Sales"
        );
        assert_eq!(strip_wait_time("EVALUATE Sales"), "EVALUATE Sales");
    }

    #[test]
    fn language_is_sniffed_when_the_detail_is_missing() {
        assert_eq!(
            sniff_language("  SELECT {} ON 0 FROM [M]"),
            QueryLanguage::Mdx
        );
        assert_eq!(sniff_language("with member x"), QueryLanguage::Mdx);
        assert_eq!(
            sniff_language("DEFINE VAR x = 1 EVALUATE {x}"),
            QueryLanguage::Dax
        );
    }

    #[test]
    fn csv_handles_quotes_escapes_and_line_breaks() {
        let text = "a,b\r\n\"x, y\",\"he said \"\"hi\"\"\nnext line\"\n1,2";
        let table = csv_table(text, Path::new("t.csv"));
        assert_eq!(table.header, ["a", "b"]);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].1[0].as_deref(), Some("x, y"));
        assert_eq!(
            table.rows[0].1[1].as_deref(),
            Some("he said \"hi\"\nnext line")
        );
        assert_eq!(table.rows[1].1[1].as_deref(), Some("2"));
    }

    #[test]
    fn unterminated_quote_is_a_notice() {
        let log = parse_query_log(
            "OperationName,EventText\nQueryEnd,EVALUATE Sales\nQueryEnd,\"EVALUATE",
            Path::new("t.csv"),
        )
        .unwrap();
        assert_eq!(log.value.queries.len(), 1);
        assert_eq!(log.skips.len(), 1);
    }

    #[test]
    fn missing_event_text_column_is_an_error() {
        let error = parse_query_log("Timestamp,Other\n1,2\n", Path::new("t.csv")).unwrap_err();
        assert!(error.to_string().contains("EventText"), "{error}");
    }

    #[test]
    fn noise_rows_are_skipped_and_empty_queries_noted() {
        let text = "OperationName,EventText,ItemName,Timestamp\n\
                    VertiPaqSEQueryEnd,SELECT x,Sales,2026-09-01\n\
                    QueryEnd,EVALUATE Sales,Sales,2026-09-03\n\
                    QueryEnd,,Sales,2026-09-02\n\
                    QueryEnd,EVALUATE Sales,Sales,2026-09-02\n";
        let log = parse_query_log(text, Path::new("t.csv")).unwrap();
        assert_eq!(log.value.rows, 4);
        assert_eq!(log.value.queries.len(), 2);
        assert_eq!(log.skips.len(), 1);
        assert_eq!(log.value.first_seen.as_deref(), Some("2026-09-02"));
        assert_eq!(log.value.last_seen.as_deref(), Some("2026-09-03"));
        assert_eq!(log.value.item_names, ["Sales"]);
    }

    #[test]
    fn kusto_v1_and_v2_and_row_objects_parse() {
        let v1 = r#"{"Tables":[{"TableName":"Table_0","Columns":[{"ColumnName":"OperationName"},
            {"ColumnName":"EventText"},{"ColumnName":"ApplicationContext"}],
            "Rows":[["QueryEnd","EVALUATE Sales",{"Sources":[{"ReportId":"r1","VisualId":"v1"}]}]]}]}"#;
        let log = parse_query_log(v1, Path::new("t.json")).unwrap().value;
        assert_eq!(log.queries.len(), 1);
        assert_eq!(
            log.queries[0].sources,
            [QuerySource {
                report_id: Some("r1".to_string()),
                visual_id: Some("v1".to_string())
            }]
        );

        let v2 = r#"[{"FrameType":"DataSetHeader"},{"FrameType":"DataTable","TableKind":"QueryProperties","Columns":[],"Rows":[]},
            {"FrameType":"DataTable","TableKind":"PrimaryResult","Columns":[{"ColumnName":"EventText"}],"Rows":[["EVALUATE Sales"]]}]"#;
        assert_eq!(
            parse_query_log(v2, Path::new("t.json"))
                .unwrap()
                .value
                .queries
                .len(),
            1
        );

        let rows = r#"[{"EventText":"EVALUATE Sales","OperationName":"QueryEnd"},{"EventText":"x","OperationName":"DiscoverEnd"}]"#;
        assert_eq!(
            parse_query_log(rows, Path::new("t.json"))
                .unwrap()
                .value
                .queries
                .len(),
            1
        );
    }

    const TWO_MODELS: &str = "OperationName,ItemName,ItemId,Timestamp,EventText\n\
        QueryEnd,Sales,11111111-aaaa,2026-09-01,EVALUATE Sales\n\
        QueryEnd,Finance,22222222-bbbb,2026-09-05,EVALUATE Ledger\n\
        QueryEnd,sales,11111111-aaaa,2026-09-03,EVALUATE Sales\n\
        QueryEnd,,,2026-09-09,EVALUATE Unknown\n";

    #[test]
    fn retain_item_keeps_one_models_queries_and_its_window() {
        let mut log = parse_query_log(TWO_MODELS, Path::new("t.csv"))
            .unwrap()
            .value;
        assert_eq!(log.item_names, ["Finance", "Sales"]);
        assert!(log.names_item("SALES"));
        assert!(!log.names_item("Budget"));
        assert_eq!(log.retain_item("Sales"), 1);
        let texts: Vec<&str> = log.queries.iter().map(|q| q.text.as_str()).collect();
        assert_eq!(
            texts,
            ["EVALUATE Sales", "EVALUATE Sales", "EVALUATE Unknown"]
        );
        assert_eq!(log.first_seen.as_deref(), Some("2026-09-01"));
        assert_eq!(log.last_seen.as_deref(), Some("2026-09-09"));
        assert_eq!(
            log.item_names.len(),
            2,
            "item_names still lists the whole export"
        );
    }

    #[test]
    fn retain_item_matches_the_item_id() {
        let mut log = parse_query_log(TWO_MODELS, Path::new("t.csv"))
            .unwrap()
            .value;
        assert!(log.names_item("22222222-BBBB"));
        assert_eq!(log.retain_item("22222222-bbbb"), 2);
        assert_eq!(log.queries[0].text, "EVALUATE Ledger");
        assert_eq!(log.queries.len(), 2);
        assert_eq!(
            log.retain_item(""),
            1,
            "an empty key keeps only unattributed rows"
        );
        assert_eq!(log.queries[0].text, "EVALUATE Unknown");
    }

    #[test]
    fn log_analytics_rest_response_parses() {
        let text = r#"{"tables":[{"name":"PrimaryResult","columns":[
            {"name":"TimeGenerated","type":"datetime"},{"name":"OperationName","type":"string"},
            {"name":"ArtifactName","type":"string"},{"name":"EventText","type":"string"}],
            "rows":[["2026-09-02T10:00:00Z","QueryEnd","Sales","EVALUATE Sales"],
                    ["2026-09-02T10:00:01Z","DiscoverEnd","Sales","<Discover/>"]]}]}"#;
        let log = parse_query_log(text, Path::new("t.json")).unwrap().value;
        assert_eq!(log.queries.len(), 1);
        assert_eq!(log.queries[0].text, "EVALUATE Sales");
        assert_eq!(log.first_seen.as_deref(), Some("2026-09-02T10:00:00Z"));
        assert_eq!(log.item_names, ["Sales"]);
    }

    const COMPRESSED_CSV: &str = "OperationName,EventText\nQueryEnd,EVALUATE Sales\n";

    fn read_bytes(bytes: &[u8]) -> Result<Ingested<QueryLog>> {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, bytes).unwrap();
        read_query_log(file.path())
    }

    fn zipped(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in files {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut writer, content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn gzip_export_is_decompressed() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, COMPRESSED_CSV.as_bytes()).unwrap();
        let log = read_bytes(&encoder.finish().unwrap()).unwrap().value;
        assert_eq!(log.queries.len(), 1);
    }

    #[test]
    fn zip_export_reads_its_single_file() {
        let bytes = zipped(&[
            ("export.csv", COMPRESSED_CSV),
            ("__MACOSX/._export.csv", "junk"),
        ]);
        assert_eq!(read_bytes(&bytes).unwrap().value.queries.len(), 1);
    }

    #[test]
    fn zip_with_several_files_is_rejected() {
        let bytes = zipped(&[("a.csv", COMPRESSED_CSV), ("b.csv", COMPRESSED_CSV)]);
        let err = read_bytes(&bytes).unwrap_err();
        assert!(
            matches!(&err, Error::UnsupportedFormat(message) if message.contains("a.csv, b.csv")),
            "{err}"
        );
    }

    #[test]
    fn queried_objects_tally_counts_users_and_reports() {
        let db = model();
        let mut first = query("EVALUATE ROW(\"x\", [Total])", QueryLanguage::Dax);
        first.user = Some("a@example.com".to_string());
        first.timestamp = Some("2026-09-01".to_string());
        first.sources = vec![QuerySource {
            report_id: Some("r1".to_string()),
            visual_id: None,
        }];
        let mut second = first.clone();
        second.user = Some("b@example.com".to_string());
        second.timestamp = Some("2026-09-05".to_string());
        second.application = Some("PowerBI".to_string());
        let log = QueryLog {
            queries: vec![first, second],
            ..QueryLog::default()
        };
        let queried = queried_objects(&db, &log);
        assert_eq!(queried.len(), 1);
        assert_eq!(queried[0].id.to_string(), "'Sales'[Total]");
        assert_eq!(queried[0].count, 2);
        assert_eq!(queried[0].users, 2);
        assert_eq!(queried[0].last_seen.as_deref(), Some("2026-09-05"));
        assert_eq!(queried[0].applications, ["PowerBI"]);
        assert_eq!(queried[0].reports, ["r1"]);
    }

    #[test]
    fn queried_objects_are_graph_roots() {
        use crate::graph::DependencyGraph;

        let db = model();
        let unused = |graph: &DependencyGraph| -> Vec<String> {
            graph
                .unused_objects()
                .iter()
                .map(|unused| unused.id.to_string())
                .collect()
        };
        let plain = DependencyGraph::build(&db, &[]);
        assert!(unused(&plain).contains(&"'Sales'[Total]".to_string()));
        assert!(unused(&plain).contains(&"'Sales'[Amount]".to_string()));
        assert!(plain.queried().is_empty());

        let log = QueryLog {
            queries: vec![query("EVALUATE ROW(\"x\", [Total])", QueryLanguage::Dax)],
            ..QueryLog::default()
        };
        let graph = DependencyGraph::build_with_queries(&db, &[], &log);
        let dead = unused(&graph);
        // The queried measure is a root, and what it uses stays live with it.
        assert!(!dead.contains(&"'Sales'[Total]".to_string()), "{dead:?}");
        assert!(!dead.contains(&"'Sales'[Amount]".to_string()), "{dead:?}");
        assert!(dead.contains(&"'Date'[Day]".to_string()), "{dead:?}");
        let total = graph
            .queried_by(&ObjectId::Measure {
                table: NameKey::new("Sales"),
                measure: NameKey::new("Total"),
            })
            .expect("queried");
        assert_eq!(total.count, 1);
        assert!(
            graph
                .queried_by(&ObjectId::Table {
                    table: NameKey::new("Sales")
                })
                .is_none()
        );
    }
}
