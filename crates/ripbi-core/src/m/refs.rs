//! Reference extraction over the M token stream.
//!
//! M names objects in three shapes, and extraction covers each:
//!
//! - **Field access** — `[Amount]`, `#"Sales"[Amount]`, `each [Amount]`. A
//!   bracket group is a field access exactly when its contents are one
//!   *generalized identifier* (identifiers, dots, digits, quoted identifiers —
//!   no operators); a group like `[K = 1]` is a record literal and names
//!   nothing.
//! - **Names** — bare and quoted identifiers. In M most bare words are local
//!   variables, but the conservatism rule from the DAX side applies unchanged:
//!   a word that collides with a table or shared-expression name can only mark
//!   an object used that truly is reachable, never the reverse. Dotted
//!   identifiers additionally emit their dot-separated parts, preserving the
//!   old substring matcher's `Server`-inside-`Server.Name` over-marking.
//! - **Column strings** — the string arguments of Power Query's column-centric
//!   built-ins: `"Amount"` in `Table.ExpandTableColumn(Source, "Amount")`.
//!   Harvesting is keyed on a curated whitelist of function names, so the
//!   `"Active"` in `Table.SelectRows(…, [Status] = "Active")` is never
//!   mistaken for a column.
//!
//! Strings and comments never yield references by themselves: a name inside a
//! comment or an unrelated string literal is not a use.

use std::borrow::Cow;
use std::ops::Range;

use super::lexer::{Token, TokenKind, tokenize};

/// One reference found in an M expression, exactly as written.
///
/// Names are raw source slices: delimiters are stripped, but quote escapes are
/// left intact (`#"It""s"` yields `"It""s"`). [`unescape_name`] produces the
/// logical names resolution expects.
///
/// Extraction is purely syntactic and knows nothing about any model: whether a
/// name is a table, a shared expression, or a local `let` variable is decided
/// by resolution, not here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RawRef<'a> {
    /// A column reference: `#"Sales"[Amount]`, `Source[Amount]`, or an
    /// unqualified `[Amount]`. `table` is `None` exactly when the reference was
    /// written unqualified — including `each [Amount]` row context.
    Field {
        /// Qualifying name as written, delimiters stripped.
        table: Option<&'a str>,
        /// The generalized identifier between the brackets, as written.
        name: &'a str,
        /// Byte range of the bracketed name, brackets included.
        span: Range<usize>,
    },
    /// A bare or quoted identifier: conservatively a table or shared-expression
    /// use. Most are local variables that resolve to nothing, which is data.
    Name {
        /// The name as written, delimiters stripped.
        name: &'a str,
        /// Byte range of the name.
        span: Range<usize>,
    },
    /// A string literal in the argument list of a column-centric built-in:
    /// `"Amount"` in `Table.SelectColumns(Source, "Amount")`.
    ColumnString {
        /// The string's content, delimiters stripped and escapes intact.
        name: &'a str,
        /// Byte range of the whole string literal.
        span: Range<usize>,
    },
}

/// Every reference in an M expression, in source order.
///
/// Comments, numbers, and unrelated strings never produce references;
/// malformed input yields the references it can, never an error.
///
/// ```
/// use ripbi_core::m::{RawRef, references};
///
/// let refs = references(r#"Table.ExpandTableColumn(Source, "Amount", {"Beløb"})"#);
/// // The harvested column strings come first, then the call name's
/// // candidates, then the walked names.
/// assert_eq!(
///     refs[0],
///     RawRef::ColumnString { name: "Amount", span: 32..40 }
/// );
/// assert_eq!(
///     refs[5],
///     RawRef::Name { name: "Source", span: 24..30 }
/// );
/// ```
#[must_use]
pub fn references(text: &str) -> Vec<RawRef<'_>> {
    let tokens = tokenize(text);
    extract(text, &tokens)
}

/// Produces the logical name of a raw slice: a `#"…"` wrapper is stripped and
/// doubled-quote escapes are resolved. Borrows when there is nothing to strip.
///
/// ```
/// use ripbi_core::m::unescape_name;
///
/// assert_eq!(unescape_name("Amount").as_ref(), "Amount");
/// assert_eq!(unescape_name("#\"1998 Sales\"").as_ref(), "1998 Sales");
/// assert_eq!(unescape_name("It\"\"s").as_ref(), "It\"s");
/// ```
#[must_use]
pub fn unescape_name(name: &str) -> Cow<'_, str> {
    if !name.starts_with("#\"") && !name.contains("\"\"") {
        return Cow::Borrowed(name);
    }
    let stripped = name
        .strip_prefix("#\"")
        .map_or(name, |inner| inner.strip_suffix('"').unwrap_or(inner));
    Cow::Owned(stripped.replace("\"\"", "\""))
}

/// The M keywords. A bare keyword is never a qualifier or a name candidate —
/// `each [Amount]` is row context, not a table named `each`. The comparison is
/// case-sensitive: M keywords are lowercase, and `Each` is a legal identifier
/// that stays a (harmless) candidate.
const KEYWORDS: [&str; 21] = [
    "and",
    "as",
    "each",
    "else",
    "error",
    "false",
    "if",
    "in",
    "is",
    "let",
    "meta",
    "not",
    "null",
    "or",
    "otherwise",
    "section",
    "shared",
    "then",
    "true",
    "try",
    "type",
];

/// The column-centric built-ins whose string arguments name columns, sorted for
/// [`binary_search`]. Curated by hand against real model corpora: partition M
/// is dominated by these, while value-centric built-ins (`Table.SelectRows`,
/// `Text.From`, …) take data as strings too and are deliberately absent — only
/// a real column reference may keep a column alive. `Table.ReplaceValue` is
/// the deliberate judgment call: most of its arguments are data values, but
/// its trailing column list is not, and a missed column reference is the one
/// direction this module must never err in.
const COLUMN_STRING_FUNCTIONS: [&str; 26] = [
    "#table",
    "record.field",
    "record.fieldordefault",
    "table.addcolumn",
    "table.addindexcolumn",
    "table.combinecolumns",
    "table.column",
    "table.duplicatecolumn",
    "table.expandlistcolumn",
    "table.expandtablecolumn",
    "table.filldown",
    "table.group",
    "table.join",
    "table.nestedjoin",
    "table.pivot",
    "table.removecolumns",
    "table.reordercolumns",
    "table.replaceerrorvalues",
    "table.replacevalue",
    "table.selectcolumns",
    "table.sort",
    "table.splitcolumn",
    "table.transformcolumns",
    "table.transformcolumntypes",
    "table.unpivot",
    "table.unpivotothercolumns",
];

fn is_keyword(name: &str) -> bool {
    KEYWORDS.contains(&name)
}

fn is_column_string_function(name: &str) -> bool {
    let folded = name.to_lowercase();
    COLUMN_STRING_FUNCTIONS
        .binary_search(&folded.as_str())
        .is_ok()
}

fn extract<'a>(text: &'a str, tokens: &[Token<'a>]) -> Vec<RawRef<'a>> {
    let mut out = Vec::new();
    let mut index = 0usize;

    while index < tokens.len() {
        let token = &tokens[index];
        match token.kind {
            TokenKind::Identifier => match tokens.get(index + 1).map(|t| t.kind) {
                // A call. Column-string functions harvest their string
                // arguments; every call is then walked into normally, so field
                // accesses and nested calls inside the arguments still count.
                Some(TokenKind::OpenParen) => {
                    if is_column_string_function(token.text) {
                        harvest_column_strings(tokens, index + 1, &mut out);
                    }
                    // The called name is itself a candidate: shared
                    // expressions frequently hold user-defined functions
                    // (`fnEasterSunday(year)`), and skipping call names would
                    // be the one direction this module must never err in.
                    // Built-ins resolve to nothing, so the cost is only
                    // over-keeping.
                    if !is_keyword(token.text) {
                        emit_names(token, &mut out);
                    }
                    index += 1;
                }
                // Name[Field] — a qualified field access. `#shared[Name]` is
                // the one qualifier that is not a table: its pieces are the
                // section's query names.
                Some(TokenKind::OpenBracket) if !is_keyword(token.text) => {
                    if token.text.eq_ignore_ascii_case("#shared") {
                        index = shared_members(tokens, index + 1, &mut out);
                    } else {
                        index = bracket_field(text, tokens, index + 1, Some(token.text), &mut out);
                    }
                }
                _ => {
                    if !is_keyword(token.text) {
                        emit_names(token, &mut out);
                    }
                    index += 1;
                }
            },
            TokenKind::QuotedIdentifier => {
                let inner = quoted_inner(token.text);
                if tokens.get(index + 1).map(|t| t.kind) == Some(TokenKind::OpenBracket) {
                    index = bracket_field(text, tokens, index + 1, Some(inner), &mut out);
                } else {
                    out.push(RawRef::Name {
                        name: inner,
                        span: token.start..token.end(),
                    });
                    index += 1;
                }
            }
            // [Field] — unqualified field access, or a record literal, which
            // names nothing.
            TokenKind::OpenBracket => {
                index = bracket_field(text, tokens, index, None, &mut out);
            }
            _ => index += 1,
        }
    }

    out
}

/// Emits a bare identifier as a name candidate, plus its dot-separated parts:
/// `Server.Name` keeps both `Server.Name` and `Server` alive, preserving the
/// substring matcher's deliberate over-marking.
fn emit_names<'a>(token: &Token<'a>, out: &mut Vec<RawRef<'a>>) {
    out.push(RawRef::Name {
        name: token.text,
        span: token.start..token.end(),
    });
    if !token.text.contains('.') {
        return;
    }
    let mut offset = token.start;
    for part in token.text.split('.') {
        if !part.is_empty() {
            out.push(RawRef::Name {
                name: part,
                span: offset..offset + part.len(),
            });
        }
        offset += part.len() + 1; // the dot
    }
}

/// Classifies the bracket group whose `[` sits at `start` and emits a field
/// reference when its contents are a generalized identifier. Returns the index
/// the walker continues from: past the `]` of a field access, or onto the
/// first token inside when the group is a record literal — its contents are
/// then walked as plain tokens, so strings inside a whitelisted call's record
/// arguments still get harvested.
fn bracket_field<'a>(
    text: &'a str,
    tokens: &[Token<'a>],
    start: usize,
    table: Option<&'a str>,
    out: &mut Vec<RawRef<'a>>,
) -> usize {
    if tokens.get(start).is_none() {
        return start;
    }

    let mut depth = 0usize;
    let mut generalized = true;
    let mut inner: Vec<&Token<'a>> = Vec::new();
    let mut close = None;

    for (offset, token) in tokens[start..].iter().enumerate() {
        match token.kind {
            TokenKind::OpenBracket => {
                depth += 1;
                if depth > 1 {
                    generalized = false;
                }
            }
            TokenKind::CloseBracket => {
                if depth == 0 {
                    // A stray `]` before the group ever opened: malformed in a
                    // way no field access explains.
                    generalized = false;
                } else {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(start + offset);
                        break;
                    }
                    generalized = false;
                }
            }
            TokenKind::Identifier
            | TokenKind::Dot
            | TokenKind::Number
            | TokenKind::QuotedIdentifier
                if depth == 1 =>
            {
                inner.push(token);
            }
            TokenKind::Comment => {}
            _ if depth >= 1 => generalized = false,
            _ => {}
        }
    }

    match close {
        // Unterminated: walk the contents conservatively instead of guessing.
        None => start + 1,
        // `[]`, the empty record, names nothing.
        Some(close) if inner.is_empty() => close + 1,
        Some(close) => {
            if generalized {
                let first = inner[0];
                let last = inner[inner.len() - 1];
                out.push(RawRef::Field {
                    table,
                    name: &text[first.start..last.end()],
                    span: tokens[start].start..tokens[close].end(),
                });
            }
            close + 1
        }
    }
}

/// Emits the bracket group whose `[` sits at `start` as name candidates — the
/// pieces of a `#shared[…]` section lookup, which are query names rather than
/// fields of a table. Returns the index the walker continues from.
fn shared_members<'a>(tokens: &[Token<'a>], start: usize, out: &mut Vec<RawRef<'a>>) -> usize {
    let mut depth = 0usize;
    for (offset, token) in tokens[start..].iter().enumerate() {
        match token.kind {
            TokenKind::OpenBracket => depth += 1,
            TokenKind::CloseBracket => {
                depth -= 1;
                if depth == 0 {
                    return start + offset + 1;
                }
            }
            TokenKind::Identifier if depth == 1 => out.push(RawRef::Name {
                name: token.text,
                span: token.start..token.end(),
            }),
            TokenKind::QuotedIdentifier if depth == 1 => out.push(RawRef::Name {
                name: quoted_inner(token.text),
                span: token.start..token.end(),
            }),
            _ => {}
        }
    }
    start
}

/// Emits every string literal between the call's `(` at `open` and its matching
/// `)` as column-string candidates. Nesting is tracked over all three bracket
/// kinds, so names inside pair lists like `{{"Amount", type text}}` count; an
/// unterminated call harvests to the end of the input.
fn harvest_column_strings<'a>(tokens: &[Token<'a>], open: usize, out: &mut Vec<RawRef<'a>>) {
    let mut depth = 0usize;
    for token in &tokens[open..] {
        match token.kind {
            TokenKind::OpenParen | TokenKind::OpenBracket | TokenKind::OpenBrace => depth += 1,
            TokenKind::CloseParen | TokenKind::CloseBracket | TokenKind::CloseBrace => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            TokenKind::String => out.push(RawRef::ColumnString {
                name: string_inner(token.text),
                span: token.start..token.end(),
            }),
            _ => {}
        }
    }
}

/// The name inside a quoted-identifier token, delimiters stripped. Unterminated
/// input has no closing delimiter; the opening one is still dropped.
fn quoted_inner(text: &str) -> &str {
    // Drop the leading #"; strip the closing quote only when it is there —
    // `""` at the end is an escape and stays.
    text[2..].strip_suffix('"').unwrap_or(&text[2..])
}

/// The content inside a string-literal token, delimiters stripped. Unterminated
/// input has no closing delimiter; the opening one is still dropped.
fn string_inner(text: &str) -> &str {
    text[1..].strip_suffix('"').unwrap_or(&text[1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(text: &str) -> Vec<RawRef<'_>> {
        references(text)
    }

    /// The qualified/unqualified shape of every `Field` ref, as `(table, name)`.
    fn fields(text: &str) -> Vec<(Option<&str>, &str)> {
        refs(text)
            .into_iter()
            .filter_map(|r| match r {
                RawRef::Field { table, name, .. } => Some((table, name)),
                _ => None,
            })
            .collect()
    }

    /// The names of every `Name` ref.
    fn names(text: &str) -> Vec<&str> {
        refs(text)
            .into_iter()
            .filter_map(|r| match r {
                RawRef::Name { name, .. } => Some(name),
                _ => None,
            })
            .collect()
    }

    /// The names of every `ColumnString` ref.
    fn column_strings(text: &str) -> Vec<&str> {
        refs(text)
            .into_iter()
            .filter_map(|r| match r {
                RawRef::ColumnString { name, .. } => Some(name),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn finds_the_issue_three_reference_forms() {
        // The shapes issue #39 names as the gap.
        let expand = r#"Table.ExpandTableColumn(Source, "Old", {"A", "B"})"#;
        assert_eq!(column_strings(expand), ["Old", "A", "B"]);

        assert_eq!(fields(r#"#"Sales"[Amount]"#), [(Some("Sales"), "Amount")]);
        assert_eq!(fields("[Amount]"), [(None, "Amount")]);

        let join = r#"Table.NestedJoin(A, "Key1", B, "Key2", "New")"#;
        assert_eq!(column_strings(join), ["Key1", "Key2", "New"]);
    }

    #[test]
    fn spans_cover_the_whole_reference() {
        let text = "#\"Sales Header\"[Net Price]";
        let found = refs(text);
        let RawRef::Field { span, .. } = &found[0] else {
            panic!("expected a field ref");
        };
        assert_eq!(&text[span.clone()], "[Net Price]");
    }

    #[test]
    fn every_span_is_a_valid_source_subslice() {
        let text = concat!(
            "let\n",
            "    Source = #\"My Table\"[X],\n",
            "    Typed = Table.TransformColumnTypes(Source, {{\"Amount\", type text}}),\n",
            "    Filtered = Table.SelectRows(Typed, each [Amount] > 0)\n",
            "in\n",
            "    Filtered",
        );
        for found in refs(text) {
            let span = match found {
                RawRef::Field { span, .. }
                | RawRef::Name { span, .. }
                | RawRef::ColumnString { span, .. } => span,
            };
            assert!(!span.is_empty());
            assert!(
                text.get(span.clone()).is_some(),
                "span {span:?} must be inside the source"
            );
        }
    }

    #[test]
    fn unqualified_field_access_in_each_row_context_is_a_field() {
        assert_eq!(
            fields("Table.SelectRows(Source, each [Amount] > 0)"),
            [(None, "Amount")]
        );
        // The comparison string is a value, not a column: SelectRows is not a
        // column-string function.
        assert_eq!(
            column_strings("Table.SelectRows(Source, each [Amount] > 0)"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_record_literal_is_not_a_field_access() {
        // The options record of Sql.Database names keys, not columns.
        assert!(fields(r#"Sql.Database("srv", "db", [CommandTimeout = 30])"#).is_empty());
        // But a real field access right next to it still counts.
        let fields_found = fields("[Amount] + [K = 1][Nope]");
        assert_eq!(fields_found[0], (None, "Amount"));
        assert!(
            !fields_found.contains(&(None, "K")),
            "the record literal [K = 1] must not yield a field for its key"
        );
        // `[K = 1][Nope]` is a field access *on the record*; the walker cannot
        // tell that from a column access, so it conservatively keeps `Nope` —
        // the same over-marking every bare word gets.
    }

    #[test]
    fn generalized_identifiers_keep_blanks_and_dots() {
        assert_eq!(fields("[Base Line]"), [(None, "Base Line")]);
        assert_eq!(fields("[A. B]"), [(None, "A. B")]);
        assert_eq!(fields("[1998 Sales]"), [(None, "1998 Sales")]);
        assert_eq!(fields(r#"[#"It""s"]"#), [(None, "#\"It\"\"s\"")]);
    }

    #[test]
    fn keywords_are_never_qualifiers_or_names() {
        // `each [Amount]` is row context, not a table named each.
        assert_eq!(fields("each [Amount]"), [(None, "Amount")]);
        assert!(names("each [Amount]").is_empty(), "`each` is a keyword");
        // Keywords still walk into the bracket group they introduce.
        assert_eq!(fields("if [X] then 1 else 2"), [(None, "X")]);
    }

    #[test]
    fn bare_identifiers_are_conservative_name_candidates() {
        assert_eq!(
            names("let Source = Sql.Database(ServerName) in Source"),
            [
                "Source",
                "Sql.Database",
                "Sql",
                "Database",
                "ServerName",
                "Source",
            ],
            "every bare word and call name is a candidate — built-ins resolve to nothing"
        );
    }

    #[test]
    fn dotted_identifiers_emit_their_parts() {
        // `Server.Name` must keep `Server` alive, as the substring matcher
        // deliberately did.
        assert!(names("Sql.Database(Server.Name)").contains(&"Server"));
        assert!(names("Sql.Database(Server.Name)").contains(&"Server.Name"));
    }

    #[test]
    fn quoted_identifiers_are_name_candidates() {
        assert_eq!(names("#\"My Query\""), ["My Query"]);
        // #shared[Name] names a query, not a field of a table.
        assert_eq!(names("#shared[#\"My Query\"]"), ["My Query"]);
        assert!(fields("#shared[#\"My Query\"]").is_empty());
    }

    #[test]
    fn harvesting_reaches_inside_pair_lists_and_nested_calls() {
        let text = concat!(
            "Table.Group(Source, {\"Key\"}, {{\"All\", ",
            "each Table.TransformColumnTypes(_, {{\"Amount\", type text}})}})",
        );
        // The outer call harvests everything inside itself — including the
        // aggregation's new-column name — and the nested whitelisted call
        // harvests its own arguments again. Duplicates are fine: they collapse
        // into one graph edge.
        assert_eq!(column_strings(text), ["Key", "All", "Amount", "Amount"]);
    }

    #[test]
    fn value_strings_outside_column_functions_are_not_columns() {
        let text = concat!(
            "Table.SelectRows(Source, each [Status] = \"Active\")\n",
            "& Text.From(123) & \"Amount\"",
        );
        assert!(column_strings(text).is_empty());
        assert_eq!(fields(text), [(None, "Status")]);
    }

    #[test]
    fn references_inside_strings_and_comments_do_not_count() {
        let text = concat!(
            "\"[In String] [X]\"\n",
            "// [In Comment] [Y]\n",
            "/* [Block] [Comment] */\n",
            "[Real]",
        );
        assert_eq!(fields(text), [(None, "Real")]);
        assert!(names(text).is_empty());
        assert!(column_strings(text).is_empty());
    }

    #[test]
    fn escaped_names_carry_raw_slices() {
        let found = refs("#\"It\"\"s\"[X]");
        match &found[0] {
            RawRef::Field {
                table: Some(table),
                name,
                ..
            } => {
                assert_eq!(*table, "It\"\"s");
                assert_eq!(*name, "X");
            }
            other => panic!("expected a qualified field ref, got {other:?}"),
        }
        // ...and unescaping produces the logical name.
        assert_eq!(unescape_name("It\"\"s").as_ref(), "It\"s");
    }

    #[test]
    fn the_full_let_query_yields_every_kind_of_reference() {
        let text = concat!(
            "let\n",
            "    Source = Sql.Database(ServerName, \"db\"),\n",
            "    Staging = #\"My Staging\"[Amount],\n",
            "    Typed = Table.TransformColumnTypes(Staging, {{\"Amount\", type text}, {\"Beløb\", Currency.Type}}),\n",
            "    Filtered = Table.SelectRows(Typed, each [Region] = \"West\"),\n",
            "    Joined = Table.NestedJoin(Filtered, {\"Key\"}, DimTable, {\"Key\"}, \"Dim\")\n",
            "in\n",
            "    Joined",
        );
        // "db" is a database name and "West" a filter value: Sql.Database and
        // Table.SelectRows are not column-string functions, so neither counts.
        assert_eq!(
            column_strings(text),
            ["Amount", "Beløb", "Key", "Key", "Dim"]
        );
        assert!(fields(text).contains(&(Some("My Staging"), "Amount")));
        assert!(fields(text).contains(&(None, "Region")));
        assert!(names(text).contains(&"DimTable"));
        assert!(names(text).contains(&"ServerName"));
    }

    #[test]
    fn unterminated_input_still_yields_conservative_refs() {
        // `#"Sales [X` never closes: one quoted identifier that names nothing
        // as a field.
        assert!(fields("#\"Sales [X").is_empty());
        // `[Col` without a closing bracket is never swallowed; its contents
        // are walked as plain tokens instead of guessed at.
        assert!(fields("[Col").is_empty());
        // An unterminated call harvests everything it can see.
        assert_eq!(column_strings("Table.SelectColumns(Source, {\"A\""), ["A"]);
    }

    #[test]
    fn empty_input_has_no_references() {
        assert!(refs("").is_empty());
        assert!(refs("   \n\t  ").is_empty());
    }

    #[test]
    fn column_string_function_matching_is_case_insensitive() {
        assert_eq!(
            column_strings("table.selectcolumns(Source, \"A\")"),
            ["A"],
            "matching a built-in must not depend on its casing"
        );
    }

    #[test]
    fn unescape_name_handles_wrapper_and_escapes() {
        assert!(matches!(unescape_name("Plain"), Cow::Borrowed("Plain")));
        assert_eq!(unescape_name("#\"A B\"").as_ref(), "A B");
        assert_eq!(unescape_name("A\"\"B").as_ref(), "A\"B");
        assert_eq!(unescape_name("#\"A\"\"B\"").as_ref(), "A\"B");
        // Unterminated wrappers degrade gracefully.
        assert_eq!(unescape_name("#\"A B").as_ref(), "A B");
    }
}
