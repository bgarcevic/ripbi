//! Fabric apps as report consumers.
//!
//! A Fabric app (the `@microsoft/fabric-app-data` / Rayfin template) is a
//! TypeScript project that queries a semantic model with DAX it composes at
//! run time. It is recognized by the `fabric.yaml` at its root, which names
//! the models it connects to. Nothing in the project is a declarative binding,
//! so the app is read as text: every string literal in its JavaScript and
//! TypeScript source becomes a [`CodeLiteral`], and the graph decides which of
//! them name model objects.
//!
//! The lexer below only separates string literals from comments, regular
//! expressions, and code — it does not parse JavaScript. Its failure mode is
//! a missed or spurious literal, never an error: a quote it misreads (an
//! apostrophe in JSX text) ends at the line break, as unterminated JavaScript
//! strings do.

use std::fs;
use std::path::Path;

use super::{Ingested, SkipKind, SkipNotice};
use crate::Result;
use crate::report::{CodeLiteral, ReportModel};

/// The file that makes a folder a Fabric app.
const MANIFESTS: [&str; 2] = ["fabric.yaml", "fabric.yml"];

/// Source extensions read for literals.
const EXTENSIONS: [&str; 8] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// Folders that never hold the app's own runtime source: dependencies,
/// build output, and test suites. Dot-folders (`.git`, `.agents`, …) are
/// skipped too — agent skill kits there carry their own example queries.
const SKIPPED_DIRS: [&str; 7] = [
    "node_modules",
    "dist",
    "build",
    "out",
    "coverage",
    "__tests__",
    "__mocks__",
];

/// Whether `path` is a Fabric app folder: a directory with `fabric.yaml`.
pub(super) fn is_app(path: &Path) -> bool {
    path.is_dir() && MANIFESTS.iter().any(|name| path.join(name).is_file())
}

/// The semantic models an app's `fabric.yaml` names, across every profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppModels {
    /// The keys under `semanticModels:` — the app's own names for the models
    /// it reads, e.g. `sales` — deduplicated, in first-seen order.
    pub aliases: Vec<String>,
    /// The `itemId` GUIDs under those keys, lowercased and deduplicated.
    /// Placeholders (the nil GUID, `${…}` substitutions, anything not
    /// GUID-shaped) are left out: they name no model.
    pub item_ids: Vec<String>,
}

/// Reads the models an app's `fabric.yaml` names. Missing, unreadable, or
/// unexpected YAML yields what could be read — never an error: pairing
/// treats an empty result as unresolved.
pub(super) fn models(path: &Path) -> AppModels {
    let text = MANIFESTS
        .iter()
        .find_map(|name| fs::read_to_string(path.join(name)).ok())
        .unwrap_or_default();
    manifest_models(&text)
}

/// The block-mapping walk behind [`models`]. It reads only the YAML
/// `fabric.yaml` is written in — `key:` lines nested by indentation — and
/// collects the children of any `semanticModels:` key with their `itemId`s,
/// wherever it sits (top level or under a profile).
fn manifest_models(text: &str) -> AppModels {
    let mut found = AppModels::default();
    // The open keys, innermost last, with their indentation.
    let mut stack: Vec<(usize, String)> = Vec::new();
    for raw in text.lines() {
        let line = strip_comment(raw);
        let content = line.trim_start();
        if content.is_empty() || content.starts_with("---") {
            continue;
        }
        let mut indent = line.len() - content.len();
        let content = match content.strip_prefix("- ") {
            Some(rest) => {
                indent += 2;
                rest.trim_start()
            }
            None => content,
        };
        let Some((key, value)) = content.split_once(':') else {
            continue;
        };
        let key = unquote(key.trim());
        let value = unquote(value.trim());
        while stack.last().is_some_and(|(open, _)| *open >= indent) {
            stack.pop();
        }
        let parent = stack.last().map(|(_, key)| key.as_str());
        let grandparent = stack
            .len()
            .checked_sub(2)
            .map(|index| stack[index].1.as_str());
        if parent.is_some_and(|parent| parent.eq_ignore_ascii_case("semanticModels"))
            && !found.aliases.iter().any(|alias| alias == key)
        {
            found.aliases.push(key.to_string());
        }
        if grandparent.is_some_and(|key| key.eq_ignore_ascii_case("semanticModels"))
            && key.eq_ignore_ascii_case("itemId")
            && is_guid(value)
        {
            let id = value.to_ascii_lowercase();
            if !found.item_ids.contains(&id) {
                found.item_ids.push(id);
            }
        }
        if value.is_empty() {
            stack.push((indent, key.to_string()));
        }
    }
    found
}

/// `line` without a trailing `# comment` outside quotes.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut previous = ' ';
    for (index, c) in line.char_indices() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && previous.is_whitespace() => return &line[..index],
            None => {}
        }
        previous = c;
    }
    line
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(value)
}

/// A real item GUID: `8-4-4-4-12` hex digits, not the nil GUID templates
/// ship as a placeholder.
fn is_guid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.chars().all(|c| c.is_ascii_hexdigit()))
        && value.chars().any(|c| c != '0' && c != '-')
}

/// Reads a Fabric app folder into a [`ReportModel`] carrying only
/// [`ReportModel::code`].
pub(super) fn load(path: &Path) -> Result<Ingested<ReportModel>> {
    let mut skips = Vec::new();
    let mut files = Vec::new();
    collect_sources(path, &mut files)?;
    files.sort();
    let mut code = Vec::new();
    for file in &files {
        let relative = file
            .strip_prefix(path)
            .unwrap_or(file)
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        match fs::read(file) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => code.extend(literals(&text, &relative)),
                Err(_) => skips.push(SkipNotice {
                    path: file.clone(),
                    location: None,
                    kind: SkipKind::MalformedValue,
                    detail: "source file is not UTF-8; its literals were not read".to_string(),
                }),
            },
            Err(error) => skips.push(SkipNotice {
                path: file.clone(),
                location: None,
                kind: SkipKind::MalformedValue,
                detail: format!("source file could not be read: {error}"),
            }),
        }
    }
    let value = ReportModel {
        name: app_name(path),
        code,
        ..Default::default()
    };
    Ok(Ingested { value, skips })
}

fn collect_sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_ref()) {
                collect_sources(&path, out)?;
            }
        } else if kind.is_file() && is_source(&name) {
            out.push(path);
        }
    }
    Ok(())
}

/// A runtime source file: a JS/TS extension, not a declaration file or a
/// test.
fn is_source(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some((stem, extension)) = lower.rsplit_once('.') else {
        return false;
    };
    EXTENSIONS.contains(&extension)
        && !stem.ends_with(".d")
        && !stem.ends_with(".test")
        && !stem.ends_with(".spec")
}

/// The app's name: Rayfin's `rayfin/rayfin.yml` `name:`, else `package.json`
/// `name`, else the folder name.
fn app_name(path: &Path) -> Option<String> {
    let rayfin = fs::read_to_string(path.join("rayfin").join("rayfin.yml"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let value = line.strip_prefix("name:")?.trim();
                let value = value.trim_matches(|c| c == '"' || c == '\'');
                (!value.is_empty()).then(|| value.to_string())
            })
        });
    let package = || {
        let text = fs::read_to_string(path.join("package.json")).ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        json.get("name")?.as_str().map(str::to_string)
    };
    let folder = || {
        let canonical = fs::canonicalize(path).ok();
        canonical
            .as_deref()
            .unwrap_or(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    };
    rayfin.or_else(package).or_else(folder)
}

/// Every string literal in one JavaScript/TypeScript source, in file order.
///
/// Quoted strings yield one literal each; a template literal yields one per
/// text run between `${…}` interpolations (whose code is lexed in turn), and
/// only an interpolation-free template is [`whole`](CodeLiteral::whole).
/// Comments and regular-expression literals are skipped. Empty runs are
/// dropped.
pub(super) fn literals(source: &str, file: &str) -> Vec<CodeLiteral> {
    let mut lexer = Lexer {
        bytes: source.as_bytes(),
        source,
        file,
        i: 0,
        line: 1,
        out: Vec::new(),
    };
    lexer.run();
    lexer.out
}

struct Lexer<'a> {
    bytes: &'a [u8],
    source: &'a str,
    file: &'a str,
    i: usize,
    line: usize,
    out: Vec<CodeLiteral>,
}

impl Lexer<'_> {
    fn run(&mut self) {
        // Brace depth inside each open `${…}`, innermost last.
        let mut interpolations: Vec<usize> = Vec::new();
        // Whether a `/` here starts a regular expression rather than dividing.
        let mut regex_ok = true;
        while let Some(&byte) = self.bytes.get(self.i) {
            match byte {
                b'\n' => {
                    self.line += 1;
                    self.i += 1;
                }
                b'/' if self.bytes.get(self.i + 1) == Some(&b'/') => {
                    while self.bytes.get(self.i).is_some_and(|&b| b != b'\n') {
                        self.i += 1;
                    }
                }
                b'/' if self.bytes.get(self.i + 1) == Some(&b'*') => {
                    let end = self.source[self.i + 2..]
                        .find("*/")
                        .map_or(self.bytes.len(), |end| self.i + 2 + end + 2);
                    self.advance_to(end);
                }
                b'/' if regex_ok => {
                    self.regex();
                    regex_ok = false;
                }
                b'\'' | b'"' => {
                    self.quoted(byte);
                    regex_ok = false;
                }
                b'`' => {
                    self.i += 1;
                    if self.template(true) {
                        interpolations.push(0);
                        regex_ok = true;
                    } else {
                        regex_ok = false;
                    }
                }
                b'{' => {
                    if let Some(depth) = interpolations.last_mut() {
                        *depth += 1;
                    }
                    self.i += 1;
                    regex_ok = true;
                }
                b'}' => {
                    self.i += 1;
                    match interpolations.last_mut() {
                        Some(0) => {
                            interpolations.pop();
                            if self.template(false) {
                                interpolations.push(0);
                                regex_ok = true;
                            } else {
                                regex_ok = false;
                            }
                        }
                        Some(depth) => {
                            *depth -= 1;
                            regex_ok = true;
                        }
                        None => regex_ok = true,
                    }
                }
                b')' | b']' => {
                    self.i += 1;
                    regex_ok = false;
                }
                b if b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80 => {
                    let start = self.i;
                    while self.bytes.get(self.i).is_some_and(|&b| {
                        b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
                    }) {
                        self.i += 1;
                    }
                    regex_ok = matches!(
                        &self.source[start..self.i],
                        "return"
                            | "typeof"
                            | "case"
                            | "in"
                            | "of"
                            | "new"
                            | "delete"
                            | "void"
                            | "throw"
                            | "instanceof"
                            | "yield"
                            | "await"
                            | "else"
                            | "do"
                    );
                }
                b if b.is_ascii_whitespace() => self.i += 1,
                _ => {
                    self.i += 1;
                    regex_ok = true;
                }
            }
        }
    }

    /// Moves to `end`, counting the line breaks passed.
    fn advance_to(&mut self, end: usize) {
        let end = end.min(self.bytes.len());
        self.line += self.bytes[self.i..end]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        self.i = end;
    }

    /// A `'…'` or `"…"` string starting at `self.i`. An unterminated string
    /// ends at the line break and yields nothing.
    fn quoted(&mut self, quote: u8) {
        let line = self.line;
        self.i += 1;
        let mut text = String::new();
        loop {
            match self.bytes.get(self.i) {
                None | Some(b'\n') => return,
                Some(&b) if b == quote => {
                    self.i += 1;
                    self.push(line, text, true);
                    return;
                }
                Some(b'\\') => self.escape(&mut text),
                Some(_) => self.char_into(&mut text),
            }
        }
    }

    /// One template text run, starting just after a backtick (`open`) or
    /// after an interpolation's closing brace. Returns whether it stopped at
    /// a `${`, leaving the lexer inside an interpolation.
    fn template(&mut self, open: bool) -> bool {
        let line = self.line;
        let mut text = String::new();
        loop {
            match self.bytes.get(self.i) {
                None => {
                    self.push(line, text, false);
                    return false;
                }
                Some(b'`') => {
                    self.i += 1;
                    self.push(line, text, open);
                    return false;
                }
                Some(b'$') if self.bytes.get(self.i + 1) == Some(&b'{') => {
                    self.i += 2;
                    self.push(line, text, false);
                    return true;
                }
                Some(b'\\') => self.escape(&mut text),
                Some(b'\n') => {
                    self.line += 1;
                    self.i += 1;
                    text.push('\n');
                }
                Some(_) => self.char_into(&mut text),
            }
        }
    }

    /// A regular-expression literal starting at `self.i`. One that does not
    /// close on its line was a division after all: only the `/` is consumed.
    fn regex(&mut self) {
        let mut j = self.i + 1;
        let mut class = false;
        loop {
            match self.bytes.get(j) {
                None | Some(b'\n') => {
                    self.i += 1;
                    return;
                }
                Some(b'\\') => j += 2,
                Some(b'[') => {
                    class = true;
                    j += 1;
                }
                Some(b']') => {
                    class = false;
                    j += 1;
                }
                Some(b'/') if !class => break,
                Some(_) => j += 1,
            }
        }
        j += 1;
        while self.bytes.get(j).is_some_and(u8::is_ascii_alphabetic) {
            j += 1;
        }
        self.i = j;
    }

    /// Decodes the escape sequence at `self.i` (a backslash) into `text`.
    fn escape(&mut self, text: &mut String) {
        self.i += 1;
        let Some(&b) = self.bytes.get(self.i) else {
            return;
        };
        let simple = match b {
            b'n' => Some('\n'),
            b't' => Some('\t'),
            b'r' => Some('\r'),
            b'b' => Some('\u{8}'),
            b'f' => Some('\u{c}'),
            b'v' => Some('\u{b}'),
            b'0' if !self.bytes.get(self.i + 1).is_some_and(u8::is_ascii_digit) => Some('\0'),
            _ => None,
        };
        if let Some(c) = simple {
            text.push(c);
            self.i += 1;
            return;
        }
        match b {
            // A line continuation contributes nothing.
            b'\n' => {
                self.line += 1;
                self.i += 1;
            }
            b'\r' => {
                self.i += 1;
                if self.bytes.get(self.i) == Some(&b'\n') {
                    self.line += 1;
                    self.i += 1;
                }
            }
            b'x' => self.hex(text, 1, 2),
            b'u' if self.bytes.get(self.i + 1) == Some(&b'{') => {
                let close = self.source[self.i..].find('}').map(|p| self.i + p);
                let code = close
                    .and_then(|close| u32::from_str_radix(&self.source[self.i + 2..close], 16).ok())
                    .and_then(char::from_u32);
                match (code, close) {
                    (Some(c), Some(close)) => {
                        text.push(c);
                        self.i = close + 1;
                    }
                    _ => self.char_into(text),
                }
            }
            b'u' => self.hex(text, 1, 4),
            _ => self.char_into(text),
        }
    }

    /// A fixed-width hex escape: `width` digits after `skip` bytes. A
    /// malformed one is kept as its letter, as browsers did before ES2018.
    fn hex(&mut self, text: &mut String, skip: usize, width: usize) {
        let start = self.i + skip;
        let code = self
            .source
            .get(start..start + width)
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            .and_then(char::from_u32);
        match code {
            Some(c) => {
                text.push(c);
                self.i = start + width;
            }
            None => self.char_into(text),
        }
    }

    /// Copies the character at `self.i` into `text`.
    fn char_into(&mut self, text: &mut String) {
        let c = self.source[self.i..].chars().next().unwrap_or('\u{fffd}');
        text.push(c);
        self.i += c.len_utf8().max(1);
    }

    fn push(&mut self, line: usize, text: String, whole: bool) {
        if !text.is_empty() {
            self.out.push(CodeLiteral {
                file: self.file.to_string(),
                line,
                text,
                whole,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AppModels, literals, manifest_models};

    #[test]
    fn manifest_models_span_profiles_and_skip_placeholder_ids() {
        let yaml = "\
activeProfile: DEV # the local one
profiles:
  DEV:
    semanticModels:
      sales:
        workspaceId: 00000000-0000-0000-0000-000000000000
        itemId: 00000000-0000-0000-0000-000000000000
  TEST:
    semanticModels:
      sales:
        itemId: '${SALES_ITEM_ID}'
      \"hr\":
        itemId: 11111111-2222-3333-4444-555555555555
  PROD:
    semanticModels:
      sales:
        workspaceId: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee
        itemId: ABCDEF01-2345-6789-ABCD-EF0123456789
";
        assert_eq!(
            manifest_models(yaml),
            AppModels {
                aliases: vec!["sales".to_string(), "hr".to_string()],
                item_ids: vec![
                    "11111111-2222-3333-4444-555555555555".to_string(),
                    "abcdef01-2345-6789-abcd-ef0123456789".to_string(),
                ],
            }
        );
    }

    #[test]
    fn manifest_models_read_a_top_level_section() {
        let yaml = "semanticModels:\n  finance:\n    itemId: 11111111-2222-3333-4444-555555555555\nother:\n  semantic: x\n";
        let found = manifest_models(yaml);
        assert_eq!(found.aliases, ["finance"]);
        assert_eq!(found.item_ids, ["11111111-2222-3333-4444-555555555555"]);
    }

    #[test]
    fn manifest_without_models_reads_empty() {
        assert_eq!(manifest_models("name: app\n"), AppModels::default());
        assert_eq!(manifest_models(""), AppModels::default());
    }

    fn texts(source: &str) -> Vec<(String, bool)> {
        literals(source, "a.ts")
            .into_iter()
            .map(|literal| (literal.text, literal.whole))
            .collect()
    }

    #[test]
    fn quoted_strings_decode_escapes() {
        assert_eq!(
            texts(r#"const a = 'P&L Rows'; const b = "it\"s"; const c = 'Bob\'s!';"#),
            [
                ("P&L Rows".to_string(), true),
                ("it\"s".to_string(), true),
                ("Bob's!".to_string(), true),
            ]
        );
    }

    #[test]
    fn templates_split_at_interpolations() {
        let source = "const q = `EVALUATE ${t}[${c}]`; const w = `'Sales'[Amount]`;";
        assert_eq!(
            texts(source),
            [
                ("EVALUATE ".to_string(), false),
                ("[".to_string(), false),
                ("]".to_string(), false),
                ("'Sales'[Amount]".to_string(), true),
            ]
        );
    }

    #[test]
    fn interpolations_lex_their_own_strings_and_nested_templates() {
        let source = "`a${ f({ k: 'X' }) }b${ `in${y}ner` }c`";
        assert_eq!(
            texts(source),
            [
                ("a".to_string(), false),
                ("X".to_string(), true),
                ("b".to_string(), false),
                ("in".to_string(), false),
                ("ner".to_string(), false),
                ("c".to_string(), false),
            ]
        );
    }

    #[test]
    fn comments_and_regexes_are_skipped() {
        let source = "// 'Date'[Month]\n/* [Total] */ s.replace(/'/g, \"''\"); x = a / b / 'c';";
        assert_eq!(
            texts(source),
            [("''".to_string(), true), ("c".to_string(), true)]
        );
    }

    #[test]
    fn an_unterminated_quote_ends_at_the_line_break() {
        let source = "<p>Don't panic</p>\nconst t = 'Sales';";
        let found = literals(source, "a.tsx");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "Sales");
        assert_eq!(found[0].line, 2);
    }

    #[test]
    fn lines_count_through_multiline_templates() {
        let source = "const q = `EVALUATE\n  'T'[C]`;\nconst n = 'N';";
        let found = literals(source, "a.ts");
        assert_eq!(found[0].line, 1);
        assert_eq!(found[1].line, 3);
    }
}
