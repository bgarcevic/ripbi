//! Excel workbooks as report consumers (issue #172).
//!
//! A workbook connected to a semantic model over Analyze in Excel (or any
//! MSOLAP connection) uses model objects from five places, and each becomes a
//! binding in the same [`ReportModel`] a PBIR report normalizes into:
//!
//! - **PivotTables** (`xl/pivotTables/`): the fields placed on rows, columns,
//!   filters, and values, by index into their cache's `cacheFields`. The
//!   cache's `cacheHierarchies` list every hierarchy of the model and are
//!   deliberately never read as roots. An implicit measure ("Sum of Amount")
//!   is an [`Aggregation`](FieldTarget::Aggregation) over the column its
//!   `xxpim:dataFieldFutureData@sourceField` names. A value filter (Top 10
//!   by a measure) binds its measure.
//! - **Calculated members and sets** of a pivot's cache: the names their MDX
//!   formulas scan to. The calculated names themselves are workbook-local and
//!   never bind.
//! - **Slicers and timelines** (`xl/slicerCaches/`, `xl/timelineCaches/`):
//!   the hierarchy and levels each filters on.
//! - **CUBE formulas** (`<f>` cells): the string-literal member and set
//!   expressions of `CUBEVALUE`, `CUBEMEMBER`, `CUBESET`,
//!   `CUBERANKEDMEMBER`, `CUBEMEMBERPROPERTY`, and `CUBEKPIMEMBER`. An
//!   argument that is a reference to another CUBE cell, a nested CUBE call,
//!   or a slicer's name is covered where it is defined; any other computed
//!   argument is a [`SkipKind::NotAnalyzed`] notice, never a guess.
//! - **DAX query tables**: an OLAP connection whose command is a query, not
//!   a cube name, becomes a [`ReportQuery`].
//!
//! Sheets become pages; pivots, slicers, timelines, and CUBE cells become
//! visuals on the sheet that holds them. Every MDX name is a
//! [`FieldTarget::Mdx`], resolved by the graph against the model, so a stale
//! pivot field surfaces as a broken binding.
//!
//! Only connections that query an OLAP model count. Every other connection
//! (Power Query, ODBC, a table) is a notice, and so is the workbook's own
//! Data Model (`$Embedded$`), which is a model, not a consumer of this one.
//! Parts are matched by local name, so ISO/IEC 29500 Strict workbooks read
//! like transitional ones; a malformed part is a notice, not an error.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use zip::ZipArchive;

use super::{Ingested, SkipKind, SkipNotice};
use crate::identity::NameKey;
use crate::report::{
    FieldTarget, FieldWell, Filter, Page, Projection, ReportModel, ReportQuery, Visual,
};
use crate::usage::{self, QueryLanguage};
use crate::{Error, Result};

/// Extensions of the Open XML workbooks this module reads.
const EXTENSIONS: [&str; 4] = ["xlsx", "xlsm", "xltx", "xltm"];

/// Extensions of binary workbooks, which are refused.
const BINARY_EXTENSIONS: [&str; 3] = ["xls", "xlsb", "xlt"];

/// The CUBE functions that take a connection and member expressions.
/// `CUBESETCOUNT` takes only a set cell and binds nothing of its own.
const CUBE_FUNCTIONS: [&str; 6] = [
    "CUBEVALUE",
    "CUBEMEMBER",
    "CUBESET",
    "CUBERANKEDMEMBER",
    "CUBEMEMBERPROPERTY",
    "CUBEKPIMEMBER",
];

/// Whether `path` names an Open XML workbook by its extension.
pub(super) fn is_workbook(path: &Path) -> bool {
    has_extension(path, &EXTENSIONS)
}

/// Whether `path` names a binary workbook (`.xls`, `.xlsb`), which is refused.
pub(super) fn is_binary_workbook(path: &Path) -> bool {
    has_extension(path, &BINARY_EXTENSIONS)
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Reads a workbook into a [`ReportModel`] whose pages are its sheets.
pub(super) fn load(path: &Path) -> Result<Ingested<ReportModel>> {
    if is_binary_workbook(path) {
        return Err(Error::UnsupportedFormat(format!(
            "{} is a binary workbook; save it as .xlsx or .xlsm to scan it",
            path.display()
        )));
    }
    let mut package = Package::open(path)?;
    let mut skips = Vec::new();
    let workbook = Workbook::read(&mut package, &mut skips)?;
    let mut report = ReportModel {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        ..ReportModel::default()
    };

    report.queries = workbook.queries(path, &mut skips);

    let mut placed: HashSet<String> = HashSet::new();
    let formulas: Vec<Vec<(String, String)>> = workbook
        .sheets
        .iter()
        .map(|sheet| sheet_formulas(&mut package, &sheet.part, &mut skips))
        .collect();
    let cube_cells: HashSet<(String, String)> = workbook
        .sheets
        .iter()
        .zip(&formulas)
        .flat_map(|(sheet, cells)| {
            cells
                .iter()
                .filter(|(_, formula)| !cube_calls(formula).is_empty())
                .map(|(cell, _)| (sheet.name.to_lowercase(), cell.clone()))
        })
        .collect();

    for (sheet, cells) in workbook.sheets.iter().zip(&formulas) {
        let mut visuals = Vec::new();
        let rels = package.rels(&sheet.part, &mut skips);
        for rel in &rels {
            match rel.kind.as_str() {
                "pivotTable" => {
                    if let Some(visual) = workbook.pivot(&mut package, &rel.target, &mut skips) {
                        visuals.push(visual);
                    }
                }
                "slicer" | "timeline" => {
                    let Some(part) = package.read(&rel.target, &mut skips) else {
                        continue;
                    };
                    for element in part.children.iter().filter(|e| e.name == rel.kind) {
                        let Some(cache) = element.attr("cache") else {
                            continue;
                        };
                        let Some(targets) = workbook.slicers.get(&cache.to_lowercase()) else {
                            continue;
                        };
                        placed.insert(cache.to_lowercase());
                        visuals.push(Visual {
                            name: NameKey::new(element.attr("name").unwrap_or(cache)),
                            visual_type: rel.kind.clone(),
                            title: element.attr("caption").map(str::to_string),
                            wells: vec![well("Field", targets.clone())],
                            ..empty_visual()
                        });
                    }
                }
                _ => {}
            }
        }
        let context = FormulaContext {
            path,
            sheet: &sheet.name,
            workbook: &workbook,
            cube_cells: &cube_cells,
        };
        for (cell, formula) in cells {
            if let Some(visual) = context.cube_visual(cell, formula, &mut skips) {
                visuals.push(visual);
            }
        }
        if !visuals.is_empty() {
            report.pages.push(Page {
                name: NameKey::new(&sheet.name),
                display_name: Some(sheet.name.clone()),
                is_hidden: sheet.hidden,
                filters: Vec::new(),
                binding: None,
                visuals,
            });
        }
    }

    // A slicer cache with no slicer drawn still filters its pivots.
    let mut unplaced: Vec<(&String, &Vec<FieldTarget>)> = workbook
        .slicers
        .iter()
        .filter(|(name, _)| !placed.contains(*name))
        .collect();
    unplaced.sort_by(|a, b| a.0.cmp(b.0));
    for (name, targets) in unplaced {
        let mut targets = targets.iter().cloned();
        report.filters.push(Filter {
            name: Some(NameKey::new(name)),
            filter_type: Some("slicer".to_string()),
            target: targets.next(),
            references: targets.collect(),
            ..Filter::default()
        });
    }

    Ok(Ingested {
        value: report,
        skips,
    })
}

fn empty_visual() -> Visual {
    Visual {
        name: NameKey::new(""),
        visual_type: String::new(),
        title: None,
        wells: Vec::new(),
        filters: Vec::new(),
        sorts: Vec::new(),
        conditional_formatting: Vec::new(),
        alt_text: Vec::new(),
        tooltip_page: None,
    }
}

fn well(role: &str, targets: Vec<FieldTarget>) -> FieldWell {
    FieldWell {
        role: role.to_string(),
        projections: targets
            .into_iter()
            .map(|target| Projection {
                target,
                query_ref: None,
                active: true,
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Workbook-level parts
// ---------------------------------------------------------------------------

struct Sheet {
    name: String,
    part: String,
    hidden: bool,
}

/// One `<connection>` of `xl/connections.xml`.
struct Connection {
    name: String,
    /// The connection queries an OLAP model (`olapPr`, or an MSOLAP provider).
    olap: bool,
    /// The workbook's own Data Model (`$Embedded$`), not a published model.
    embedded: bool,
    command: Option<String>,
    command_type: Option<String>,
}

impl Connection {
    /// Whether pivots and formulas over this connection read the scanned
    /// model: every OLAP connection of a workbook passed explicitly counts,
    /// since a `pbiazure://` target names a dataset GUID ripbi cannot check.
    fn counts(&self) -> bool {
        self.olap && !self.embedded
    }
}

/// What a pivot cache contributes: its field and hierarchy names, and its
/// calculated members. Only an OLAP cache is kept.
struct Cache {
    fields: Vec<String>,
    hierarchies: Vec<String>,
    calculated: Vec<String>,
}

struct Workbook {
    sheets: Vec<Sheet>,
    connections: HashMap<String, Connection>,
    /// Pivot caches by part name; `None` for a cache that is not OLAP.
    caches: HashMap<String, Option<Cache>>,
    /// Calculated member and set names, as normalized unique names. They are
    /// local to the workbook and never bind.
    calculated: HashSet<String>,
    /// OLAP slicer and timeline caches by lowercased name → their targets.
    slicers: HashMap<String, Vec<FieldTarget>>,
    /// Lowercased defined names.
    defined_names: HashSet<String>,
}

impl Workbook {
    fn read(package: &mut Package, skips: &mut Vec<SkipNotice>) -> Result<Self> {
        let workbook_part = package
            .rels("", skips)
            .into_iter()
            .find(|rel| rel.kind == "officeDocument")
            .map_or_else(|| "xl/workbook.xml".to_string(), |rel| rel.target);
        let Some(root) = package.read(&workbook_part, skips) else {
            return Err(Error::UnsupportedFormat(format!(
                "{} has no readable workbook part ({workbook_part})",
                package.path.display()
            )));
        };
        let rels = package.rels(&workbook_part, skips);
        let target_of = |id: &str| {
            rels.iter()
                .find(|rel| rel.id == id)
                .map(|rel| rel.target.clone())
        };

        let sheets = root
            .child("sheets")
            .map(|sheets| {
                sheets
                    .children
                    .iter()
                    .filter(|e| e.name == "sheet")
                    .filter_map(|sheet| {
                        Some(Sheet {
                            name: sheet.attr("name")?.to_string(),
                            part: target_of(sheet.attr("id")?)?,
                            hidden: sheet.attr("state").is_some_and(|s| s != "visible"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let defined_names = root
            .child("definedNames")
            .map(|names| {
                names
                    .children
                    .iter()
                    .filter_map(|e| e.attr("name"))
                    .map(str::to_lowercase)
                    .collect()
            })
            .unwrap_or_default();

        let mut workbook = Workbook {
            sheets,
            connections: HashMap::new(),
            caches: HashMap::new(),
            calculated: HashSet::new(),
            slicers: HashMap::new(),
            defined_names,
        };
        for rel in &rels {
            if rel.kind == "connections" {
                workbook.read_connections(package, &rel.target, skips);
            }
        }
        for rel in &rels {
            match rel.kind.as_str() {
                "pivotCacheDefinition" => workbook.cache(package, &rel.target, skips),
                "slicerCache" | "timelineCache" => {
                    workbook.read_slicer_cache(package, &rel.target, skips);
                }
                _ => {}
            }
        }
        Ok(workbook)
    }

    fn read_connections(&mut self, package: &mut Package, part: &str, skips: &mut Vec<SkipNotice>) {
        let Some(root) = package.read(part, skips) else {
            return;
        };
        for element in root.children.iter().filter(|e| e.name == "connection") {
            let Some(id) = element.attr("id") else {
                continue;
            };
            let name = element.attr("name").unwrap_or(id).to_string();
            let db = element.child("dbPr");
            let connection_string = db.and_then(|db| db.attr("connection")).unwrap_or("");
            let lower = connection_string.to_ascii_lowercase();
            let connection = Connection {
                olap: element.child("olapPr").is_some() || lower.contains("msolap"),
                embedded: lower.contains("$embedded$") || element.attr("model") == Some("1"),
                command: db.and_then(|db| db.attr("command")).map(str::to_string),
                command_type: db.and_then(|db| db.attr("commandType")).map(str::to_string),
                name,
            };
            let detail = if connection.embedded {
                Some(format!(
                    "connection '{}' queries the workbook's own Data Model, not a published \
                     semantic model; pivots and formulas over it were not read",
                    connection.name
                ))
            } else if !connection.olap {
                Some(format!(
                    "connection '{}' does not query an OLAP model; nothing it loads binds the \
                     model",
                    connection.name
                ))
            } else {
                None
            };
            if let Some(detail) = detail {
                skips.push(SkipNotice {
                    path: package.path.clone(),
                    location: Some(part.to_string()),
                    kind: SkipKind::NotAnalyzed,
                    detail,
                });
            }
            self.connections.insert(id.to_string(), connection);
        }
    }

    /// Reads one pivot cache definition (once), recording its calculated
    /// names workbook-wide.
    fn cache(&mut self, package: &mut Package, part: &str, skips: &mut Vec<SkipNotice>) {
        if self.caches.contains_key(part) {
            return;
        }
        let cache = package.read(part, skips).and_then(|root| {
            let source = root.child("cacheSource")?;
            if source.attr("type") != Some("external") {
                return None;
            }
            let hierarchies: Vec<String> = root
                .child("cacheHierarchies")
                .map(|h| {
                    h.children
                        .iter()
                        .filter(|e| e.name == "cacheHierarchy")
                        .map(|e| e.attr("uniqueName").unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default();
            let olap = match source.attr("connectionId") {
                Some(id) => match self.connections.get(id) {
                    Some(connection) => connection.counts(),
                    None => !hierarchies.is_empty(),
                },
                None => !hierarchies.is_empty(),
            };
            if !olap {
                return None;
            }
            let fields = root
                .child("cacheFields")
                .map(|f| {
                    f.children
                        .iter()
                        .filter(|e| e.name == "cacheField")
                        .map(|e| e.attr("name").unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default();
            let mut calculated = Vec::new();
            for member in root
                .child("calculatedMembers")
                .map(|c| c.children.as_slice())
                .unwrap_or_default()
            {
                if let Some(name) = member.attr("name") {
                    self.calculated.extend(unique_name(name));
                }
                if let Some(mdx) = member.attr("mdx") {
                    calculated.push(mdx.to_string());
                }
            }
            Some(Cache {
                fields,
                hierarchies,
                calculated,
            })
        });
        self.caches.insert(part.to_string(), cache);
    }

    /// Reads an OLAP slicer or timeline cache: the levels it filters on
    /// (`(All)` excepted), else its source hierarchy.
    fn read_slicer_cache(
        &mut self,
        package: &mut Package,
        part: &str,
        skips: &mut Vec<SkipNotice>,
    ) {
        let Some(root) = package.read(part, skips) else {
            return;
        };
        let (Some(name), Some(source)) = (root.attr("name"), root.attr("sourceName")) else {
            return;
        };
        // A slicer over a range pivot names a plain field, not a hierarchy.
        if !source.trim_start().starts_with('[') {
            return;
        }
        let mut levels = Vec::new();
        root.descendants("level", &mut levels);
        let mut targets: Vec<FieldTarget> = levels
            .iter()
            .filter_map(|level| level.attr("uniqueName"))
            .filter_map(|name| exact_target(name, &self.calculated))
            .filter(|target| {
                !matches!(target, FieldTarget::Mdx { level: Some(level), .. } if level.as_str() == "(All)")
            })
            .collect();
        if targets.is_empty() {
            targets.extend(exact_target(source, &self.calculated));
        }
        self.slicers.insert(name.to_lowercase(), targets);
    }

    /// One PivotTable as a visual, or `None` when it is not over an OLAP
    /// connection (a range pivot binds nothing in the model).
    fn pivot(
        &self,
        package: &mut Package,
        part: &str,
        skips: &mut Vec<SkipNotice>,
    ) -> Option<Visual> {
        let cache_part = package
            .rels(part, skips)
            .into_iter()
            .find(|rel| rel.kind == "pivotCacheDefinition")?
            .target;
        let cache = self.caches.get(&cache_part)?.as_ref()?;
        let root = package.read(part, skips)?;
        let name = root.attr("name").unwrap_or("PivotTable").to_string();
        let field = |index: Option<&str>| -> Option<FieldTarget> {
            let index: usize = index?.parse().ok()?;
            exact_target(cache.fields.get(index)?, &self.calculated)
        };
        let list = |list: &str, item: &str, attribute: &str| -> Vec<FieldTarget> {
            root.child(list)
                .map(|l| {
                    l.children
                        .iter()
                        .filter(|e| e.name == item)
                        .filter_map(|e| field(e.attr(attribute)))
                        .collect()
                })
                .unwrap_or_default()
        };

        let mut wells = vec![
            well("Rows", list("rowFields", "field", "x")),
            well("Columns", list("colFields", "field", "x")),
            well("Filters", list("pageFields", "pageField", "fld")),
        ];

        let mut values = Vec::new();
        for data in root
            .child("dataFields")
            .map(|d| d.children.as_slice())
            .unwrap_or_default()
        {
            let index = data.attr("fld").and_then(|i| i.parse::<usize>().ok());
            let Some(cache_name) = index.and_then(|i| cache.fields.get(i)) else {
                continue;
            };
            if let Some(target) = exact_target(cache_name, &self.calculated) {
                values.push(target);
                continue;
            }
            if self.is_calculated(cache_name) {
                continue;
            }
            // An implicit measure: the column it aggregates is recorded only
            // on the data field's extension.
            let mut future = Vec::new();
            data.descendants("dataFieldFutureData", &mut future);
            let inner = future
                .first()
                .and_then(|f| f.attr("sourceField"))
                .and_then(|source| field(Some(source)));
            match inner {
                Some(inner) => values.push(FieldTarget::Aggregation {
                    function: Some(data.attr("subtotal").unwrap_or("sum").to_string()),
                    inner: Box::new(inner),
                }),
                None => skips.push(SkipNotice {
                    path: package.path.clone(),
                    location: Some(part.to_string()),
                    kind: SkipKind::NotAnalyzed,
                    detail: format!(
                        "pivot '{name}' data field '{}' names no model field ripbi can read; \
                         it was not read",
                        data.attr("name").unwrap_or(cache_name)
                    ),
                }),
            }
        }
        wells.push(well("Values", values));

        let calculated: Vec<FieldTarget> = cache
            .calculated
            .iter()
            .flat_map(|mdx| scanned_targets(strip_quotes(mdx), &self.calculated))
            .collect();
        wells.push(well("Calculated members", calculated));
        wells.retain(|w| !w.projections.is_empty());

        // Label and value filters: a Top 10 by a measure binds the measure.
        let filters = root
            .child("filters")
            .map(|f| {
                f.children
                    .iter()
                    .filter(|e| e.name == "filter")
                    .filter_map(|filter| {
                        let mut references = Vec::new();
                        if let Some(hierarchy) = filter
                            .attr("iMeasureHier")
                            .and_then(|i| i.parse::<usize>().ok())
                            .and_then(|i| cache.hierarchies.get(i))
                        {
                            references.extend(exact_target(hierarchy, &self.calculated));
                        }
                        references.extend(field(filter.attr("iMeasureFld")));
                        let target = field(filter.attr("fld"));
                        (target.is_some() || !references.is_empty()).then(|| Filter {
                            filter_type: filter.attr("type").map(str::to_string),
                            target,
                            references,
                            ..Filter::default()
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(Visual {
            name: NameKey::new(&name),
            visual_type: "pivotTable".to_string(),
            title: Some(name),
            wells,
            filters,
            ..empty_visual()
        })
    }

    fn is_calculated(&self, name: &str) -> bool {
        unique_name(name).is_some_and(|name| self.calculated.contains(&name))
    }

    /// The DAX and MDX commands of OLAP connections — query tables. A cube
    /// command (`commandType="1"`, or a bare cube name like `Model`) is the
    /// connection's target, not a query.
    fn queries(&self, path: &Path, skips: &mut Vec<SkipNotice>) -> Vec<ReportQuery> {
        let mut connections: Vec<&Connection> = self
            .connections
            .values()
            .filter(|connection| connection.counts())
            .collect();
        connections.sort_by(|a, b| a.name.cmp(&b.name));
        let mut queries = Vec::new();
        for connection in connections {
            let Some(command) = connection.command.as_deref() else {
                continue;
            };
            if connection.command_type.as_deref() == Some("1") {
                continue;
            }
            let upper = command.trim_start().to_ascii_uppercase();
            let is_dax = upper.starts_with("EVALUATE")
                || upper.starts_with("DEFINE")
                || upper.contains("EVALUATE");
            let is_mdx = usage::sniff_language(command) == QueryLanguage::Mdx;
            if is_dax || is_mdx {
                queries.push(ReportQuery {
                    connection: connection.name.clone(),
                    text: command.to_string(),
                });
            } else if command.contains(['\n', '(', '[']) {
                skips.push(SkipNotice {
                    path: path.to_path_buf(),
                    location: Some(format!("connection '{}'", connection.name)),
                    kind: SkipKind::NotAnalyzed,
                    detail: "the connection's command is neither a cube name nor a DAX or MDX \
                             query; it was not read"
                        .to_string(),
                });
            }
        }
        queries
    }
}

// ---------------------------------------------------------------------------
// MDX names
// ---------------------------------------------------------------------------

/// The bracketed names of the first MDX chain in `text`, member keys dropped.
fn chain_names(chain: &[usage::Segment<'_>]) -> Vec<String> {
    chain
        .iter()
        .filter(|segment| !segment.key)
        .map(|segment| usage::unescape_bracket(segment.name).into_owned())
        .collect()
}

/// A unique name normalized for comparison: `[measures].[sales x2]`.
fn unique_name(text: &str) -> Option<String> {
    let chains = usage::mdx_chains(text);
    let names = chain_names(chains.first()?);
    (!names.is_empty()).then(|| join(&names))
}

fn join(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("[{}]", name.to_lowercase()))
        .collect::<Vec<_>>()
        .join(".")
}

/// Whether the leading segments of `names` are a calculated member or set.
fn names_calculated(names: &[String], calculated: &HashSet<String>) -> bool {
    (1..=names.len().min(2)).any(|n| calculated.contains(&join(&names[..n])))
}

fn mdx_target(names: &[String], exact: bool) -> Option<FieldTarget> {
    let mut names = names.iter().map(NameKey::new);
    Some(FieldTarget::Mdx {
        dimension: names.next()?,
        hierarchy: names.next(),
        level: names.next(),
        exact,
    })
}

/// The target of a whole unique name a client stored for a field — `None`
/// for a calculated name or a name with no bracketed segment (an implicit
/// measure's `Unsupported0`, a range pivot's column caption).
fn exact_target(name: &str, calculated: &HashSet<String>) -> Option<FieldTarget> {
    let chains = usage::mdx_chains(name);
    let names = chain_names(chains.first()?);
    if names.is_empty() || names_calculated(&names, calculated) {
        return None;
    }
    mdx_target(&names, true)
}

/// Every name an MDX expression scans to, calculated names excepted.
fn scanned_targets(text: &str, calculated: &HashSet<String>) -> Vec<FieldTarget> {
    usage::mdx_chains(text)
        .iter()
        .map(|chain| chain_names(chain))
        .filter(|names| !names.is_empty() && !names_calculated(names, calculated))
        .filter_map(|names| mdx_target(&names, false))
        .collect()
}

/// A set formula Excel stored quoted (`'[Product].[All Products].Children'`):
/// MDX reads single quotes as a string, so the quotes come off first.
fn strip_quotes(mdx: &str) -> &str {
    let trimmed = mdx.trim();
    trimmed
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
        .unwrap_or(trimmed)
}

// ---------------------------------------------------------------------------
// CUBE formulas
// ---------------------------------------------------------------------------

/// Every formula of a sheet, as `(cell, formula)`. A shared formula's
/// followers repeat the leader's names, so only the leader's text is read.
fn sheet_formulas(
    package: &mut Package,
    part: &str,
    skips: &mut Vec<SkipNotice>,
) -> Vec<(String, String)> {
    let Some(root) = package.read(part, skips) else {
        return Vec::new();
    };
    let mut cells = Vec::new();
    root.descendants("c", &mut cells);
    cells
        .into_iter()
        .filter_map(|cell| {
            let formula = cell.child("f")?;
            let text = formula.text.trim();
            (!text.is_empty()).then(|| {
                (
                    cell.attr("r").unwrap_or_default().to_ascii_uppercase(),
                    text.to_string(),
                )
            })
        })
        .collect()
}

struct CubeCall<'a> {
    function: &'static str,
    args: Vec<&'a str>,
}

/// Every CUBE call in a formula, nested ones included.
fn cube_calls(formula: &str) -> Vec<CubeCall<'_>> {
    let bytes = formula.as_bytes();
    let mut calls = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = skip_quoted(bytes, i, b'"'),
            b'\'' => i = skip_quoted(bytes, i, b'\''),
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
                {
                    i += 1;
                }
                if bytes.get(i) != Some(&b'(') {
                    continue;
                }
                let name = formula[start..i].to_ascii_uppercase();
                let name = name.strip_prefix("_XLFN.").unwrap_or(&name);
                if let Some(function) = CUBE_FUNCTIONS.iter().find(|f| **f == name) {
                    calls.push(CubeCall {
                        function,
                        args: split_args(formula, i + 1),
                    });
                }
            }
            _ => i += 1,
        }
    }
    calls
}

/// The index just past a quoted run starting at `start`; a doubled quote
/// is an escaped one.
fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == quote {
            if bytes.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

/// The top-level arguments of a call whose argument list starts at `start`
/// (just past its `(`).
fn split_args(formula: &str, start: usize) -> Vec<&str> {
    let bytes = formula.as_bytes();
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut arg_start = start;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = skip_quoted(bytes, i, b'"');
                continue;
            }
            b'\'' => {
                i = skip_quoted(bytes, i, b'\'');
                continue;
            }
            b'(' | b'{' => depth += 1,
            b')' | b'}' if depth > 0 => depth -= 1,
            b')' => {
                args.push(formula[arg_start..i].trim());
                return args;
            }
            b',' if depth == 0 => {
                args.push(formula[arg_start..i].trim());
                arg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    args.push(formula[arg_start..].trim());
    args
}

/// The positions of a CUBE function's member or set arguments.
fn member_args(function: &str, count: usize) -> Vec<usize> {
    match function {
        "CUBEVALUE" => (1..count).collect(),
        "CUBESET" => vec![1, 4],
        _ => vec![1],
    }
}

/// The text of `arg` when it is exactly one string literal.
fn literal(arg: &str) -> Option<String> {
    let bytes = arg.as_bytes();
    (bytes.first() == Some(&b'"') && skip_quoted(bytes, 0, b'"') == bytes.len() && bytes.len() >= 2)
        .then(|| arg[1..arg.len() - 1].replace("\"\"", "\""))
}

/// Every string literal inside an expression.
fn literals_in(expression: &str) -> Vec<String> {
    let bytes = expression.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let end = skip_quoted(bytes, i, b'"');
                let inner_end = end.saturating_sub(1).max(i + 1);
                out.push(expression[i + 1..inner_end].replace("\"\"", "\""));
                i = end;
            }
            b'\'' => i = skip_quoted(bytes, i, b'\''),
            _ => i += 1,
        }
    }
    out
}

/// A cell reference: `A1`, `$A$1`, `Sheet2!A1`, `'My sheet'!$B$3`, as
/// (lowercased sheet, uppercased cell without `$`). Ranges are not cells.
fn cell_reference(arg: &str) -> Option<(Option<String>, String)> {
    let (sheet, cell) = match arg.rsplit_once('!') {
        Some((sheet, cell)) => {
            let sheet = sheet
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
                .map_or_else(|| sheet.to_string(), |s| s.replace("''", "'"));
            (Some(sheet.to_lowercase()), cell)
        }
        None => (None, arg),
    };
    let cell: String = cell.chars().filter(|c| *c != '$').collect();
    let letters = cell.chars().take_while(char::is_ascii_alphabetic).count();
    let digits = &cell[letters..];
    ((1..=3).contains(&letters) && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
        .then(|| (sheet, cell.to_ascii_uppercase()))
}

fn is_name(arg: &str) -> bool {
    let mut chars = arg.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '\\')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '\\')
}

struct FormulaContext<'a> {
    path: &'a Path,
    sheet: &'a str,
    workbook: &'a Workbook,
    /// Every cell holding a CUBE call, as (lowercased sheet, cell).
    cube_cells: &'a HashSet<(String, String)>,
}

impl FormulaContext<'_> {
    /// The CUBE calls of one formula cell as a visual named after the cell,
    /// one well per function; `None` when the cell binds nothing.
    fn cube_visual(
        &self,
        cell: &str,
        formula: &str,
        skips: &mut Vec<SkipNotice>,
    ) -> Option<Visual> {
        let calls = cube_calls(formula);
        if calls.is_empty() {
            return None;
        }
        let location = format!("{}!{cell}", self.sheet);
        let mut wells: Vec<FieldWell> = Vec::new();
        for call in calls {
            if !self.connection_counts(call.args.first().copied(), &location, call.function, skips)
            {
                continue;
            }
            let mut targets = Vec::new();
            for position in member_args(call.function, call.args.len()) {
                let Some(arg) = call.args.get(position).copied() else {
                    continue;
                };
                targets.extend(self.arg_targets(&call, position, arg, &location, skips));
            }
            match wells.iter_mut().find(|w| w.role == call.function) {
                Some(existing) => existing.projections.extend(well("", targets).projections),
                None => wells.push(well(call.function, targets)),
            }
        }
        wells.retain(|w| !w.projections.is_empty());
        (!wells.is_empty()).then(|| Visual {
            name: NameKey::new(cell),
            visual_type: "cubeFormula".to_string(),
            title: Some(location),
            wells,
            ..empty_visual()
        })
    }

    /// Whether a CUBE call's connection argument names a connection that
    /// reads the scanned model. A computed connection is assumed to: keeping
    /// extra objects alive is the safe direction.
    fn connection_counts(
        &self,
        arg: Option<&str>,
        location: &str,
        function: &str,
        skips: &mut Vec<SkipNotice>,
    ) -> bool {
        let Some(name) = arg.and_then(literal) else {
            return true;
        };
        let Some(connection) = self
            .workbook
            .connections
            .values()
            .find(|c| c.name.eq_ignore_ascii_case(&name))
        else {
            return true;
        };
        if connection.counts() {
            return true;
        }
        skips.push(SkipNotice {
            path: self.path.to_path_buf(),
            location: Some(location.to_string()),
            kind: SkipKind::NotAnalyzed,
            detail: format!(
                "{function} queries connection '{}', which does not read the scanned model; \
                 it was not read",
                connection.name
            ),
        });
        false
    }

    /// The targets of one member or set argument. A string literal scans;
    /// a reference to another CUBE cell, a nested CUBE call, and a slicer's
    /// name are covered where they are defined; anything else is a notice,
    /// with whatever literals it carries still read.
    fn arg_targets(
        &self,
        call: &CubeCall<'_>,
        position: usize,
        arg: &str,
        location: &str,
        skips: &mut Vec<SkipNotice>,
    ) -> Vec<FieldTarget> {
        let calculated = &self.workbook.calculated;
        if arg.is_empty() || arg.parse::<f64>().is_ok() {
            return Vec::new();
        }
        if let Some(text) = literal(arg) {
            if call.function == "CUBEKPIMEMBER" && !text.contains('[') {
                return mdx_target(&["Measures".to_string(), text], false)
                    .into_iter()
                    .collect();
            }
            return scanned_targets(&text, calculated);
        }
        if let Some((sheet, cell)) = cell_reference(arg) {
            let sheet = sheet.unwrap_or_else(|| self.sheet.to_lowercase());
            if self.cube_cells.contains(&(sheet, cell)) {
                return Vec::new();
            }
        } else if is_name(arg) {
            let lower = arg.to_lowercase();
            if self.workbook.slicers.contains_key(&lower) {
                return Vec::new();
            }
        } else if starts_with_call(arg) && !cube_calls(arg).is_empty() {
            return Vec::new();
        }
        let what = if is_name(arg) && self.workbook.defined_names.contains(&arg.to_lowercase()) {
            "a defined name"
        } else if cell_reference(arg).is_some() {
            "a cell reference"
        } else {
            "a computed expression"
        };
        skips.push(SkipNotice {
            path: self.path.to_path_buf(),
            location: Some(location.to_string()),
            kind: SkipKind::NotAnalyzed,
            detail: format!(
                "argument {} of {} is {what} (`{arg}`), which ripbi cannot evaluate; only its \
                 string literals were read",
                position + 1,
                call.function
            ),
        });
        literals_in(arg)
            .iter()
            .filter(|text| text.contains('[') && text.contains(']'))
            .flat_map(|text| scanned_targets(text, calculated))
            .collect()
    }
}

/// Whether `arg` is wholly one function call (`CUBEMEMBER(…)`).
fn starts_with_call(arg: &str) -> bool {
    arg.ends_with(')')
        && arg.find('(').is_some_and(|open| {
            open > 0
                && arg[..open]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        })
}

// ---------------------------------------------------------------------------
// The package: ZIP parts, relationships, and a minimal XML tree
// ---------------------------------------------------------------------------

struct Package {
    path: PathBuf,
    zip: ZipArchive<File>,
    /// Lowercased part name → the name as stored.
    names: HashMap<String, String>,
}

/// One relationship of a part, its target resolved to a part name.
struct Rel {
    id: String,
    /// The last segment of the relationship type URI (`worksheet`,
    /// `pivotTable`, …) — the same in the transitional and Strict schemas.
    kind: String,
    target: String,
}

impl Package {
    fn open(path: &Path) -> Result<Self> {
        let zip = ZipArchive::new(File::open(path)?)?;
        let names = zip
            .file_names()
            .filter_map(|name| name.ok())
            .map(|name| (name.to_lowercase(), name.into_owned()))
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            zip,
            names,
        })
    }

    /// The parsed part, or `None` when it is absent. A part that cannot be
    /// read or parsed is a notice.
    fn read(&mut self, part: &str, skips: &mut Vec<SkipNotice>) -> Option<Element> {
        let name = self.names.get(&part.to_lowercase())?.clone();
        let mut bytes = Vec::new();
        let read = self
            .zip
            .by_name(&name)
            .map_err(|error| error.to_string())
            .and_then(|mut entry| {
                entry
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())
            })
            .and_then(|_| parse_xml(&bytes));
        match read {
            Ok(element) => Some(element),
            Err(error) => {
                skips.push(SkipNotice {
                    path: self.path.clone(),
                    location: Some(name),
                    kind: SkipKind::MalformedValue,
                    detail: format!("workbook part could not be read ({error}); it was skipped"),
                });
                None
            }
        }
    }

    /// The relationships of `part` (`""` for the package root).
    fn rels(&mut self, part: &str, skips: &mut Vec<SkipNotice>) -> Vec<Rel> {
        let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
        let rels_part = if dir.is_empty() {
            format!("_rels/{file}.rels")
        } else {
            format!("{dir}/_rels/{file}.rels")
        };
        let Some(root) = self.read(&rels_part, skips) else {
            return Vec::new();
        };
        root.children
            .iter()
            .filter(|rel| rel.attr("TargetMode") != Some("External"))
            .filter_map(|rel| {
                Some(Rel {
                    id: rel.attr("Id")?.to_string(),
                    kind: rel.attr("Type")?.rsplit('/').next()?.to_string(),
                    target: resolve_target(part, rel.attr("Target")?),
                })
            })
            .collect()
    }
}

/// A relationship target resolved against its source part's folder.
fn resolve_target(source: &str, target: &str) -> String {
    if let Some(absolute) = target.strip_prefix('/') {
        return absolute.to_string();
    }
    let mut parts: Vec<&str> = source.split('/').collect();
    parts.pop();
    for segment in target.split('/') {
        match segment {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            segment => parts.push(segment),
        }
    }
    parts.join("/")
}

/// An XML element by local name — namespace prefixes dropped, so the Strict
/// and transitional schemas read alike.
#[derive(Debug, Default)]
struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    children: Vec<Element>,
    text: String,
}

impl Element {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    fn descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a Element>) {
        for child in &self.children {
            if child.name == name {
                out.push(child);
            }
            child.descendants(name, out);
        }
    }
}

fn element(start: &quick_xml::events::BytesStart<'_>) -> Element {
    let attributes = start
        .attributes()
        .flatten()
        .filter_map(|attribute| {
            let value = attribute.normalized_value(XmlVersion::Implicit1_0).ok()?;
            Some((
                attribute.key.local_name().as_ref().to_string(),
                value.into_owned(),
            ))
        })
        .collect();
    Element {
        name: start.local_name().as_ref().to_string(),
        attributes,
        ..Element::default()
    }
}

/// Parses one part into its root element.
fn parse_xml(bytes: &[u8]) -> std::result::Result<Element, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    let text = text.trim_start_matches('\u{feff}');
    let mut reader = Reader::from_str(text);
    let mut stack = vec![Element::default()];
    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(start) => stack.push(element(&start)),
            Event::Empty(empty) => {
                let element = element(&empty);
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element);
                }
            }
            Event::End(_) => {
                let element = stack
                    .pop()
                    .filter(|_| !stack.is_empty())
                    .ok_or("unbalanced end tag")?;
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
                    Ok(Some(ch)) => Some(ch),
                    _ => match &*reference {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    },
                };
                if let (Some(ch), Some(current)) = (resolved, stack.last_mut()) {
                    current.text.push(ch);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut document = stack
        .pop()
        .filter(|_| stack.is_empty())
        .ok_or("ends before its root element closes")?;
    if document.children.len() != 1 {
        return Err("no single root element".to_string());
    }
    Ok(document.children.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_calls_split_arguments_and_find_nested_calls() {
        let calls = cube_calls(
            r#"IFERROR(CUBEVALUE("Sales Model","[Measures].[Total Sales]",CUBEMEMBER("Sales Model","[Date].[Year].&[2024]"),$A$21),0)"#,
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function, "CUBEVALUE");
        assert_eq!(
            calls[0].args,
            [
                r#""Sales Model""#,
                r#""[Measures].[Total Sales]""#,
                r#"CUBEMEMBER("Sales Model","[Date].[Year].&[2024]")"#,
                "$A$21"
            ]
        );
        assert_eq!(calls[1].function, "CUBEMEMBER");
    }

    #[test]
    fn literals_and_references_are_told_apart() {
        assert_eq!(literal(r#""a ""b"" c""#).as_deref(), Some(r#"a "b" c"#));
        assert_eq!(literal(r#""a"&B1"#), None);
        assert_eq!(cell_reference("$A$21"), Some((None, "A21".to_string())));
        assert_eq!(
            cell_reference("'My sheet'!b3"),
            Some((Some("my sheet".to_string()), "B3".to_string()))
        );
        assert_eq!(cell_reference("Slicer_Country"), None);
        assert!(is_name("Slicer_Country"));
    }

    #[test]
    fn exact_names_split_into_dimension_hierarchy_and_level() {
        let none = HashSet::new();
        assert_eq!(
            exact_target("[Product].[Category].[Category]", &none),
            Some(FieldTarget::Mdx {
                dimension: NameKey::new("Product"),
                hierarchy: Some(NameKey::new("Category")),
                level: Some(NameKey::new("Category")),
                exact: true,
            })
        );
        assert_eq!(exact_target("Unsupported0", &none), None);
        let calculated: HashSet<String> = ["[measures].[sales x2]".to_string()].into();
        assert_eq!(exact_target("[Measures].[Sales x2]", &calculated), None);
        // Member keys are values, not names.
        assert_eq!(
            scanned_targets("[Sales].[Region].&[West]", &none),
            [FieldTarget::Mdx {
                dimension: NameKey::new("Sales"),
                hierarchy: Some(NameKey::new("Region")),
                level: None,
                exact: false,
            }]
        );
    }

    #[test]
    fn relationship_targets_resolve_against_the_source_folder() {
        assert_eq!(
            resolve_target(
                "xl/pivotTables/pivotTable1.xml",
                "../pivotCache/pivotCacheDefinition1.xml"
            ),
            "xl/pivotCache/pivotCacheDefinition1.xml"
        );
        assert_eq!(
            resolve_target("xl/workbook.xml", "/xl/connections.xml"),
            "xl/connections.xml"
        );
        assert_eq!(resolve_target("", "xl/workbook.xml"), "xl/workbook.xml");
    }
}
