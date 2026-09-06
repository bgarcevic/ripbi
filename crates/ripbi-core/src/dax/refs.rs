//! Reference extraction: adjacency rules over the DAX token stream.
//!
//! A reference in DAX is never a single token — `'Sales'[Amount]` is a quoted
//! table followed by a bracketed name — so extraction walks the token stream and
//! merges adjacent tokens by shape. The tokenizer emits no whitespace tokens, so
//! "adjacent" means exactly "the next token in the stream".

use std::borrow::Cow;
use std::ops::Range;

use super::lexer::{Token, TokenKind, tokenize};
use crate::identity::{FieldRef, NameKey};

/// One object reference found in an expression, exactly as written.
///
/// Names are raw source slices: delimiters are stripped, but quote escapes are
/// left intact (`'It''s'` yields `"It''s"`). [`unescape_name`] and
/// [`RawRef::to_field_ref`] produce the logical names that [`FieldRef`] and the
/// rest of the identity layer expect.
///
/// Extraction is purely syntactic and knows nothing about any model: whether a
/// bracketed name is a column or a measure, and whether an identifier names a
/// real table or function at all, is decided by resolution, not here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RawRef<'a> {
    /// A column or measure reference: `'Table'[Name]`, `Table[Name]`, or an
    /// unqualified `[Name]`. `table` is `None` exactly when the reference was
    /// written unqualified.
    Field {
        /// Qualifying table name as written, delimiters stripped.
        table: Option<&'a str>,
        /// The name inside the brackets, as written.
        name: &'a str,
        /// Byte range of the whole reference, qualifier included.
        span: Range<usize>,
    },
    /// A bare table use: `COUNTROWS(Sales)`, `EVALUATE Sales`. Detected so that a
    /// table referenced only this way is never reported unused.
    Table {
        /// The table name as written, delimiters stripped.
        name: &'a str,
        /// Byte range of the name.
        span: Range<usize>,
    },
    /// A function call: an identifier followed by `(`. Whether this is a
    /// user-defined function of the model (a real dependency) or a built-in
    /// (`SUM`, `COUNTROWS`, …) is decided by resolution.
    Function {
        /// The function name as written.
        name: &'a str,
        /// Byte range of the name.
        span: Range<usize>,
    },
}

/// Every reference in a DAX expression, in source order.
///
/// Strings, comments, numbers, `dt"…"` literals, and `@parameters` never produce
/// references; malformed input yields the references it can, never an error.
///
/// ```
/// use ripbi_core::dax::RawRef;
///
/// let refs = ripbi_core::dax::references("DIVIDE([Sales Amount], 10)");
/// // The DIVIDE call is a function candidate; [Sales Amount] is the field ref.
/// assert_eq!(
///     refs[1],
///     RawRef::Field { table: None, name: "Sales Amount", span: 7..21 }
/// );
/// ```
#[must_use]
pub fn references(text: &str) -> Vec<RawRef<'_>> {
    extract(&tokenize(text))
}

/// Strips the doubled-quote escapes from a name as written inside single quotes:
/// `"It''s"` → `"It's"`. Borrows when there is nothing to unescape.
///
/// Bracketed names need no unescaping: `]` cannot appear in an Analysis
/// Services object name, so a bracketed slice is already the logical name.
#[must_use]
pub fn unescape_name(name: &str) -> Cow<'_, str> {
    if name.contains("''") {
        Cow::Owned(name.replace("''", "'"))
    } else {
        Cow::Borrowed(name)
    }
}

impl RawRef<'_> {
    /// The logical [`FieldRef`] of a [`RawRef::Field`] — delimiters stripped and
    /// quote escapes resolved, per the identity layer's producer contract.
    /// `None` for [`RawRef::Table`] and [`RawRef::Function`], which name no
    /// column or measure.
    #[must_use]
    pub fn to_field_ref(&self) -> Option<FieldRef> {
        let RawRef::Field { table, name, .. } = self else {
            return None;
        };
        Some(FieldRef {
            table: table.map(|table| NameKey::new(unescape_name(table).as_ref())),
            name: NameKey::new(unescape_name(name).as_ref()),
        })
    }
}

fn extract<'a>(tokens: &[Token<'a>]) -> Vec<RawRef<'a>> {
    let mut out = Vec::new();
    let mut index = 0usize;

    while index < tokens.len() {
        let token = &tokens[index];
        match token.kind {
            TokenKind::QuotedTable => {
                let name = quoted_inner(token.text);
                // 'Table'[Name] — the qualifier merges with the bracketed name.
                if let Some(bracket) = bracket_at(tokens, index + 1) {
                    out.push(RawRef::Field {
                        table: Some(name),
                        name: quoted_inner(bracket.text),
                        span: token.start..bracket.end(),
                    });
                    index += 2;
                } else {
                    out.push(RawRef::Table {
                        name,
                        span: token.start..token.end(),
                    });
                    index += 1;
                }
            }
            TokenKind::Identifier => {
                match tokens.get(index + 1).map(|t| t.kind) {
                    // Sales[Name] — an unquoted table qualifier.
                    Some(TokenKind::BracketName) => {
                        let bracket = &tokens[index + 1];
                        out.push(RawRef::Field {
                            table: Some(token.text),
                            name: quoted_inner(bracket.text),
                            span: token.start..bracket.end(),
                        });
                        index += 2;
                    }
                    // A call — a user-defined function candidate. Built-ins
                    // resolve to nothing and stay behind as unresolved data.
                    Some(TokenKind::OpenParen) => {
                        out.push(RawRef::Function {
                            name: token.text,
                            span: token.start..token.end(),
                        });
                        index += 1;
                    }
                    // A bare name — conservatively a table use. A variable or
                    // keyword that collides with a table name can only mark an
                    // object used that truly is reachable; never the reverse.
                    _ => {
                        out.push(RawRef::Table {
                            name: token.text,
                            span: token.start..token.end(),
                        });
                        index += 1;
                    }
                }
            }
            // [Name] — unqualified: a measure, or a column of the row context.
            TokenKind::BracketName => {
                out.push(RawRef::Field {
                    table: None,
                    name: quoted_inner(token.text),
                    span: token.start..token.end(),
                });
                index += 1;
            }
            _ => index += 1,
        }
    }

    out
}

/// The bracketed token directly after `index`, if there is one.
fn bracket_at<'a, 'b>(tokens: &'a [Token<'b>], index: usize) -> Option<&'a Token<'b>> {
    let token = tokens.get(index)?;
    (token.kind == TokenKind::BracketName).then_some(token)
}

/// The name inside a quoted-table or bracketed-name token, delimiters stripped.
/// Unterminated input has no closing delimiter; the opening one is still dropped.
fn quoted_inner(text: &str) -> &str {
    let bytes = text.as_bytes();
    let closed = text.len() >= 2
        && match bytes[0] {
            b'\'' => bytes[text.len() - 1] == b'\'',
            b'[' => bytes[text.len() - 1] == b']',
            _ => false,
        };
    if closed {
        &text[1..text.len() - 1]
    } else {
        &text[1..]
    }
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

    /// The names of every `Table` ref.
    fn tables(text: &str) -> Vec<&str> {
        refs(text)
            .into_iter()
            .filter_map(|r| match r {
                RawRef::Table { name, .. } => Some(name),
                _ => None,
            })
            .collect()
    }

    /// The names of every `Function` ref.
    fn functions(text: &str) -> Vec<&str> {
        refs(text)
            .into_iter()
            .filter_map(|r| match r {
                RawRef::Function { name, .. } => Some(name),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn finds_the_three_reference_forms() {
        assert_eq!(
            fields("'Sales Header'[Net Price] + Sales[Amount] - [Total]"),
            [
                (Some("Sales Header"), "Net Price"),
                (Some("Sales"), "Amount"),
                (None, "Total"),
            ]
        );
    }

    #[test]
    fn spans_cover_the_whole_reference() {
        let text = "SUM('Sales Header'[Net Price])";
        let found = refs(text);
        assert_eq!(found.len(), 2, "the SUM call plus its one argument");
        let RawRef::Field { span, .. } = &found[1] else {
            panic!("the second ref is the qualified field");
        };
        assert_eq!(&text[span.clone()], "'Sales Header'[Net Price]");
    }

    #[test]
    fn every_span_is_a_valid_source_subslice() {
        let text = "'It''s'[X] + COUNTROWS(Sales) + [Total] * MyFunc([Amount])";
        for found in refs(text) {
            let span = match found {
                RawRef::Field { span, .. }
                | RawRef::Table { span, .. }
                | RawRef::Function { span, .. } => span,
            };
            assert!(!span.is_empty());
            assert!(
                text.get(span.clone()).is_some(),
                "span {span:?} must be inside the source"
            );
        }
    }

    #[test]
    fn whitespace_between_qualifier_and_name_still_qualifies() {
        assert_eq!(
            fields("'Sales' [Amount] + Dato [Måned]"),
            [(Some("Sales"), "Amount"), (Some("Dato"), "Måned"),]
        );
    }

    #[test]
    fn quoted_tables_with_escaped_quotes_carry_raw_names() {
        let found = refs("'It''s'[X]");
        assert_eq!(found.len(), 1);
        match &found[0] {
            RawRef::Field {
                table: Some(table),
                name,
                ..
            } => {
                assert_eq!(*table, "It''s");
                assert_eq!(*name, "X");
            }
            other => panic!("expected a qualified field ref, got {other:?}"),
        }
        // ...and unescaping produces the logical name.
        let field = found[0].to_field_ref().expect("field ref");
        assert_eq!(field.table.as_ref().map(NameKey::as_str), Some("It's"));
        assert_eq!(field.name.as_str(), "X");
    }

    #[test]
    fn bare_table_uses_are_detected() {
        assert_eq!(
            tables("COUNTROWS(Sales) + COUNTROWS('Sales Order')"),
            ["Sales", "Sales Order"]
        );
        // Even the keyword is a candidate: it resolves to nothing against any
        // model, which is exactly the conservative direction.
        assert_eq!(tables("EVALUATE Sales"), ["EVALUATE", "Sales"]);
    }

    #[test]
    fn calls_become_function_candidates() {
        assert_eq!(functions("SUMX(VALUES(T), [M])"), ["SUMX", "VALUES"]);
        assert_eq!(fields("SUMX(VALUES(T), [M])"), [(None, "M"),]);
        assert_eq!(tables("SUMX(VALUES(T), [M])"), ["T"]);
    }

    #[test]
    fn a_reference_followed_by_a_call_is_not_a_call() {
        // [Total] ( … ) is the implicit-CALCULATE measure call form.
        assert!(functions("[Total]([Amount])").is_empty());
        assert_eq!(
            fields("[Total]([Amount])"),
            [(None, "Total"), (None, "Amount")]
        );
    }

    #[test]
    fn refs_inside_strings_and_comments_do_not_count() {
        let text = concat!(
            "\"[In String] 'Table'[X]\"\n",
            "// [In Line Comment] 'Q'[Y]\n",
            "-- [Dash Comment]\n",
            "/* [Block] [Comment] */\n",
            "dt\"[Date Literal]\"\n",
            "[Real]",
        );
        assert_eq!(fields(text), [(None, "Real")]);
        assert!(tables(text).is_empty());
        assert!(functions(text).is_empty());
    }

    #[test]
    fn comments_are_skipped_but_the_code_around_them_is_not() {
        assert_eq!(
            fields("[A] /* [B] */ [C] -- [D]\n[E] // [F]"),
            [(None, "A"), (None, "C"), (None, "E"),]
        );
    }

    #[test]
    fn the_tricky_sample_from_the_sqlbi_smoke_tests_yields_one_field_ref() {
        // Ported from SQLBI.Whiteboard.Core.SmokeTests: exactly one comment, the
        // string is not a comment, and the only field reference is
        // Sales[Amount]. The bare words (Tricky, VAR, Year, Note, RETURN) are
        // conservative table candidates — 8 of them — never field references.
        let source = concat!(
            "Tricky :=\n",
            "-- a real comment\n",
            "VAR Year = 2024\n",
            "VAR Note = \"-- not a comment\"\n",
            "RETURN Year & Note & Sales[Amount]\n",
        );
        assert_eq!(refs(source).len(), 9);
        assert_eq!(fields(source), [(Some("Sales"), "Amount")]);
        assert_eq!(tables(source).len(), 8);
        assert!(functions(source).is_empty());

        let tokens = crate::dax::lexer::tokenize(source);
        assert_eq!(
            tokens
                .iter()
                .filter(|t| t.kind == TokenKind::Comment)
                .count(),
            1,
            "the real comment, and only it, is a comment"
        );
    }

    #[test]
    fn the_sumx_definition_sample_yields_three_field_refs() {
        // Ingestion strips definition headers before the lexer sees an
        // expression, so the body alone yields the bare table plus two
        // qualified field references.
        assert_eq!(
            fields("SUMX ( Sales, Sales[Quantity] * Sales[Net Price] )"),
            [(Some("Sales"), "Quantity"), (Some("Sales"), "Net Price")]
        );
        assert_eq!(
            tables("SUMX ( Sales, Sales[Quantity] * Sales[Net Price] )"),
            ["Sales"]
        );

        // With the header included anyway, the lexer cannot know `[Sales
        // Amount]` names the object being defined — it is an unqualified field
        // reference like any other. Ownership is the AST's business, not the
        // lexer's.
        let header = "[Sales Amount] = SUMX ( Sales, Sales[Quantity] * Sales[Net Price] )";
        assert_eq!(
            fields(header),
            [
                (None, "Sales Amount"),
                (Some("Sales"), "Quantity"),
                (Some("Sales"), "Net Price"),
            ]
        );
    }

    #[test]
    fn function_bodies_keep_their_own_references() {
        // SELECTCOLUMNS('Sales', "Key", [SalesOrderLineKey]) — the string
        // "Key" must not become a reference; 'Sales' is a bare table use.
        let source = "SELECTCOLUMNS('Sales', \"Key\", [SalesOrderLineKey])";
        assert_eq!(fields(source), [(None, "SalesOrderLineKey")]);
        assert_eq!(tables(source), ["Sales"]);
    }

    #[test]
    fn hierarchy_level_syntax_lexes_as_independent_refs() {
        // Not valid in DAX model expressions, but must not corrupt extraction.
        assert_eq!(
            fields("Product[Category].[Subcategory]"),
            [(Some("Product"), "Category"), (None, "Subcategory"),]
        );
    }

    #[test]
    fn unterminated_tokens_still_produce_conservative_refs() {
        // `'Sales [Amount]` never closes, so everything is one quoted table —
        // an unresolved table use, never a lost reference.
        assert_eq!(tables("'Sales [Amount]"), ["Sales [Amount]"]);
        // `[Col` without a closing bracket is still an unqualified field.
        assert_eq!(fields("[Col"), [(None, "Col")]);
    }

    #[test]
    fn keywords_and_variables_are_conservative_table_candidates() {
        // TRUE resolves to nothing against any model; a variable named like a
        // table can only over-mark usage, which is the safe direction.
        assert_eq!(tables("TRUE && FALSE"), ["TRUE", "FALSE"]);
    }

    #[test]
    fn numbers_and_parameters_are_never_refs() {
        assert!(refs("1.5E+10 + @Risk + .5").is_empty());
    }

    #[test]
    fn empty_input_has_no_references() {
        assert!(refs("").is_empty());
        assert!(refs("   \n\t  ").is_empty());
    }

    #[test]
    fn unescape_name_borrows_without_escapes_and_resolves_with_them() {
        assert!(matches!(unescape_name("Plain"), Cow::Borrowed("Plain")));
        assert!(matches!(unescape_name("It''s"), Cow::Owned(_)));
        assert_eq!(unescape_name("It''s''''s").as_ref(), "It's''s");
        // Bracketed names never carry escapes to begin with.
        assert!(matches!(
            unescape_name("Net Price"),
            Cow::Borrowed("Net Price")
        ));
    }

    #[test]
    fn to_field_ref_is_none_for_table_and_function_refs() {
        let text = "COUNTROWS(Sales)";
        for found in &refs(text) {
            match found {
                RawRef::Table { .. } | RawRef::Function { .. } => {
                    assert!(found.to_field_ref().is_none());
                }
                RawRef::Field { .. } => assert!(found.to_field_ref().is_some()),
            }
        }
    }
}
