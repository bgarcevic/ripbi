//! M tokenizer, following the lexical grammar of the official Power Query
//! specification (<https://learn.microsoft.com/en-us/powerquery-m/m-spec-lexical-structure>)
//! with microsoft/powerquery-parser (MIT) as the battle-tested reference for the
//! fiddly corners. Reduced to what reference extraction needs — no line-mode
//! bookkeeping, no error positions, no formatter.
//!
//! Like the DAX tokenizer, the scanner is tolerance-first: unterminated strings,
//! quoted identifiers, and comments never fail — they simply run to the end of
//! the input. Lexing is a total function; there is nothing to report as an error.
//!
//! Two spec details shape the token set:
//!
//! - `#"…"` is **only** a quoted identifier. M has no interpolated strings;
//!   the `#"name"` form follows exactly the same character rules as a text
//!   literal (doubled-quote escapes, `#(lf)`-style escapes stay inside the
//!   token), and `#!"…"` is the verbatim literal.
//! - Regular identifiers absorb internal dots lexically — `Table.SelectRows`
//!   is one token, and so is a dotted parameter use such as `Server.Name`.

/// What a [`Token`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// End of input. Always the last token; `text` is empty.
    Eof,
    /// `Source`, `let`, `Table.SelectRows`, `#table`, `Server.Name` — keywords,
    /// dotted names, and the `#`-prefixed keyword family are all identifiers
    /// here; whether a word means a keyword or a name is resolution's business,
    /// not the lexer's.
    Identifier,
    /// `#"1998 Sales"` — a quoted identifier, `""` as the escape.
    QuotedIdentifier,
    /// `#!"not parsed"` — a verbatim literal (evaluates to an error value).
    Verbatim,
    /// `"text"` — a text literal, `""` as the escape.
    String,
    /// `1`, `1.5`, `1.5E-10`, `0xff`.
    Number,
    /// `// …` or `/* … */` — never produces a reference.
    Comment,
    /// `= < > <= >= <> + - * / & @ ! ? ?? => .. ...`
    Operator,
    /// `.` on its own — a dot that did not absorb into an identifier.
    Dot,
    /// `(`
    OpenParen,
    /// `)`
    CloseParen,
    /// `[` — opens a field access (`[Name]`) or a record literal (`[K = V]`);
    /// the extractor tells the two apart.
    OpenBracket,
    /// `]`
    CloseBracket,
    /// `{`
    OpenBrace,
    /// `}`
    CloseBrace,
    /// `,`
    Comma,
    /// `;`
    Semicolon,
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

/// Multi-character operators, longest first.
const MULTI_CHAR_OPERATORS: [&str; 7] = ["...", "..", "??", "<>", "<=", ">=", "=>"];

/// Converts M source into tokens, ending with one [`TokenKind::Eof`].
///
/// Whitespace produces no tokens, so the token after any given token is exactly
/// its next significant neighbour — the property the reference extractor's
/// adjacency rules rely on.
///
/// ```
/// use ripbi_core::m::{TokenKind, tokenize};
///
/// let tokens = tokenize("Table.SelectRows(Source, each [Amount] > 0)");
/// let kinds: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
/// assert_eq!(
///     kinds,
///     [
///         TokenKind::Identifier,
///         TokenKind::OpenParen,
///         TokenKind::Identifier,
///         TokenKind::Comma,
///         TokenKind::Identifier,
///         TokenKind::OpenBracket,
///         TokenKind::Identifier,
///         TokenKind::CloseBracket,
///         TokenKind::Operator,
///         TokenKind::Number,
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

        let rest = &source[index..];

        // Line and block comments. Unterminated block comments run to the end.
        if rest.starts_with("//") {
            let end = rest.find(['\r', '\n']).map_or(source.len(), |i| index + i);
            tokens.push(Token {
                kind: TokenKind::Comment,
                text: &source[start..end],
                start,
            });
            index = end;
            continue;
        }
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

        if ch == '"' {
            index = scan_delimited(source, index);
            kind = TokenKind::String;
        } else if rest.starts_with("#!\"") {
            index = scan_delimited(source, index + 2);
            kind = TokenKind::Verbatim;
        } else if rest.starts_with("#\"") {
            index = scan_delimited(source, index + 1);
            kind = TokenKind::QuotedIdentifier;
        } else if ch == '#'
            && source[index + 1..]
                .chars()
                .next()
                .is_some_and(is_identifier_start)
        {
            // The `#`-prefixed keyword family: #table, #date, #shared, …
            index = scan_identifier(source, index + 1);
            kind = TokenKind::Identifier;
        } else if ch.is_ascii_digit()
            || (ch == '.' && bytes.get(index + 1).is_some_and(|b| b.is_ascii_digit()))
        {
            index = scan_number(source, index);
            kind = TokenKind::Number;
        } else if is_identifier_start(ch) {
            index = scan_identifier(source, index);
            kind = TokenKind::Identifier;
        } else if let Some(op) = MULTI_CHAR_OPERATORS
            .iter()
            .find(|op| rest.starts_with(**op))
        {
            index += op.len();
            kind = TokenKind::Operator;
        } else {
            index += ch_len;
            kind = match ch {
                '(' => TokenKind::OpenParen,
                ')' => TokenKind::CloseParen,
                '[' => TokenKind::OpenBracket,
                ']' => TokenKind::CloseBracket,
                '{' => TokenKind::OpenBrace,
                '}' => TokenKind::CloseBrace,
                ',' => TokenKind::Comma,
                ';' => TokenKind::Semicolon,
                '.' => TokenKind::Dot,
                '=' | '<' | '>' | '+' | '-' | '*' | '/' | '&' | '@' | '!' | '?' => {
                    TokenKind::Operator
                }
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

/// Scans a quoted run (`"…"`, `#"…"`, `#!"…` up to its closing quote), treating
/// a doubled quote as an escaped quote. Unterminated input consumes the rest of
/// the source. `index` points at the opening quote.
fn scan_delimited(source: &str, index: usize) -> usize {
    let bytes = source.as_bytes();
    let mut index = index + 1; // opening delimiter
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'"') {
            index += 2;
            continue;
        }
        return index + 1;
    }
    index
}

fn scan_number(source: &str, mut index: usize) -> usize {
    let bytes = source.as_bytes();

    // Hexadecimal: 0xff / 0XFF. A `0x` without hex digits after it falls back
    // to the decimal path below and lexes as `0`.
    if bytes[index] == b'0'
        && bytes
            .get(index + 1)
            .is_some_and(|b| b.eq_ignore_ascii_case(&b'x'))
        && bytes.get(index + 2).is_some_and(|b| b.is_ascii_hexdigit())
    {
        index += 2;
        while index < bytes.len() && bytes[index].is_ascii_hexdigit() {
            index += 1;
        }
        return index;
    }

    while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
        index += 1;
    }

    // Exponent: 1.5E+10, 2e-3. Only consume it when digits actually follow, so
    // that a field access such as `1[E]` cannot be mistaken for an exponent.
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

/// Scans an identifier, absorbing the internal dots of names such as
/// `Table.SelectRows` or `Server.Name`, per the spec's
/// `available-identifier dot-character regular-identifier` production. A dot
/// that is not followed by an identifier character is left as its own token.
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
    fn tokenizes_a_typical_partition_step() {
        assert_eq!(
            texts(
                r#"#"Changed Type" = Table.TransformColumnTypes(Source, {{"Amount", type text}})"#
            ),
            [
                "#\"Changed Type\"",
                "=",
                "Table.TransformColumnTypes",
                "(",
                "Source",
                ",",
                "{",
                "{",
                "\"Amount\"",
                ",",
                "type",
                "text",
                "}",
                "}",
                ")",
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
        let source = "Table.Column(#\"My Table\", \"Col\")";
        let tokens = tokenize(source);
        for token in &tokens {
            assert_eq!(&source[token.start..token.end()], token.text);
        }
        assert_eq!(
            texts(source),
            ["Table.Column", "(", "#\"My Table\"", ",", "\"Col\"", ")"]
        );
    }

    #[test]
    fn quoted_identifiers_keep_doubled_quotes_verbatim() {
        // The lexer preserves escapes; logical unescaping happens at bind time.
        assert_eq!(texts("#\"It\"\"s\""), ["#\"It\"\"s\""]);
        assert_eq!(
            texts("#\"Change \"\"Type\"\"\""),
            ["#\"Change \"\"Type\"\"\""]
        );
    }

    #[test]
    fn strings_keep_doubled_quotes_verbatim() {
        assert_eq!(texts(r#""say ""hi"" ok""#), [r#""say ""hi"" ok""#]);
    }

    #[test]
    fn escape_sequences_stay_inside_the_string() {
        // #(cr,lf) contains no quote, so it is just string content.
        assert_eq!(texts("\"a#(cr,lf)b\""), ["\"a#(cr,lf)b\""]);
        // #(#)( escapes the escape start itself — still string content.
        assert_eq!(texts("\"#(#)(\""), ["\"#(#)(\""]);
    }

    #[test]
    fn the_verbatim_literal_is_its_own_kind() {
        assert_eq!(
            kinds("#!\"let x = 1\""),
            [TokenKind::Verbatim, TokenKind::Eof]
        );
        assert_eq!(texts("#!\"let x = 1\""), ["#!\"let x = 1\""]);
    }

    #[test]
    fn the_hash_quote_pair_is_never_an_interpolated_string() {
        // M has no interpolated strings: #"…" is always a quoted identifier,
        // braces and all — matching the spec and powerquery-parser.
        assert_eq!(
            texts("#\"Amount {x}\""),
            ["#\"Amount {x}\""],
            "the brace stays inside the quoted identifier"
        );
    }

    #[test]
    fn identifiers_absorb_internal_dots() {
        assert_eq!(texts("Table.SelectRows"), ["Table.SelectRows"]);
        assert_eq!(texts("a.b.c(x)"), ["a.b.c", "(", "x", ")"]);
        // A dotted parameter use is one identifier, just like a built-in.
        assert_eq!(
            texts("Sql.Database(Server.Name)"),
            ["Sql.Database", "(", "Server.Name", ")"]
        );
    }

    #[test]
    fn a_trailing_dot_stays_its_own_token() {
        assert_eq!(
            kinds("A.B."),
            [TokenKind::Identifier, TokenKind::Dot, TokenKind::Eof]
        );
        assert_eq!(
            kinds("A . B"),
            [
                TokenKind::Identifier,
                TokenKind::Dot,
                TokenKind::Identifier,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn hash_prefixed_keywords_lex_as_identifiers() {
        assert_eq!(
            texts("#table({\"A\"}, {{}})"),
            [
                "#table", "(", "{", "\"A\"", "}", ",", "{", "{", "}", "}", ")"
            ]
        );
        assert_eq!(
            texts("#date(2024, 1, 1)"),
            ["#date", "(", "2024", ",", "1", ",", "1", ")"]
        );
        // #shared[Query] is how M reaches every query in the section.
        assert_eq!(
            kinds("#shared[#\"My Query\"]"),
            [
                TokenKind::Identifier,
                TokenKind::OpenBracket,
                TokenKind::QuotedIdentifier,
                TokenKind::CloseBracket,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn a_bare_hash_is_unknown() {
        assert_eq!(
            kinds("# 1"),
            [TokenKind::Unknown, TokenKind::Number, TokenKind::Eof]
        );
    }

    #[rstest]
    #[case("0xff", &["0xff"])]
    #[case("0X1F", &["0X1F"])]
    #[case("1.5", &["1.5"])]
    #[case("1.5E+10", &["1.5E+10"])]
    #[case("2e-3", &["2e-3"])]
    #[case("1.5E", &["1.5", "E"])]
    #[case(".5", &[".5"])]
    #[case("0x", &["0", "x"])]
    fn numbers_follow_the_spec_shapes(#[case] source: &str, #[case] expected: &[&str]) {
        assert_eq!(texts(source), expected);
    }

    #[test]
    fn a_list_range_lexes_without_confusing_numbers_and_dots() {
        // The dot-absorbing number scan reads `1..5` as one number token.
        // Numbers never yield references, so the tolerance is harmless.
        assert_eq!(texts("{1..5}"), ["{", "1..5", "}"]);
    }

    #[rstest]
    #[case("// line comment")]
    #[case("/* block comment */")]
    fn comments_become_single_comment_tokens(#[case] source: &str) {
        assert_eq!(kinds(source), [TokenKind::Comment, TokenKind::Eof]);
    }

    #[test]
    fn line_comments_end_at_the_line_break() {
        let source = "[A] // [B] not a ref\n[C]";
        let tokens = tokenize(source);
        assert_eq!(
            texts(source),
            ["[", "A", "]", "// [B] not a ref", "[", "C", "]"]
        );
        let comment = &tokens[3];
        assert_eq!(comment.kind, TokenKind::Comment);
        assert!(!comment.text.contains('\n'));
    }

    #[test]
    fn unterminated_block_comment_runs_to_the_end() {
        assert_eq!(kinds("/* [hidden"), [TokenKind::Comment, TokenKind::Eof]);
    }

    #[test]
    fn comment_markers_inside_a_string_are_not_a_comment() {
        assert_eq!(texts("\"// not a comment\""), ["\"// not a comment\""],);
    }

    #[test]
    fn brackets_are_punctuator_tokens() {
        // Field access and record literals both use plain brackets; the
        // extractor tells the two apart by their contents.
        assert_eq!(
            kinds("[Amount] = [K = 1]"),
            [
                TokenKind::OpenBracket,
                TokenKind::Identifier,
                TokenKind::CloseBracket,
                TokenKind::Operator,
                TokenKind::OpenBracket,
                TokenKind::Identifier,
                TokenKind::Operator,
                TokenKind::Number,
                TokenKind::CloseBracket,
                TokenKind::Eof,
            ]
        );
    }

    #[rstest]
    #[case("??")]
    #[case("<>")]
    #[case(">=")]
    #[case("<=")]
    #[case("=>")]
    #[case("..")]
    #[case("...")]
    fn multi_character_operators_lex_as_one_token(#[case] source: &str) {
        assert_eq!(kinds(source), [TokenKind::Operator, TokenKind::Eof]);
        assert_eq!(tokenize(source).first().expect("op").text, source);
    }

    #[test]
    fn operators_prefer_the_longest_form() {
        assert_eq!(texts("a<=b"), ["a", "<=", "b"]);
        assert_eq!(texts("a?b"), ["a", "?", "b"]);
        assert_eq!(texts("a ?? b"), ["a", "??", "b"]);
    }

    #[test]
    fn unknown_characters_never_fail_the_scan() {
        assert_eq!(
            kinds("[A] ~ [B]"),
            [
                TokenKind::OpenBracket,
                TokenKind::Identifier,
                TokenKind::CloseBracket,
                TokenKind::Unknown,
                TokenKind::OpenBracket,
                TokenKind::Identifier,
                TokenKind::CloseBracket,
                TokenKind::Eof,
            ]
        );
    }

    #[rstest]
    #[case("\"string")]
    #[case("#\"identifier")]
    #[case("#!\"verbatim")]
    fn unterminated_delimiters_run_to_the_end_without_panicking(#[case] source: &str) {
        let tokens = tokenize(source);
        assert_eq!(tokens.len(), 2); // one token + Eof
        assert_eq!(&source[tokens[0].start..tokens[0].end()], source);
    }

    #[test]
    fn unicode_names_lex_as_single_identifiers() {
        assert_eq!(texts("Måned"), ["Måned"]);
        assert_eq!(texts("#\"Salg ~ Beløb\""), ["#\"Salg ~ Beløb\""]);
    }

    #[test]
    fn multibyte_characters_do_not_distort_offsets() {
        let source = "#\"Ærø\"[Ø]";
        let tokens = tokenize(source);
        for token in &tokens {
            assert_eq!(&source[token.start..token.end()], token.text);
        }
        assert_eq!(tokens.last().expect("eof").start, source.len());
    }
}
