//! TMDL semantic-model parsing: a `definition/` folder into a
//! [`TabularDatabase`].
//!
//! Parsing is two stages. Stage one scans each `.tmdl` file into a generic node
//! tree ([`Node`]): tab-depth nesting, `key: value` properties, bare flags,
//! object headers, and multi-line expression blocks — with no knowledge of what
//! the objects mean. Stage two pattern-matches known descriptors into the AST.
//! Anything the AST does not carry is either on the curated ignore list
//! ([`IGNORED_KEYS`], deliberately unmodeled metadata, silent) or reported as a
//! [`SkipNotice`] (unexpected drift). Only a file whose lines cannot be parsed
//! into a tree at all fails with [`Error::Tmdl`]; a malformed *object* is a
//! notice, never a failed run.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::identity::fold_name;
use crate::ingest::{SkipKind, SkipNotice};
use crate::model::{
    CalculationGroup, CalculationItem, Column, ColumnKind, Function, Hierarchy, HierarchyLevel,
    Kpi, Measure, Partition, PartitionSource, Relationship, Role, SharedExpression, Table,
    TablePermission, TabularDatabase,
};
use crate::{Error, Result};

/// Keys this crate deliberately does not model, skipped silently wherever they
/// appear — Tier 1 of the drift policy in `docs/formats.md`. Everything here is
/// metadata that cannot consume a model object, so skipping it cannot cause a
/// false "unused" finding. Anything unknown *off* this list is reported as a
/// skip notice instead.
const IGNORED_KEYS: &[&str] = &[
    // Universal metadata
    "lineageTag",
    "changedProperty",
    "description",
    "annotation",
    "extendedProperty",
    // Column metadata
    "dataType",
    "formatString",
    "summarizeBy",
    "sourceColumn",
    "dataCategory",
    "isKey",
    "isNameInferred",
    "isDataTypeInferred",
    "isUnique",
    "isDefaultLabel",
    "isDefaultImage",
    "isAvailableInMdx",
    "keepUniqueRows",
    "tableDetailPosition",
    // Measure and table metadata
    "displayFolder",
    "isPrivate",
    // Date variations bind columns to hierarchies; deliberately unmodeled
    // (a variation-only hierarchy could be mis-reported — known gap, see
    // docs/formats.md)
    "variation",
    "defaultHierarchy",
    "isDefault",
    "showAsVariationsOnly",
    // Model and database metadata
    "culture",
    "sourceQueryCulture",
    "defaultPowerBIDataSourceVersion",
    "discourageImplicitMeasures",
    "dataAccessOptions",
    "compatibilityLevel",
    "createOrReplace",
    "retainDataTillForceCalculate",
    // Cultures (the cultures/ folder is never read; keys kept for stray uses)
    "cultureInfo",
    "linguisticMetadata",
    "contentType",
    // Calculation groups
    "precedence",
    // Partitions
    "mode",
    // Roles
    "modelPermission",
    // Relationships
    "crossFilteringBehavior",
    "fromCardinality",
    "toCardinality",
    "joinOnDateBehavior",
    "hideArrows",
    "securityFilteringBehavior",
    "reliability",
    // Hierarchies and levels
    "ordinal",
    // KPIs
    "statusGraphic",
];

/// Object descriptors that must carry a name after the descriptor. A file
/// where one does not is malformed TMDL, not drift.
const NAMED_DESCRIPTORS: &[&str] = &[
    "annotation",
    "calculationItem",
    "column",
    "dataSource",
    "expression",
    "extendedProperty",
    "function",
    "hierarchy",
    "level",
    "measure",
    "model",
    "partition",
    "perspective",
    "relationship",
    "role",
    "table",
    "tablePermission",
    "variation",
];

/// Parses a semantic model's `definition/` folder into a [`TabularDatabase`].
///
/// Files are processed in a fixed order (database, model, relationships,
/// expressions, `tables/`, `roles/`, then anything else) so notices come out
/// deterministic. Tables are ordered by `model.tmdl`'s `ref table` directives,
/// with unreferenced table files appended in file-name order.
pub(super) fn load_database(
    definition: &Path,
    name: Option<String>,
    skips: &mut Vec<SkipNotice>,
) -> Result<TabularDatabase> {
    let mut loader = Loader::new(name);

    let mut entries: Vec<(String, PathBuf, bool)> = Vec::new();
    for entry in fs::read_dir(definition)? {
        let entry = entry?;
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.path(),
            is_dir,
        ));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (file_name, path, is_dir) in &entries {
        if *is_dir {
            match file_name.as_str() {
                "tables" => loader.load_tables_dir(path, skips)?,
                "roles" => loader.load_roles_dir(path, skips)?,
                // Cultures and translations are deliberately unmodeled.
                "cultures" => {}
                other => notice(
                    skips,
                    path,
                    None,
                    SkipKind::UnknownObject,
                    format!("unknown directory '{other}' in definition/"),
                ),
            }
            continue;
        }
        match file_name.as_str() {
            "database.tmdl" => loader.load_database_tmdl(path, skips)?,
            "model.tmdl" => loader.load_model_tmdl(path, skips)?,
            "relationships.tmdl" => loader.load_relationships_tmdl(path, skips)?,
            "expressions.tmdl" => loader.load_expressions_tmdl(path, skips)?,
            file_name if file_name.ends_with(".tmdl") => loader.load_extra_tmdl(path, skips)?,
            other => notice(
                skips,
                path,
                None,
                SkipKind::UnknownObject,
                format!("unexpected file '{other}' in definition/"),
            ),
        }
    }

    Ok(loader.finish(definition, skips))
}

/// Accumulates parsed objects across files and assembles the final ordering.
struct Loader {
    database: TabularDatabase,
    /// Tables pending ordering; `model.tmdl`'s refs decide their final order.
    tables: Vec<Table>,
    seen_tables: HashSet<String>,
    table_refs: Vec<(String, usize)>,
    model_tmdl: Option<PathBuf>,
}

impl Loader {
    fn new(name: Option<String>) -> Self {
        Self {
            database: TabularDatabase {
                name,
                ..Default::default()
            },
            tables: Vec::new(),
            seen_tables: HashSet::new(),
            table_refs: Vec::new(),
            model_tmdl: None,
        }
    }

    fn load_database_tmdl(&mut self, path: &Path, skips: &mut Vec<SkipNotice>) -> Result<()> {
        for root in &parse_file(path)? {
            match root.key.as_str() {
                "database" => {
                    for child in &root.children {
                        if is_ignored(child) {
                            continue;
                        }
                        notice(
                            skips,
                            path,
                            Some(child.line),
                            SkipKind::UnknownProperty,
                            format!("unknown property '{}' on database", child.key),
                        );
                    }
                }
                "annotation" | "extendedProperty" => {}
                other => notice(
                    skips,
                    path,
                    Some(root.line),
                    SkipKind::UnknownObject,
                    format!("'{other}' does not belong in database.tmdl"),
                ),
            }
        }
        Ok(())
    }

    fn load_model_tmdl(&mut self, path: &Path, skips: &mut Vec<SkipNotice>) -> Result<()> {
        self.model_tmdl = Some(path.to_path_buf());
        for root in &parse_file(path)? {
            match root.key.as_str() {
                "model" => {
                    for child in &root.children {
                        if is_ignored(child) {
                            continue;
                        }
                        notice(
                            skips,
                            path,
                            Some(child.line),
                            SkipKind::UnknownProperty,
                            format!("unknown property '{}' on model", child.key),
                        );
                    }
                }
                "ref" => match root.name.as_deref() {
                    Some("table") => {
                        let referenced = root.tail.as_deref().map(unquote).unwrap_or_default();
                        self.table_refs.push((referenced, root.line));
                    }
                    // Expression and culture refs are valid directives whose
                    // order this crate does not need.
                    Some("cultureInfo") | Some("expression") => {}
                    other => notice(
                        skips,
                        path,
                        Some(root.line),
                        SkipKind::UnknownObject,
                        format!("unknown ref type '{other:?}' in model.tmdl"),
                    ),
                },
                // TMDL permits objects to live in model.tmdl; rare, but legal.
                "table" => self.add_table(path, root.line, map_table(root, path, skips), skips),
                "relationship" => self.add_relationship(root, path, skips),
                "role" => self.database.roles.push(map_role(root, path, skips)),
                "expression" => self
                    .database
                    .expressions
                    .push(map_expression(root, path, skips)),
                "function" => self
                    .database
                    .functions
                    .push(map_function(root, path, skips)),
                "annotation" | "extendedProperty" | "dataSource" | "perspective" => {}
                other => notice(
                    skips,
                    path,
                    Some(root.line),
                    SkipKind::UnknownObject,
                    format!("unknown object '{other}' in model.tmdl"),
                ),
            }
        }
        Ok(())
    }

    fn load_relationships_tmdl(&mut self, path: &Path, skips: &mut Vec<SkipNotice>) -> Result<()> {
        for root in &parse_file(path)? {
            match root.key.as_str() {
                "relationship" => self.add_relationship(root, path, skips),
                "annotation" | "extendedProperty" => {}
                other => notice(
                    skips,
                    path,
                    Some(root.line),
                    SkipKind::UnknownObject,
                    format!("'{other}' does not belong in relationships.tmdl"),
                ),
            }
        }
        Ok(())
    }

    fn load_expressions_tmdl(&mut self, path: &Path, skips: &mut Vec<SkipNotice>) -> Result<()> {
        for root in &parse_file(path)? {
            match root.key.as_str() {
                "expression" => self
                    .database
                    .expressions
                    .push(map_expression(root, path, skips)),
                "function" => self
                    .database
                    .functions
                    .push(map_function(root, path, skips)),
                "annotation" | "extendedProperty" => {}
                other => notice(
                    skips,
                    path,
                    Some(root.line),
                    SkipKind::UnknownObject,
                    format!("'{other}' does not belong in expressions.tmdl"),
                ),
            }
        }
        Ok(())
    }

    fn load_extra_tmdl(&mut self, path: &Path, skips: &mut Vec<SkipNotice>) -> Result<()> {
        // A .tmdl file this crate does not know by name: known objects are
        // still mapped (TMDL allows any object in any file); anything else is
        // drift worth a notice.
        for root in &parse_file(path)? {
            match root.key.as_str() {
                "table" => self.add_table(path, root.line, map_table(root, path, skips), skips),
                "relationship" => self.add_relationship(root, path, skips),
                "role" => self.database.roles.push(map_role(root, path, skips)),
                "expression" => self
                    .database
                    .expressions
                    .push(map_expression(root, path, skips)),
                "function" => self
                    .database
                    .functions
                    .push(map_function(root, path, skips)),
                "annotation" | "extendedProperty" | "dataSource" | "perspective" => {}
                other => notice(
                    skips,
                    path,
                    Some(root.line),
                    SkipKind::UnknownObject,
                    format!("unknown object '{other}'"),
                ),
            }
        }
        Ok(())
    }

    fn load_tables_dir(&mut self, dir: &Path, skips: &mut Vec<SkipNotice>) -> Result<()> {
        for (file_name, path, is_dir) in sorted_entries(dir)? {
            if is_dir {
                notice(
                    skips,
                    &path,
                    None,
                    SkipKind::UnknownObject,
                    format!("unexpected directory '{file_name}' in tables/"),
                );
                continue;
            }
            if !file_name.ends_with(".tmdl") {
                notice(
                    skips,
                    &path,
                    None,
                    SkipKind::UnknownObject,
                    format!("unexpected file '{file_name}' in tables/"),
                );
                continue;
            }
            for root in &parse_file(&path)? {
                match root.key.as_str() {
                    "table" => {
                        self.add_table(&path, root.line, map_table(root, &path, skips), skips)
                    }
                    "annotation" | "extendedProperty" => {}
                    other => notice(
                        skips,
                        &path,
                        Some(root.line),
                        SkipKind::UnknownObject,
                        format!("'{other}' does not belong in a table file"),
                    ),
                }
            }
        }
        Ok(())
    }

    fn load_roles_dir(&mut self, dir: &Path, skips: &mut Vec<SkipNotice>) -> Result<()> {
        for (file_name, path, is_dir) in sorted_entries(dir)? {
            if is_dir {
                notice(
                    skips,
                    &path,
                    None,
                    SkipKind::UnknownObject,
                    format!("unexpected directory '{file_name}' in roles/"),
                );
                continue;
            }
            if !file_name.ends_with(".tmdl") {
                notice(
                    skips,
                    &path,
                    None,
                    SkipKind::UnknownObject,
                    format!("unexpected file '{file_name}' in roles/"),
                );
                continue;
            }
            for root in &parse_file(&path)? {
                match root.key.as_str() {
                    "role" => self.database.roles.push(map_role(root, &path, skips)),
                    "annotation" | "extendedProperty" => {}
                    other => notice(
                        skips,
                        &path,
                        Some(root.line),
                        SkipKind::UnknownObject,
                        format!("'{other}' does not belong in a role file"),
                    ),
                }
            }
        }
        Ok(())
    }

    fn add_table(&mut self, path: &Path, line: usize, table: Table, skips: &mut Vec<SkipNotice>) {
        if self.seen_tables.insert(fold_name(&table.name)) {
            self.tables.push(table);
        } else {
            notice(
                skips,
                path,
                Some(line),
                SkipKind::MalformedValue,
                format!("duplicate table '{}'", table.name),
            );
        }
    }

    fn add_relationship(&mut self, root: &Node, path: &Path, skips: &mut Vec<SkipNotice>) {
        if let Some(relationship) = map_relationship(root, path, skips) {
            self.database.relationships.push(relationship);
        }
    }

    /// Applies `ref table` ordering and appends unreferenced tables in
    /// file-name order, noticing references with no matching file.
    fn finish(mut self, definition: &Path, skips: &mut Vec<SkipNotice>) -> TabularDatabase {
        let folded: Vec<String> = self.tables.iter().map(|t| fold_name(&t.name)).collect();
        let mut slots: Vec<Option<Table>> = self.tables.drain(..).map(Some).collect();

        for (reference, line) in &self.table_refs {
            let wanted = fold_name(reference);
            let mut found = None;
            for (index, slot) in slots.iter().enumerate() {
                if slot.is_some() && folded[index] == wanted {
                    found = Some(index);
                    break;
                }
            }
            match found {
                Some(index) => {
                    if let Some(table) = slots[index].take() {
                        self.database.tables.push(table);
                    }
                }
                None => notice(
                    skips,
                    self.model_tmdl.as_deref().unwrap_or(definition),
                    Some(*line),
                    SkipKind::UnknownObject,
                    format!("ref table '{reference}' has no matching table definition"),
                ),
            }
        }
        for table in slots.into_iter().flatten() {
            self.database.tables.push(table);
        }
        self.database
    }
}

/// A directory listing sorted by file name, as `(name, path, is_dir)`.
fn sorted_entries(dir: &Path) -> Result<Vec<(String, PathBuf, bool)>> {
    let mut entries: Vec<(String, PathBuf, bool)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.path(),
            is_dir,
        ));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

/// Reads and parses one `.tmdl` file into its root nodes.
fn parse_file(path: &Path) -> Result<Vec<Node>> {
    let text = fs::read_to_string(path)?;
    let nodes = parse_document(&text, path)?;
    validate(&nodes, path)?;
    Ok(nodes)
}

// --- Stage 1: the generic node tree -----------------------------------------

/// One parsed TMDL line, owning the lines nested beneath it by tab depth.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Node {
    /// First token: an object descriptor (`table`) or a property key
    /// (`dataType`).
    key: String,
    /// The name after the descriptor, as written (quotes kept).
    name: Option<String>,
    /// Leftover text after the name, as written — `ref table`'s target.
    tail: Option<String>,
    /// The value after `:` or `=`, or a captured multi-line block.
    value: NodeValue,
    /// 1-based line number the node started on.
    line: usize,
    /// Lines nested beneath this one by tab depth.
    children: Vec<Node>,
}

impl Node {
    /// The node's text value, inline or block — `None` for flags and nameless
    /// headers.
    fn text(&self) -> Option<&str> {
        match &self.value {
            NodeValue::Inline(text) | NodeValue::Block(text) => Some(text),
            NodeValue::None => None,
        }
    }
}

/// A node's value: nothing, a single-line value, or a dedented multi-line
/// block captured after a bare `=`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum NodeValue {
    /// A bare flag (`isHidden`) or a nameless header (`calculationGroup`).
    #[default]
    None,
    /// `key: value`, `key = value`, or `descriptor name = inline`.
    Inline(String),
    /// A multi-line block following `key =` / `descriptor name =`, dedented by
    /// its own first line's depth, blank lines inside preserved.
    Block(String),
}

/// Parses a TMDL document into its root nodes.
///
/// Every non-blank, non-`///` line becomes a node; nesting follows tab depth.
/// Fails only when a line cannot be tokenized at all.
fn parse_document(text: &str, path: &Path) -> Result<Vec<Node>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut builder = TreeBuilder {
        roots: Vec::new(),
        stack: Vec::new(),
        block: None,
    };
    builder.run(&lines, path)?;
    Ok(builder.roots)
}

/// Stack-based tree builder. The stack holds open nodes; a node is attached to
/// its parent when the parent closes (is popped), so children land in source
/// order. While a multi-line block is open, its lines are consumed raw and the
/// stack is guaranteed untouched, so the block's opener is always the top.
struct TreeBuilder {
    roots: Vec<Node>,
    stack: Vec<(usize, Node)>,
    block: Option<Block>,
}

/// An open multi-line block: the body's indent (its first line's depth), the
/// captured lines, and the value already written on the opener line — PBI
/// Desktop puts an expression's first line there when it continues below.
struct Block {
    indent: usize,
    body: Vec<String>,
    prefix: Option<String>,
}

impl TreeBuilder {
    fn run(&mut self, lines: &[&str], path: &Path) -> Result<()> {
        for (index, raw) in lines.iter().enumerate() {
            let depth = tab_depth(raw);

            let absorbed = match self.block.as_mut() {
                // Blank lines and lines at or below the body's indent belong
                // to the block; trailing blanks are trimmed on close.
                Some(block) if raw.trim().is_empty() => {
                    block.body.push(String::new());
                    true
                }
                Some(block) if depth >= block.indent => {
                    block.body.push(raw[block.indent..].to_string());
                    true
                }
                _ => false,
            };
            if absorbed {
                continue;
            }
            if self.block.is_some() {
                self.close_block();
            }

            let content = raw[depth..].trim_start();
            if content.is_empty() || content.starts_with("///") {
                continue;
            }

            let line = tokenize(content).map_err(|malformed| {
                Error::Tmdl(format!(
                    "{}: line {}: {malformed}",
                    path.display(),
                    index + 1
                ))
            })?;
            let node = Node {
                key: line.key.to_string(),
                name: line.name.map(str::to_string),
                tail: line.tail.map(str::to_string),
                value: match line.value {
                    Some(value) => NodeValue::Inline(value.to_string()),
                    None => NodeValue::None,
                },
                line: index + 1,
                children: Vec::new(),
            };

            if line.equals_form {
                // Two ways an `=` expression grows a block:
                //
                // 1. `key =` with nothing after it — the whole value is the
                //    following, deeper-indented block. Its first line fixes the
                //    block's indent, and the block ends at the first non-blank
                //    line above that, which is how a sibling property at
                //    depth+1 (a measure's formatString) closes the expression.
                //
                // 2. `key = start` where the expression continues below the
                //    property level (deeper than depth+1) — PBI Desktop
                //    serializes multi-line DAX this way when the first line
                //    belongs on the header (samples/…/Owners.tmdl).
                let value = line.value.unwrap_or_default();
                let continuation = !value.is_empty();
                let next = lines[index + 1..]
                    .iter()
                    .find(|probe| !probe.trim().is_empty());
                let opens = match next {
                    Some(next) if continuation => tab_depth(next) > depth + 1,
                    Some(next) => tab_depth(next) > depth,
                    None => false,
                };
                if opens && let Some(next) = next {
                    let indent = tab_depth(next);
                    let prefix = continuation.then(|| value.to_string());
                    self.open(node, depth);
                    self.block = Some(Block {
                        indent,
                        body: Vec::new(),
                        prefix,
                    });
                    continue;
                }
                // No block follows: the inline value (possibly empty) stands.
            }

            self.open(node, depth);
        }

        if self.block.is_some() {
            self.close_block();
        }
        while let Some((_, node)) = self.stack.pop() {
            self.attach(node);
        }
        Ok(())
    }

    /// Pushes a node at `depth`, closing everything at or above that depth.
    fn open(&mut self, node: Node, depth: usize) {
        while self.stack.last().is_some_and(|(open, _)| *open >= depth) {
            let Some((_, popped)) = self.stack.pop() else {
                break;
            };
            self.attach(popped);
        }
        self.stack.push((depth, node));
    }

    fn attach(&mut self, node: Node) {
        match self.stack.last_mut() {
            Some((_, parent)) => parent.children.push(node),
            None => self.roots.push(node),
        }
    }

    /// Finishes the open block, trimming trailing blank lines, and stores its
    /// body — prefixed by the opener line's own value, when it had one — as
    /// the opener's value.
    fn close_block(&mut self) {
        let Some(block) = self.block.take() else {
            return;
        };
        let mut body = block.body;
        while body.last().is_some_and(String::is_empty) {
            body.pop();
        }
        if let Some((_, node)) = self.stack.last_mut() {
            node.value = match block.prefix {
                Some(prefix) => NodeValue::Block(format!("{prefix}\n{}", body.join("\n"))),
                None => NodeValue::Block(body.join("\n")),
            };
        }
    }
}

/// Leading tab count. TMDL is tab-indented; spaces after the tabs belong to
/// the line's content.
fn tab_depth(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b'\t').count()
}

/// One tokenized line, borrowing the content it was parsed from.
struct Line<'a> {
    key: &'a str,
    name: Option<&'a str>,
    tail: Option<&'a str>,
    value: Option<&'a str>,
    /// Whether the value came after `=` (expressions, raw values) rather than
    /// `:` (single-line scalars) — only `=` values can grow blocks.
    equals_form: bool,
}

/// Tokenizes one line: `key`, optional `name` (quoted or bare), then a
/// `key: value` property, a `key = value` raw property or expression header, a
/// leftover tail (`ref table X`), or nothing (a flag).
fn tokenize(content: &str) -> std::result::Result<Line<'_>, String> {
    let key_end = content
        .find(|c: char| c.is_whitespace() || c == ':' || c == '=')
        .unwrap_or(content.len());
    let key = &content[..key_end];
    if key.is_empty() {
        return Err(format!("unparsable line {content:?}"));
    }
    let rest = content[key_end..].trim_start();

    let (name, rest) = read_name(rest);
    let rest = rest.trim_start();

    if let Some(value) = rest.strip_prefix(':') {
        return Ok(Line {
            key,
            name,
            tail: None,
            value: Some(value.trim_start()),
            equals_form: false,
        });
    }
    if let Some(value) = rest.strip_prefix('=') {
        return Ok(Line {
            key,
            name,
            tail: None,
            value: Some(value.trim_start()),
            equals_form: true,
        });
    }
    if !rest.is_empty() {
        return Ok(Line {
            key,
            name,
            tail: Some(rest.trim_end()),
            value: None,
            equals_form: false,
        });
    }
    Ok(Line {
        key,
        name,
        tail: None,
        value: None,
        equals_form: false,
    })
}

/// Reads the optional name after the key: a quoted identifier (kept with its
/// quotes, `''` escapes intact) or a bare word ending at whitespace, `:`, or
/// `=`.
fn read_name(rest: &str) -> (Option<&str>, &str) {
    if let Some(quoted) = rest.strip_prefix('\'') {
        let bytes = quoted.as_bytes();
        let mut cursor = 0;
        while cursor < quoted.len() {
            if bytes[cursor] == b'\'' {
                if bytes.get(cursor + 1) == Some(&b'\'') {
                    cursor += 2;
                    continue;
                }
                // `cursor` indexes the closing quote inside `quoted`; +2 in
                // `rest` covers that quote plus the opening one.
                let end = cursor + 2;
                return (Some(&rest[..end]), &rest[end..]);
            }
            cursor += 1;
        }
        // Unterminated quote: treat the whole rest as the name.
        return (Some(rest), "");
    }
    if rest.is_empty() || rest.starts_with(':') || rest.starts_with('=') {
        return (None, rest);
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || c == ':' || c == '=')
        .unwrap_or(rest.len());
    (Some(&rest[..end]), &rest[end..])
}

/// Rejects descriptors that lost their required name — broken TMDL, not drift.
///
/// A descriptor with a *value* is a property that happens to share the name
/// (`column: X` on a hierarchy level), so only flag-shaped nodes — no value,
/// no name — can be a header that lost its name.
fn validate(nodes: &[Node], path: &Path) -> Result<()> {
    for node in nodes {
        if NAMED_DESCRIPTORS.contains(&node.key.as_str())
            && node.name.is_none()
            && node.value == NodeValue::None
        {
            return Err(Error::Tmdl(format!(
                "{}: line {}: '{}' requires a name",
                path.display(),
                node.line,
                node.key
            )));
        }
        if node.key == "ref" && (node.name.is_none() || node.tail.is_none()) {
            return Err(Error::Tmdl(format!(
                "{}: line {}: ref requires a type and a target",
                path.display(),
                node.line
            )));
        }
        validate(&node.children, path)?;
    }
    Ok(())
}

// --- Stage 2: AST mapping ----------------------------------------------------

/// Records one skip.
fn notice(
    skips: &mut Vec<SkipNotice>,
    path: &Path,
    line: Option<usize>,
    kind: SkipKind,
    detail: String,
) {
    skips.push(SkipNotice {
        path: path.to_path_buf(),
        location: line.map(|line| format!("line {line}")),
        kind,
        detail,
    });
}

/// Whether a node's key is deliberately unmodeled metadata (Tier 1).
fn is_ignored(node: &Node) -> bool {
    IGNORED_KEYS.contains(&node.key.as_str())
}

/// Unquotes a TMDL identifier: `'Sales Order'` → `Sales Order`, `''` → `'`.
/// Unquoted names pass through unchanged.
fn unquote(name: &str) -> String {
    let Some(inner) = name.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) else {
        return name.to_string();
    };
    inner.replace("''", "'")
}

/// Parses a column reference: `Table.Column`, `'Quoted Table'.Column`,
/// `'Quoted Table'.'Quoted Column'`, or a bare (possibly quoted) `Column` —
/// the same-table form used by `sortByColumn`. Returns `(table, column)` with
/// the table absent for the bare form; `None` when nothing parses.
fn parse_column_ref(text: &str) -> Option<(Option<String>, String)> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('\'') {
        let bytes = rest.as_bytes();
        let mut cursor = 0;
        while cursor < rest.len() {
            if bytes[cursor] == b'\'' {
                if bytes.get(cursor + 1) == Some(&b'\'') {
                    cursor += 2;
                    continue;
                }
                let remainder = &rest[cursor + 1..];
                if !remainder.starts_with('.') {
                    // No member access after the quotes: a bare quoted name,
                    // the same-table form (sortByColumn and friends).
                    return (!rest[..cursor].is_empty()).then(|| (None, unquote(text)));
                }
                let table = rest[..cursor].replace("''", "'");
                return Some((Some(table), unquote(&remainder[1..])));
            }
            cursor += 1;
        }
        return None;
    }
    if let Some((table, column)) = text.split_once('.') {
        return Some((Some(table.to_string()), unquote(column)));
    }
    (!text.is_empty()).then(|| (None, unquote(text)))
}

/// Parses `true`/`false`; anything else is drift the caller notices.
fn parse_bool(value: Option<&str>) -> Option<bool> {
    match value {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

fn map_table(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> Table {
    let mut table = Table {
        name: unquote(node.name.as_deref().unwrap_or_default()),
        ..Default::default()
    };
    for child in &node.children {
        if is_ignored(child) {
            continue;
        }
        match child.key.as_str() {
            "measure" => table.measures.push(map_measure(child, path, skips)),
            "column" => table.columns.push(map_column(child, path, skips)),
            "hierarchy" => table.hierarchies.push(map_hierarchy(child, path, skips)),
            "partition" => table.partitions.push(map_partition(child, path, skips)),
            "calculationGroup" => {
                table.calculation_group = Some(map_calculation_group(child, path, skips));
            }
            "isHidden" => table.is_hidden = true,
            "defaultDetailRowsDefinition" => {
                table.detail_rows_expression = child.text().map(str::to_string);
            }
            other => notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!("unknown property '{other}' on table '{}'", table.name),
            ),
        }
    }
    table
}

fn map_measure(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> Measure {
    let name = unquote(node.name.as_deref().unwrap_or_default());
    if node.text().is_none() {
        notice(
            skips,
            path,
            Some(node.line),
            SkipKind::MalformedValue,
            format!("measure '{name}' has no expression"),
        );
    }
    let mut measure = Measure {
        name,
        expression: node.text().unwrap_or_default().to_string(),
        ..Default::default()
    };
    for child in &node.children {
        if is_ignored(child) {
            continue;
        }
        match child.key.as_str() {
            "isHidden" => measure.is_hidden = true,
            "formatStringDefinition" => {
                measure.format_string_expression = child.text().map(str::to_string);
            }
            "detailRowsDefinition" => {
                measure.detail_rows_expression = child.text().map(str::to_string);
            }
            "kpi" => measure.kpi = Some(map_kpi(child, path, skips)),
            other => notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!("unknown property '{other}' on measure '{}'", measure.name),
            ),
        }
    }
    measure
}

/// Maps a `kpi` block: `statusGraphic` plus the three TOM expression
/// properties. Spellings verified against the Analysis Services engine — the
/// short forms (`target =`, `status =`, `trend =`) are not valid TMDL.
fn map_kpi(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> Kpi {
    let mut kpi = Kpi::default();
    for child in &node.children {
        if is_ignored(child) {
            continue;
        }
        match child.key.as_str() {
            "targetExpression" => {
                kpi.target_expression = child.text().map(str::to_string);
            }
            "statusExpression" => {
                kpi.status_expression = child.text().map(str::to_string);
            }
            "trendExpression" => {
                kpi.trend_expression = child.text().map(str::to_string);
            }
            other => notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!("unknown property '{other}' on kpi"),
            ),
        }
    }
    kpi
}

fn map_column(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> Column {
    let kind = match node.text() {
        Some(expression) => ColumnKind::Calculated {
            expression: expression.to_string(),
        },
        None => ColumnKind::Data,
    };
    let mut column = Column {
        name: unquote(node.name.as_deref().unwrap_or_default()),
        kind,
        ..Default::default()
    };
    for child in &node.children {
        if is_ignored(child) {
            continue;
        }
        match child.key.as_str() {
            "isHidden" => column.is_hidden = true,
            "sortByColumn" => column.sort_by_column = child.text().map(unquote),
            // Group-by columns live inside a nameless relatedColumnDetails
            // object, one groupByColumn per grouped column (verified shape —
            // samples/…/Toggle for breakdown.tmdl).
            "relatedColumnDetails" => {
                for detail in &child.children {
                    if detail.key == "groupByColumn" {
                        if let Some(text) = detail.text() {
                            column.group_by_columns.push(unquote(text));
                        }
                    } else if !is_ignored(detail) {
                        notice(
                            skips,
                            path,
                            Some(detail.line),
                            SkipKind::UnknownProperty,
                            format!("unknown property '{}' on relatedColumnDetails", detail.key),
                        );
                    }
                }
            }
            other => notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!("unknown property '{other}' on column '{}'", column.name),
            ),
        }
    }
    column
}

fn map_hierarchy(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> Hierarchy {
    let mut hierarchy = Hierarchy {
        name: unquote(node.name.as_deref().unwrap_or_default()),
        ..Default::default()
    };
    for child in &node.children {
        if is_ignored(child) {
            continue;
        }
        match child.key.as_str() {
            "isHidden" => hierarchy.is_hidden = true,
            "level" => {
                let name = unquote(child.name.as_deref().unwrap_or_default());
                let column = child
                    .children
                    .iter()
                    .find(|sub| sub.key == "column")
                    .and_then(Node::text)
                    .map(unquote);
                let Some(column) = column else {
                    notice(
                        skips,
                        path,
                        Some(child.line),
                        SkipKind::MalformedValue,
                        format!("level '{name}' has no column"),
                    );
                    hierarchy.levels.push(HierarchyLevel {
                        name,
                        column: String::new(),
                    });
                    continue;
                };
                hierarchy.levels.push(HierarchyLevel { name, column });
            }
            other => notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!(
                    "unknown property '{other}' on hierarchy '{}'",
                    hierarchy.name
                ),
            ),
        }
    }
    hierarchy
}

fn map_partition(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> Partition {
    let name = unquote(node.name.as_deref().unwrap_or_default());
    let kind = node.text();
    let source = node
        .children
        .iter()
        .find(|child| child.key == "source")
        .and_then(Node::text);

    let expression = match (kind, source) {
        (Some("m" | "calculated" | "query"), Some(text)) => text.to_string(),
        (Some("m" | "calculated" | "query"), None) => {
            notice(
                skips,
                path,
                Some(node.line),
                SkipKind::MalformedValue,
                format!("partition '{name}' has no source"),
            );
            String::new()
        }
        _ => String::new(),
    };
    let source = match kind {
        Some("m") => PartitionSource::M { expression },
        Some("calculated") => PartitionSource::Calculated { expression },
        Some("query") => PartitionSource::Query { query: expression },
        Some(other) => {
            // entity (DirectLake), calculationGroup, future kinds: the
            // designated drift catch-all, kind string preserved.
            PartitionSource::Other {
                kind: Some(other.to_string()),
            }
        }
        None => {
            notice(
                skips,
                path,
                Some(node.line),
                SkipKind::MalformedValue,
                format!("partition '{name}' has no kind"),
            );
            PartitionSource::Other { kind: None }
        }
    };
    for child in &node.children {
        if is_ignored(child) || child.key == "source" {
            continue;
        }
        notice(
            skips,
            path,
            Some(child.line),
            SkipKind::UnknownProperty,
            format!(
                "unknown property '{child}' on partition '{name}'",
                child = child.key
            ),
        );
    }
    Partition { name, source }
}

fn map_calculation_group(
    node: &Node,
    path: &Path,
    skips: &mut Vec<SkipNotice>,
) -> CalculationGroup {
    let mut group = CalculationGroup::default();
    for child in &node.children {
        if is_ignored(child) {
            continue;
        }
        match child.key.as_str() {
            "calculationItem" => group.items.push(map_calculation_item(child, path, skips)),
            // Selection expressions are objects: the expression itself, with
            // an optional nested formatStringDefinition for its dynamic
            // format string. Standalone format-string spellings are not valid
            // TMDL (verified against the Analysis Services engine).
            "noSelectionExpression" => {
                group.no_selection_expression = child.text().map(str::to_string);
                group.no_selection_format_string_expression = nested_format_string(child);
            }
            "multipleOrEmptySelectionExpression" => {
                group.multiple_or_empty_selection_expression = child.text().map(str::to_string);
                group.multiple_or_empty_selection_format_string_expression =
                    nested_format_string(child);
            }
            other => notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!("unknown property '{other}' on calculation group"),
            ),
        }
    }
    group
}

/// A selection expression's nested `formatStringDefinition` child, if any.
fn nested_format_string(node: &Node) -> Option<String> {
    node.children
        .iter()
        .find(|child| child.key == "formatStringDefinition")
        .and_then(Node::text)
        .map(str::to_string)
}

fn map_calculation_item(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> CalculationItem {
    let name = unquote(node.name.as_deref().unwrap_or_default());
    if node.text().is_none() {
        notice(
            skips,
            path,
            Some(node.line),
            SkipKind::MalformedValue,
            format!("calculation item '{name}' has no expression"),
        );
    }
    let mut item = CalculationItem {
        name,
        expression: node.text().unwrap_or_default().to_string(),
        ..Default::default()
    };
    for child in &node.children {
        if is_ignored(child) {
            continue;
        }
        match child.key.as_str() {
            "formatStringDefinition" => {
                item.format_string_expression = child.text().map(str::to_string);
            }
            other => notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!(
                    "unknown property '{other}' on calculation item '{}'",
                    item.name
                ),
            ),
        }
    }
    item
}

/// Maps a relationship; `None` when its references are missing or unreadable,
/// with a notice already recorded. Active by default, matching TOM: TMDL omits
/// `isActive` for active relationships.
fn map_relationship(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> Option<Relationship> {
    let mut relationship = Relationship {
        name: node.name.as_deref().map(unquote),
        ..Default::default()
    };
    let mut from: Option<(Option<String>, String)> = None;
    let mut to: Option<(Option<String>, String)> = None;
    for child in &node.children {
        if is_ignored(child) {
            continue;
        }
        match child.key.as_str() {
            "fromColumn" => from = child.text().and_then(parse_column_ref),
            "toColumn" => to = child.text().and_then(parse_column_ref),
            "isActive" => match parse_bool(child.text()) {
                Some(value) => relationship.is_active = value,
                None => notice(
                    skips,
                    path,
                    Some(child.line),
                    SkipKind::MalformedValue,
                    format!(
                        "cannot parse isActive '{}'",
                        child.text().unwrap_or_default()
                    ),
                ),
            },
            other => notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!("unknown property '{other}' on relationship"),
            ),
        }
    }

    let (from, to) = match (from, to) {
        (Some(from), Some(to)) => (from, to),
        _ => {
            notice(
                skips,
                path,
                Some(node.line),
                SkipKind::MalformedValue,
                format!(
                    "relationship '{}' is missing or unreadable fromColumn/toColumn",
                    node.name.as_deref().unwrap_or_default()
                ),
            );
            return None;
        }
    };
    // Relationship references must be table-qualified.
    let (Some(from_table), from_column) = from else {
        notice(
            skips,
            path,
            Some(node.line),
            SkipKind::MalformedValue,
            "fromColumn is not table-qualified".to_string(),
        );
        return None;
    };
    let (Some(to_table), to_column) = to else {
        notice(
            skips,
            path,
            Some(node.line),
            SkipKind::MalformedValue,
            "toColumn is not table-qualified".to_string(),
        );
        return None;
    };
    relationship.from_table = from_table;
    relationship.from_column = from_column;
    relationship.to_table = to_table;
    relationship.to_column = to_column;
    Some(relationship)
}

fn map_role(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> Role {
    let mut role = Role {
        name: unquote(node.name.as_deref().unwrap_or_default()),
        ..Default::default()
    };
    for child in &node.children {
        if is_ignored(child) {
            continue;
        }
        match child.key.as_str() {
            // The filter is the `=` expression (inline or block); a
            // `filterExpression` child also loads — both spellings verified
            // against the Analysis Services engine.
            "tablePermission" => {
                let table = unquote(child.name.as_deref().unwrap_or_default());
                let filter_expression = child.text().map(str::to_string).or_else(|| {
                    child
                        .children
                        .iter()
                        .find(|sub| sub.key == "filterExpression")
                        .and_then(Node::text)
                        .map(str::to_string)
                });
                role.table_permissions.push(TablePermission {
                    table,
                    filter_expression,
                });
            }
            other => notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!("unknown property '{other}' on role '{}'", role.name),
            ),
        }
    }
    role
}

fn map_expression(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> SharedExpression {
    let name = unquote(node.name.as_deref().unwrap_or_default());
    if node.text().is_none() {
        notice(
            skips,
            path,
            Some(node.line),
            SkipKind::MalformedValue,
            format!("expression '{name}' has no body"),
        );
    }
    for child in &node.children {
        if !is_ignored(child) {
            notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!("unknown property '{}' on expression '{name}'", child.key),
            );
        }
    }
    SharedExpression {
        name,
        expression: node.text().unwrap_or_default().to_string(),
    }
}

fn map_function(node: &Node, path: &Path, skips: &mut Vec<SkipNotice>) -> Function {
    let name = unquote(node.name.as_deref().unwrap_or_default());
    if node.text().is_none() {
        notice(
            skips,
            path,
            Some(node.line),
            SkipKind::MalformedValue,
            format!("function '{name}' has no body"),
        );
    }
    let mut function = Function {
        name,
        expression: node.text().unwrap_or_default().to_string(),
        ..Default::default()
    };
    for child in &node.children {
        if is_ignored(child) {
            continue;
        }
        match child.key.as_str() {
            "isHidden" => function.is_hidden = true,
            other => notice(
                skips,
                path,
                Some(child.line),
                SkipKind::UnknownProperty,
                format!("unknown property '{other}' on function '{}'", function.name),
            ),
        }
    }
    function
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn parse(text: &str) -> Vec<Node> {
        parse_document(text, Path::new("test.tmdl")).expect("valid document")
    }

    fn map_one(text: &str, key: &str) -> Node {
        parse(text)
            .into_iter()
            .find(|node| node.key == key)
            .unwrap_or_else(|| panic!("no '{key}' root in document"))
    }

    fn key_at<'a>(node: &'a Node, key: &str) -> &'a Node {
        node.children
            .iter()
            .find(|child| child.key == key)
            .unwrap_or_else(|| panic!("no '{key}' child"))
    }

    mod scanner {
        use super::*;

        #[test]
        fn nests_children_by_tab_depth() {
            let roots = parse("table Sales\n\tmeasure M = 1\n\t\tisHidden\n\tcolumn C\n");
            assert_eq!(roots.len(), 1);
            let table = &roots[0];
            assert_eq!(table.key, "table");
            assert_eq!(table.name.as_deref(), Some("Sales"));
            assert_eq!(table.line, 1);
            assert_eq!(table.children.len(), 2);
            assert_eq!(table.children[0].key, "measure");
            assert_eq!(table.children[1].key, "column");
            let measure = &table.children[0];
            assert_eq!(measure.value, NodeValue::Inline("1".to_string()));
            assert_eq!(measure.children[0].key, "isHidden");
            assert_eq!(measure.children[0].value, NodeValue::None);
        }

        #[rstest]
        #[case("flag", None, NodeValue::None)]
        #[case("key: value", None, NodeValue::Inline("value".to_string()))]
        #[case("key = raw", None, NodeValue::Inline("raw".to_string()))]
        #[case("annotation N = v", Some("N"), NodeValue::Inline("v".to_string()))]
        fn reads_line_forms(
            #[case] text: &str,
            #[case] name: Option<&str>,
            #[case] value: NodeValue,
        ) {
            let roots = parse(text);
            assert_eq!(roots.len(), 1);
            assert_eq!(roots[0].name.as_deref(), name);
            assert_eq!(roots[0].value, value);
        }

        #[test]
        fn reads_ref_directives_with_their_target_as_tail() {
            let roots = parse("ref table 'Sales Order'\n");
            assert_eq!(roots[0].key, "ref");
            assert_eq!(roots[0].name.as_deref(), Some("table"));
            assert_eq!(roots[0].tail.as_deref(), Some("'Sales Order'"));
        }

        #[test]
        fn keeps_quoted_names_raw_with_escapes() {
            let roots = parse("measure 'It''s' = 1\n");
            assert_eq!(roots[0].name.as_deref(), Some("'It''s'"));
        }

        #[test]
        fn stops_names_at_equals_without_space() {
            let roots = parse("measure M= 1\n");
            assert_eq!(roots[0].name.as_deref(), Some("M"));
            assert_eq!(roots[0].value, NodeValue::Inline("1".to_string()));
        }

        #[test]
        fn captures_blocks_dedented_with_inner_blank_lines() {
            let roots = parse(
                "expression E =\n\t\tlet\n\t\t    x = 1\n\n\t\tin\n\t\t    x\n\nannotation A = 1\n",
            );
            assert_eq!(roots.len(), 2);
            assert_eq!(
                roots[0].value,
                NodeValue::Block("let\n    x = 1\n\nin\n    x".to_string())
            );
            assert_eq!(roots[1].key, "annotation");
        }

        #[test]
        fn closes_a_block_at_a_shallower_sibling_property() {
            // The expressions.tmdl shape: the block sits two levels deep, a
            // sibling property one level deep must not be swallowed.
            let roots = parse("expression E =\n\t\tbody\n\tlineageTag: t\n");
            assert_eq!(roots[0].value, NodeValue::Block("body".to_string()));
            assert_eq!(roots[0].children.len(), 1);
            assert_eq!(roots[0].children[0].key, "lineageTag");
        }

        #[test]
        fn closes_a_block_at_a_sibling_object_header() {
            let roots = parse("measure M =\n\t\tVAR x = 1\n\t\tRETURN x\nmeasure N = 2\n");
            assert_eq!(roots.len(), 2);
            assert_eq!(
                roots[0].value,
                NodeValue::Block("VAR x = 1\nRETURN x".to_string())
            );
            assert_eq!(roots[1].value, NodeValue::Inline("2".to_string()));
        }

        /// PBI Desktop serializes multi-line DAX with the first line on the
        /// header itself when the expression starts there; the rest continues
        /// below the property level (samples/…/Owners.tmdl). A property at
        /// depth+1 must close the expression, not join it.
        #[test]
        fn continues_an_inline_expression_below_property_depth() {
            let roots =
                parse("measure M = ```\n\t\t\tVAR x = 1\n\t\t\tRETURN x\n\t\tformatString: 0\n");
            assert_eq!(
                roots[0].value,
                NodeValue::Block("```\nVAR x = 1\nRETURN x".to_string())
            );
            assert_eq!(roots[0].children.len(), 1);
            assert_eq!(roots[0].children[0].key, "formatString");
        }

        /// A plain property at depth+1 after an inline value is a sibling, not
        /// a continuation — the expression stops at the header line.
        #[test]
        fn does_not_continue_an_inline_value_at_property_depth() {
            let roots = parse("measure M = 1 + 1\n\tformatString: 0\n");
            assert_eq!(roots[0].value, NodeValue::Inline("1 + 1".to_string()));
            assert_eq!(roots[0].children.len(), 1);
        }

        #[test]
        fn keeps_an_empty_value_when_equals_has_no_block() {
            let roots = parse("source =\nsource2 = x\n");
            assert_eq!(roots[0].value, NodeValue::Inline(String::new()));
        }

        #[test]
        fn rejects_unparsable_lines() {
            let error = parse_document("= broken\n", Path::new("test.tmdl"));
            assert!(error.is_err());
        }

        #[test]
        fn numbers_lines_from_one() {
            let roots = parse("\n\ntable Sales\n");
            assert_eq!(roots[0].line, 3);
        }
    }

    mod unquote {
        use super::*;

        #[rstest]
        #[case("Sales", "Sales")]
        #[case("'Sales Order'", "Sales Order")]
        #[case("'It''s quoted'", "It's quoted")]
        #[case("'A.B: C'", "A.B: C")]
        #[case("'", "'")]
        fn strips_quotes_and_unescapes(#[case] raw: &str, #[case] expected: &str) {
            assert_eq!(unquote(raw), expected);
        }
    }

    mod column_refs {
        use super::*;

        #[rstest]
        #[case("Sales.CustomerKey", Some((Some("Sales".into()), "CustomerKey".into())))]
        #[case(
            "'Sales Order'.SalesOrderLineKey",
            Some((Some("Sales Order".into()), "SalesOrderLineKey".into()))
        )]
        #[case(
            "'T'.'Col Name'",
            Some((Some("T".into()), "Col Name".into()))
        )]
        #[case(
            "Sales.'Order Quantity'",
            Some((Some("Sales".into()), "Order Quantity".into()))
        )]
        #[case("'Mth of year'", Some((None, "Mth of year".into())))]
        #[case("Plain", Some((None, "Plain".into())))]
        #[case("", None)]
        #[case("'Unterminated", None)]
        // Indistinguishable from the bare form — relationship mapping is what
        // rejects it, by requiring a table part.
        #[case("'TableOnly'", Some((None, "TableOnly".into())))]
        fn parses_written_forms(
            #[case] text: &str,
            #[case] expected: Option<(Option<String>, String)>,
        ) {
            assert_eq!(parse_column_ref(text), expected);
        }
    }

    mod validation {
        use super::*;

        #[test]
        fn rejects_a_table_without_a_name() {
            let error = validate(
                &[Node {
                    key: "table".to_string(),
                    ..Default::default()
                }],
                Path::new("test.tmdl"),
            );
            assert!(error.is_err());
        }

        #[test]
        fn rejects_an_incomplete_ref() {
            let error = validate(
                &[Node {
                    key: "ref".to_string(),
                    name: Some("table".to_string()),
                    ..Default::default()
                }],
                Path::new("test.tmdl"),
            );
            assert!(error.is_err());
        }

        #[test]
        fn accepts_nameless_calculation_groups() {
            let nodes = parse("calculationGroup\n\tprecedence: 1\n");
            assert!(validate(&nodes, Path::new("test.tmdl")).is_ok());
        }
    }

    mod mapping {
        use super::*;

        #[test]
        fn notices_an_unknown_property_with_its_line() {
            let mut skips = Vec::new();
            let node = map_one(
                "table Sales\n\tcolumn Amount\n\t\tdataType: double\n\t\taiHint: smart\n",
                "table",
            );
            let table = map_table(&node, Path::new("tables/Sales.tmdl"), &mut skips);

            assert_eq!(skips.len(), 1);
            assert_eq!(skips[0].kind, SkipKind::UnknownProperty);
            assert_eq!(skips[0].location.as_deref(), Some("line 4"));
            assert!(skips[0].detail.contains("aiHint"));
            assert_eq!(skips[0].path, Path::new("tables/Sales.tmdl"));
            assert_eq!(table.columns[0].name, "Amount");
        }

        #[test]
        fn ignores_deliberately_unmodeled_metadata_silently() {
            let mut skips = Vec::new();
            let node = map_one(
                "table Sales\n\tlineageTag: g\n\tisHidden\n\tcolumn Amount\n\t\tdataType: double\n\t\tformatString: 0\n\t\tsummarizeBy: sum\n\t\tsourceColumn: Amount\n\t\tchangedProperty = IsHidden\n\t\tannotation SetBy = User\n\tmeasure M = 1\n\t\tdisplayFolder: Core\n",
                "table",
            );
            let table = map_table(&node, Path::new("t"), &mut skips);

            assert!(
                skips.is_empty(),
                "unmodeled metadata must be silent: {skips:?}"
            );
            assert!(table.is_hidden);
            assert_eq!(table.columns.len(), 1);
            assert_eq!(table.measures.len(), 1);
        }

        #[test]
        fn maps_a_measure_with_kpi_and_dynamic_format_string() {
            let mut skips = Vec::new();
            let node = map_one(
                "measure 'Growth %' =\n\t\tVAR p = 1\n\t\tRETURN p\n\tisHidden\n\tformatStringDefinition = \"0%\"\n\tdetailRowsDefinition = DR\n\tkpi\n\t\tstatusGraphic: Three Traffic Lights\n\t\ttargetExpression = [B]\n\t\tstatusExpression = IF(1, 1, -1)\n\t\ttrendExpression = [T]\n",
                "measure",
            );
            let measure = map_measure(&node, Path::new("t"), &mut skips);

            assert!(skips.is_empty());
            assert!(measure.is_hidden);
            assert_eq!(measure.expression, "VAR p = 1\nRETURN p");
            assert_eq!(measure.format_string_expression.as_deref(), Some("\"0%\""));
            assert_eq!(measure.detail_rows_expression.as_deref(), Some("DR"));
            let kpi = measure.kpi.expect("kpi mapped");
            assert_eq!(kpi.target_expression.as_deref(), Some("[B]"));
            assert_eq!(kpi.status_expression.as_deref(), Some("IF(1, 1, -1)"));
            assert_eq!(kpi.trend_expression.as_deref(), Some("[T]"));
        }

        /// The short KPI spellings are not valid TMDL — the AS engine rejects
        /// `target =` — so they must surface as drift here, not map silently.
        #[test]
        fn notices_the_invalid_short_kpi_spellings() {
            let mut skips = Vec::new();
            let node = map_one("measure M = 1\n\tkpi\n\t\ttarget = [B]\n", "measure");
            let measure = map_measure(&node, Path::new("t"), &mut skips);

            let kpi = measure.kpi.expect("kpi block still maps");
            assert_eq!(kpi.target_expression, None);
            assert_eq!(skips.len(), 1);
            assert_eq!(skips[0].kind, SkipKind::UnknownProperty);
        }

        #[test]
        fn notices_a_measure_without_an_expression() {
            let mut skips = Vec::new();
            // `measure M` with no `=` at all: value None, noticed, kept empty.
            let node = map_one("measure M\n", "measure");
            let measure = map_measure(&node, Path::new("t"), &mut skips);

            assert_eq!(measure.expression, "");
            assert_eq!(skips.len(), 1);
            assert_eq!(skips[0].kind, SkipKind::MalformedValue);
        }

        #[test]
        fn maps_calculated_columns_sort_and_group_by() {
            let mut skips = Vec::new();
            let node = map_one(
                "column 'Margin %' = DIVIDE([Sales Amount], 10)\n\tsortByColumn: 'Sales Amount'\n\trelatedColumnDetails\n\t\tgroupByColumn: 'A'\n\t\tgroupByColumn: B\n",
                "column",
            );
            let column = map_column(&node, Path::new("t"), &mut skips);

            assert!(skips.is_empty());
            assert_eq!(
                column.kind,
                ColumnKind::Calculated {
                    expression: "DIVIDE([Sales Amount], 10)".to_string()
                }
            );
            assert_eq!(column.sort_by_column.as_deref(), Some("Sales Amount"));
            assert_eq!(column.group_by_columns, ["A", "B"]);
        }

        #[test]
        fn maps_hierarchies_with_quoted_level_columns() {
            let mut skips = Vec::new();
            let node = map_one(
                "hierarchy Fiscal\n\tlevel 'Month Name'\n\t\tcolumn: 'Mth of year'\n\tlevel Year\n\t\tcolumn: YearNum\n",
                "hierarchy",
            );
            let hierarchy = map_hierarchy(&node, Path::new("t"), &mut skips);

            assert!(skips.is_empty());
            assert_eq!(
                hierarchy.levels,
                [
                    HierarchyLevel {
                        name: "Month Name".to_string(),
                        column: "Mth of year".to_string(),
                    },
                    HierarchyLevel {
                        name: "Year".to_string(),
                        column: "YearNum".to_string(),
                    }
                ]
            );
        }

        #[test]
        fn notices_a_level_without_a_column() {
            let mut skips = Vec::new();
            let node = map_one("hierarchy H\n\tlevel Year\n", "hierarchy");
            let hierarchy = map_hierarchy(&node, Path::new("t"), &mut skips);

            assert_eq!(hierarchy.levels[0].column, "");
            assert_eq!(skips.len(), 1);
            assert_eq!(skips[0].kind, SkipKind::MalformedValue);
        }

        #[rstest]
        // (kind on the header, expected source)
        #[case::m("m", PartitionSource::M { expression: "let x = 1 in x".to_string() })]
        #[case::calculated(
            "calculated",
            PartitionSource::Calculated { expression: "TOPN(10, T)".to_string() }
        )]
        #[case::query("query", PartitionSource::Query { query: "SELECT 1".to_string() })]
        #[case::calculation_group(
            "calculationGroup",
            PartitionSource::Other { kind: Some("calculationGroup".to_string()) }
        )]
        #[case::entity(
            "entity",
            PartitionSource::Other { kind: Some("entity".to_string()) }
        )]
        fn maps_partition_kinds(#[case] kind: &str, #[case] expected: PartitionSource) {
            let mut skips = Vec::new();
            let text = match kind {
                "calculationGroup" | "entity" => format!("partition P = {kind}\n"),
                _ => format!(
                    "partition P = {kind}\n\tmode: import\n\tsource = {}\n",
                    match kind {
                        "m" => "let x = 1 in x",
                        "calculated" => "TOPN(10, T)",
                        _ => "SELECT 1",
                    }
                ),
            };
            let node = map_one(&text, "partition");
            let partition = map_partition(&node, Path::new("t"), &mut skips);

            assert!(skips.is_empty());
            assert_eq!(partition.source, expected);
        }

        #[test]
        fn notices_an_m_partition_without_a_source() {
            let mut skips = Vec::new();
            let node = map_one("partition P = m\n\tmode: import\n", "partition");
            let partition = map_partition(&node, Path::new("t"), &mut skips);

            assert_eq!(
                partition.source,
                PartitionSource::M {
                    expression: String::new()
                }
            );
            assert_eq!(skips.len(), 1);
            assert_eq!(skips[0].kind, SkipKind::MalformedValue);
        }

        #[test]
        fn maps_relationships_with_quoted_tables_and_defaults_active() {
            let mut skips = Vec::new();
            let node = map_one(
                "relationship guid-1\n\tfromColumn: Sales.Key\n\ttoColumn: 'Sales Order'.Key\n",
                "relationship",
            );
            let relationship = map_relationship(&node, Path::new("t"), &mut skips).expect("mapped");

            assert!(skips.is_empty());
            assert!(relationship.is_active);
            assert_eq!(relationship.from_table, "Sales");
            assert_eq!(relationship.to_table, "Sales Order");
            assert_eq!(relationship.name.as_deref(), Some("guid-1"));
        }

        #[test]
        fn maps_inactive_relationships_and_ignores_unmodeled_flags() {
            let mut skips = Vec::new();
            let node = map_one(
                "relationship g\n\tisActive: false\n\tcrossFilteringBehavior: bothDirections\n\tfromColumn: A.X\n\ttoColumn: B.Y\n",
                "relationship",
            );
            let relationship = map_relationship(&node, Path::new("t"), &mut skips).expect("mapped");

            assert!(skips.is_empty());
            assert!(!relationship.is_active);
        }

        #[test]
        fn drops_a_relationship_with_missing_references() {
            let mut skips = Vec::new();
            let node = map_one("relationship g\n\tfromColumn: A.X\n", "relationship");

            assert!(map_relationship(&node, Path::new("t"), &mut skips).is_none());
            assert_eq!(skips.len(), 1);
            assert_eq!(skips[0].kind, SkipKind::MalformedValue);
        }

        #[test]
        fn notices_an_unparseable_is_active() {
            let mut skips = Vec::new();
            let node = map_one(
                "relationship g\n\tisActive: maybe\n\tfromColumn: A.X\n\ttoColumn: B.Y\n",
                "relationship",
            );
            let relationship = map_relationship(&node, Path::new("t"), &mut skips).expect("mapped");

            assert!(
                relationship.is_active,
                "malformed value keeps the TOM default"
            );
            assert_eq!(skips.len(), 1);
            assert_eq!(skips[0].kind, SkipKind::MalformedValue);
        }

        #[test]
        fn maps_roles_with_block_and_metadata_only_permissions() {
            let mut skips = Vec::new();
            let node = map_one(
                "role Admin\n\tmodelPermission: read\n\ttablePermission Sales =\n\t\tFILTER('Sales', [Amount] > 0)\n\ttablePermission 'Sales Order'\n",
                "role",
            );
            let role = map_role(&node, Path::new("t"), &mut skips);

            assert!(skips.is_empty());
            assert_eq!(role.name, "Admin");
            assert_eq!(
                role.table_permissions,
                [
                    TablePermission {
                        table: "Sales".to_string(),
                        filter_expression: Some("FILTER('Sales', [Amount] > 0)".to_string()),
                    },
                    TablePermission {
                        table: "Sales Order".to_string(),
                        filter_expression: None,
                    },
                ]
            );
        }

        #[test]
        fn maps_calculation_groups_with_selection_expressions() {
            let mut skips = Vec::new();
            // Selection expressions carry their dynamic format strings as a
            // nested formatStringDefinition child (verified against the AS
            // engine; the standalone spellings are not valid TMDL).
            let node = map_one(
                "calculationGroup\n\tprecedence: 100\n\tcalculationItem Current = SELECTEDMEASURE()\n\tcalculationItem 'YoY %' = 1\n\t\tformatStringDefinition = \"0%\"\n\tnoSelectionExpression = SELECTEDMEASURE()\n\t\tformatStringDefinition = F()\n\tmultipleOrEmptySelectionExpression = ERROR(\"x\")\n\t\tformatStringDefinition = \"G\"\n",
                "calculationGroup",
            );
            let group = map_calculation_group(&node, Path::new("t"), &mut skips);

            assert!(skips.is_empty());
            assert_eq!(group.items.len(), 2);
            assert_eq!(
                group.items[1].format_string_expression.as_deref(),
                Some("\"0%\"")
            );
            assert_eq!(
                group.no_selection_expression.as_deref(),
                Some("SELECTEDMEASURE()")
            );
            assert_eq!(
                group.no_selection_format_string_expression.as_deref(),
                Some("F()")
            );
            assert_eq!(
                group.multiple_or_empty_selection_expression.as_deref(),
                Some("ERROR(\"x\")")
            );
            assert_eq!(
                group
                    .multiple_or_empty_selection_format_string_expression
                    .as_deref(),
                Some("\"G\"")
            );
        }
    }

    mod ignore_list {
        use super::*;

        /// Every key on the ignore list must actually be silent under the
        /// object that carries it — a typo in the list would surface as noise.
        #[test]
        fn silences_a_column_carrying_all_universal_metadata() {
            let mut skips = Vec::new();
            let text = "column C\n".to_string()
                + &[
                    "dataType",
                    "formatString",
                    "summarizeBy",
                    "sourceColumn",
                    "dataCategory",
                    "isKey",
                    "isNameInferred",
                    "isDataTypeInferred",
                    "isUnique",
                    "isDefaultLabel",
                    "isDefaultImage",
                    "isAvailableInMdx",
                    "keepUniqueRows",
                    "lineageTag",
                    "tableDetailPosition",
                ]
                .iter()
                .map(|key| format!("\t{key}: x\n"))
                .collect::<String>()
                + "\tvariation V\n\t\tdefaultHierarchy: H\n\t\tisDefault\n\tshowAsVariationsOnly\n";
            let node = map_one(&text, "column");
            map_column(&node, Path::new("t"), &mut skips);

            assert!(skips.is_empty(), "metadata keys must be silent: {skips:?}");
            // The child scan still finds modeled keys among the noise.
            assert!(key_at(&node, "dataType").value == NodeValue::Inline("x".to_string()));
        }
    }
}
