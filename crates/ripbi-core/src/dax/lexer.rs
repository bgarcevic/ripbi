//! DAX tokenizer: a Rust port of SQLBI Whiteboard's `DaxLexer` (MIT), reduced to
//! what reference extraction needs. See [`crate::dax`] for the full provenance
//! and for what was deliberately dropped.
//!
//! Like the original, the scanner is tolerance-first: unterminated strings,
//! quotes, and brackets never fail — they simply run to the end of the input.
//! Lexing is a total function; there is nothing to report as an error.

/// What a [`Token`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// End of input. Always the last token; `text` is empty.
    Eof,
    /// `Sales`, `MyVar`, `NORM.DIST` — internal dots are absorbed.
    Identifier,
    /// `'Sales Header'` — a single-quoted table name, `''` as the escape.
    QuotedTable,
    /// `[Net Price]` — a bracketed column, measure, or hierarchy-level name.
    /// The lexer cannot tell the three apart; resolution does.
    BracketName,
    /// `"text"` — a string literal, `""` as the escape.
    String,
    /// `dt"2024-01-01"` — a date-time literal.
    DateTime,
    /// `1`, `1.5`, `1.5E+10`.
    Number,
    /// `@Risk` — a query parameter (DAX queries, not model expressions).
    QueryParameter,
    /// `// …`, `-- …`, or `/* … */` — never produces a reference.
    Comment,
    /// `+ - * / ^ & = == <> < > <= >= && || ! => :=`
    Operator,
    /// `(`
    OpenParen,
    /// `)`
    CloseParen,
    /// `{`
    OpenBrace,
    /// `}`
    CloseBrace,
    /// `,`
    Comma,
    /// `;`
    Semicolon,
    /// `:`
    Colon,
    /// `.`
    Dot,
    /// Any other character. Never fails the scan.
    Unknown,
}

/// One lexical token: [`kind`](Token::kind), the source [`slice`](Token::text)
/// it covers, and the byte offset it starts at.
///
/// The text is borrowed from the expression — lexing allocates nothing but the
/// returned `Vec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token<'a> {
    /// What the token is.
    pub kind: TokenKind,
    /// The exact source text, delimiters and escapes included.
    pub text: &'a str,
    /// Byte offset of the token in the expression.
    pub start: usize,
}

impl Token<'_> {
    /// The byte offset one past the token's last byte.
    #[must_use]
    pub fn end(&self) -> usize {
        self.start + self.text.len()
    }
}

/// Two-character operators, checked before single characters.
const TWO_CHAR_OPERATORS: [&str; 8] = ["=>", ":=", "==", "<>", ">=", "<=", "&&", "||"];

/// Converts DAX source into tokens, ending with one [`TokenKind::Eof`].
///
/// Whitespace produces no tokens, so the token after any given token is exactly
/// its next significant neighbour — the property the reference extractor's
/// adjacency rules rely on.
///
/// ```
/// use ripbi_core::dax::{TokenKind, tokenize};
///
/// let tokens = tokenize("SUM(Sales[Amount])");
/// let kinds: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
/// assert_eq!(
///     kinds,
///     [
///         TokenKind::Identifier,
///         TokenKind::OpenParen,
///         TokenKind::Identifier,
///         TokenKind::BracketName,
///         TokenKind::CloseParen,
///         TokenKind::Eof,
///     ]
/// );
/// ```
#[must_use]
pub fn tokenize(source: &str) -> Vec<Token<'_>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;

    while index < source.len() {
        let start = index;
        // `index` is always on a char boundary: every advance below moves past a
        // whole character (or an ASCII-only run).
        let ch = source[index..].chars().next().expect("index < len");
        let ch_len = ch.len_utf8();

        if ch.is_whitespace() {
            index += ch_len;
            continue;
        }

        // Line comments: both -- and // are legal DAX.
        let rest = &source[index..];
        if rest.starts_with("--") || rest.starts_with("//") {
            let end = rest.find(['\r', '\n']).map_or(source.len(), |i| index + i);
            tokens.push(Token {
                kind: TokenKind::Comment,
                text: &source[start..end],
                start,
            });
            index = end;
            continue;
        }

        // Block comments: unterminated runs to the end of the input.
        if let Some(after_open) = rest.strip_prefix("/*") {
            let end = after_open
                .find("*/")
                .map_or(source.len(), |i| index + 2 + i + 2);
            tokens.push(Token {
                kind: TokenKind::Comment,
                text: &source[start..end],
                start,
            });
            index = end;
            continue;
        }

        let kind;

        // dt"2024-01-01" — the prefix belongs to the literal.
        if matches!(ch, 'd' | 'D')
            && bytes
                .get(index + 1)
                .is_some_and(|b| b.eq_ignore_ascii_case(&b't'))
            && bytes.get(index + 2) == Some(&b'"')
        {
            index = scan_delimited(source, index + 2, b'"');
            kind = TokenKind::DateTime;
        } else if ch == '"' {
            index = scan_delimited(source, index, b'"');
            kind = TokenKind::String;
        } else if ch == '\'' {
            index = scan_delimited(source, index, b'\'');
            kind = TokenKind::QuotedTable;
        } else if ch == '[' {
            index += 1;
            while index < bytes.len() && bytes[index] != b']' {
                index += 1;
            }
            if index < bytes.len() {
                index += 1;
            }
            kind = TokenKind::BracketName;
        } else if ch == '@' {
            index = advance_while(source, index + 1, is_identifier_part);
            kind = TokenKind::QueryParameter;
        } else if ch.is_ascii_digit()
            || (ch == '.' && bytes.get(index + 1).is_some_and(|b| b.is_ascii_digit()))
        {
            index = scan_number(source, index);
            kind = TokenKind::Number;
        } else if is_identifier_start(ch) {
            index = scan_identifier(source, index);
            kind = TokenKind::Identifier;
        } else if TWO_CHAR_OPERATORS.iter().any(|op| rest.starts_with(op)) {
            index += 2;
            kind = TokenKind::Operator;
        } else {
            index += ch_len;
            kind = match ch {
                '(' => TokenKind::OpenParen,
                ')' => TokenKind::CloseParen,
                '{' => TokenKind::OpenBrace,
                '}' => TokenKind::CloseBrace,
                ',' => TokenKind::Comma,
                ';' => TokenKind::Semicolon,
                ':' => TokenKind::Colon,
                '.' => TokenKind::Dot,
                '+' | '-' | '*' | '/' | '^' | '&' | '=' | '<' | '>' | '!' => TokenKind::Operator,
                _ => TokenKind::Unknown,
            };
        }

        tokens.push(Token {
            kind,
            text: &source[start..index],
            start,
        });
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        text: "",
        start: source.len(),
    });
    tokens
}

/// Scans a quoted run, treating a doubled delimiter as an escaped delimiter.
/// Unterminated input consumes the rest of the source.
fn scan_delimited(source: &str, mut index: usize, delimiter: u8) -> usize {
    let bytes = source.as_bytes();
    index += 1; // opening delimiter
    while index < bytes.len() {
        if bytes[index] != delimiter {
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&delimiter) {
            index += 2;
            continue;
        }
        return index + 1;
    }
    index
}

fn scan_number(source: &str, mut index: usize) -> usize {
    let bytes = source.as_bytes();
    while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
        index += 1;
    }

    // Exponent: 1.5E+10, 2e-3. Only consume it when digits actually follow, so
    // that a column reference such as Sales[E] cannot be mistaken for an exponent.
    if bytes
        .get(index)
        .is_some_and(|b| b.eq_ignore_ascii_case(&b'e'))
    {
        let mut lookahead = index + 1;
        if matches!(bytes.get(lookahead), Some(&b'+') | Some(&b'-')) {
            lookahead += 1;
        }
        if bytes.get(lookahead).is_some_and(|b| b.is_ascii_digit()) {
            index = lookahead;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        }
    }
    index
}

/// Scans an identifier, absorbing the dots inside names such as `NORM.DIST` or
/// `CHISQ.INV.RT`. A dot that is not followed by an identifier character is left
/// as its own token, so `'Date'.[Date]` still lexes as three tokens.
fn scan_identifier(source: &str, mut index: usize) -> usize {
    index = advance_while(source, index, is_identifier_part);
    while bytes_at(source, index) == Some(b'.')
        && source[index + 1..]
            .chars()
            .next()
            .is_some_and(is_identifier_part)
    {
        index = advance_while(source, index + 1, is_identifier_part);
    }
    index
}

/// The byte at `index`, when it is inside the source. Multi-byte characters are
/// never equal to an ASCII byte, so this is safe to branch on.
fn bytes_at(source: &str, index: usize) -> Option<u8> {
    source.as_bytes().get(index).copied()
}

/// Advances `index` past every character satisfying `predicate`.
fn advance_while(source: &str, mut index: usize, predicate: impl Fn(char) -> bool) -> usize {
    while let Some(ch) = source[index..].chars().next() {
        if !predicate(ch) {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn is_identifier_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_'
}

fn is_identifier_part(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// The kinds of every token, including the trailing `Eof` — the shape-level
    /// assertions below all compare against this compact form.
    fn kinds(source: &str) -> Vec<TokenKind> {
        tokenize(source).into_iter().map(|t| t.kind).collect()
    }

    /// The text of every token except the trailing `Eof`.
    fn texts(source: &str) -> Vec<&str> {
        let all = tokenize(source);
        let end = all.len() - 1; // drop Eof, whose text is empty
        all[..end].iter().map(|t| t.text).collect()
    }

    #[test]
    fn tokenizes_a_simple_measure_expression() {
        assert_eq!(
            kinds("SUM(Sales[Amount]) + 1"),
            [
                TokenKind::Identifier,
                TokenKind::OpenParen,
                TokenKind::Identifier,
                TokenKind::BracketName,
                TokenKind::CloseParen,
                TokenKind::Operator,
                TokenKind::Number,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn empty_input_produces_only_eof() {
        assert_eq!(kinds(""), [TokenKind::Eof]);
        assert_eq!(kinds("   \r\n\t"), [TokenKind::Eof]);
    }

    #[test]
    fn eof_marks_the_end_of_input() {
        let tokens = tokenize("[X]");
        let eof = tokens.last().expect("eof token");
        assert_eq!(eof.kind, TokenKind::Eof);
        assert_eq!(eof.start, 3);
        assert_eq!(eof.text, "");
    }

    #[test]
    fn tokens_carry_their_exact_source_slices_and_offsets() {
        let source = "SUM('Sales Header'[Net Price])";
        let tokens = tokenize(source);
        for token in &tokens {
            assert_eq!(&source[token.start..token.end()], token.text);
        }
        assert_eq!(
            texts(source),
            ["SUM", "(", "'Sales Header'", "[Net Price]", ")"]
        );
    }

    #[test]
    fn quoted_tables_keep_doubled_quotes_verbatim() {
        // The lexer preserves escapes; logical unescaping happens at
        // materialization (see refs::unescape_name).
        assert_eq!(texts("'It''s'[X]"), ["'It''s'", "[X]"]);
        assert_eq!(
            texts("'Sales''s Data'[Amount]"),
            ["'Sales''s Data'", "[Amount]"]
        );
    }

    #[test]
    fn strings_keep_doubled_quotes_verbatim() {
        assert_eq!(texts(r#""say ""hi"" ok""#), [r#""say ""hi"" ok""#]);
    }

    #[test]
    fn identifiers_absorb_internal_dots() {
        assert_eq!(
            texts("NORM.DIST(1, 2, TRUE)"),
            ["NORM.DIST", "(", "1", ",", "2", ",", "TRUE", ")"]
        );
        assert_eq!(texts("CHISQ.INV.RT(x)"), ["CHISQ.INV.RT", "(", "x", ")"]);
    }

    #[test]
    fn a_trailing_dot_stays_its_own_token() {
        assert_eq!(
            kinds("'Date'.[Date]"),
            [
                TokenKind::QuotedTable,
                TokenKind::Dot,
                TokenKind::BracketName,
                TokenKind::Eof,
            ]
        );
    }

    #[rstest]
    #[case("Sales[E]", &["Sales", "[E]"])]
    #[case("1.5E+10", &["1.5E+10"])]
    #[case("2e-3", &["2e-3"])]
    #[case("1.5E", &["1.5", "E"])]
    #[case("1E", &["1", "E"])]
    #[case(".5", &[".5"])]
    #[case("1.2.3", &["1.2.3"])]
    fn numbers_follow_the_exponent_lookahead_rule(#[case] source: &str, #[case] expected: &[&str]) {
        assert_eq!(texts(source), expected);
    }

    #[test]
    fn comment_markers_inside_a_string_are_not_a_comment() {
        // Ported from the SQLBI smoke tests: the "--" inside the string literal
        // must remain part of the string.
        assert_eq!(
            kinds(r#"VAR Note = "-- not a comment""#),
            [
                TokenKind::Identifier,
                TokenKind::Identifier,
                TokenKind::Operator,
                TokenKind::String,
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            texts(r#"VAR Note = "-- not a comment""#),
            ["VAR", "Note", "=", r#""-- not a comment""#,]
        );
    }

    #[rstest]
    #[case("-- line comment")]
    #[case("// line comment")]
    #[case("/* block comment */")]
    fn comments_become_single_comment_tokens(#[case] source: &str) {
        assert_eq!(kinds(source), [TokenKind::Comment, TokenKind::Eof]);
    }

    #[test]
    fn line_comments_end_at_the_line_break() {
        let source = "[A] -- [B] not a ref\n[C]";
        let tokens = tokenize(source);
        assert_eq!(texts(source), ["[A]", "-- [B] not a ref", "[C]"]);
        let comment = &tokens[1];
        assert_eq!(comment.kind, TokenKind::Comment);
        assert!(!comment.text.contains('\n'));
    }

    #[test]
    fn unterminated_block_comment_runs_to_the_end() {
        assert_eq!(kinds("/* [hidden"), [TokenKind::Comment, TokenKind::Eof]);
        assert_eq!(
            tokenize("/* [hidden").first().expect("comment").text,
            "/* [hidden"
        );
    }

    #[test]
    fn datetime_literals_swallow_the_whole_prefix() {
        assert_eq!(
            kinds("dt\"2024-01-01\""),
            [TokenKind::DateTime, TokenKind::Eof]
        );
        assert_eq!(
            kinds("DT\"2024-01-01\""),
            [TokenKind::DateTime, TokenKind::Eof]
        );
        assert_eq!(texts("dt\"2024-01-01\""), ["dt\"2024-01-01\""]);
    }

    #[rstest]
    #[case("==")]
    #[case("<>")]
    #[case(">=")]
    #[case("<=")]
    #[case("&&")]
    #[case("||")]
    #[case("=>")]
    #[case(":=")]
    fn two_character_operators_lex_as_one_token(#[case] source: &str) {
        assert_eq!(kinds(source), [TokenKind::Operator, TokenKind::Eof]);
        assert_eq!(tokenize(source).first().expect("op").text, source);
    }

    #[test]
    fn operators_prefer_the_two_character_form() {
        // "a<=b" is a single <= operator, not < then =.
        assert_eq!(texts("a<=b"), ["a", "<=", "b"]);
        assert_eq!(texts("a<b"), ["a", "<", "b"]);
    }

    #[test]
    fn query_parameters_take_the_identifier_tail() {
        assert_eq!(kinds("@Risk"), [TokenKind::QueryParameter, TokenKind::Eof]);
        assert_eq!(texts("@Risk"), ["@Risk"]);
    }

    #[test]
    fn unknown_characters_never_fail_the_scan() {
        assert_eq!(
            kinds("[A] # [B]"),
            [
                TokenKind::BracketName,
                TokenKind::Unknown,
                TokenKind::BracketName,
                TokenKind::Eof,
            ]
        );
    }

    #[rstest]
    #[case("'Table")]
    #[case("\"string")]
    #[case("[Col")]
    fn unterminated_delimiters_run_to_the_end_without_panicking(#[case] source: &str) {
        let tokens = tokenize(source);
        assert_eq!(tokens.len(), 2); // one token + Eof
        assert_eq!(&source[tokens[0].start..tokens[0].end()], source);
    }

    #[test]
    fn unicode_names_lex_as_single_identifiers() {
        assert_eq!(texts("MÅNED.Æble"), ["MÅNED.Æble"]);
        assert_eq!(texts("'Salg'[Årsag]"), ["'Salg'", "[Årsag]"]);
    }

    #[test]
    fn unicode_whitespace_is_skipped_at_char_boundaries() {
        // U+00A0 no-break space between the two identifiers.
        let source = "A\u{00A0}B";
        assert_eq!(texts(source), ["A", "B"]);
    }

    #[test]
    fn multibyte_characters_do_not_distort_offsets() {
        let source = "'Ærø'[Ø]";
        let tokens = tokenize(source);
        for token in &tokens {
            assert_eq!(&source[token.start..token.end()], token.text);
        }
        assert_eq!(tokens.last().expect("eof").start, source.len());
    }
}
