# DAX lexing and reference resolution

How a DAX expression becomes a list of object references. A lexer plus resolution — not a
full parser: nothing here builds a parse tree, evaluates anything, or understands DAX
semantics beyond "this shape names an object".

## Provenance

The tokenizer (`src/dax/lexer.rs`) is a Rust port of SQLBI Whiteboard's `DaxLexer`
([sql-bi/SQLBI-Whiteboard](https://github.com/sql-bi/SQLBI-Whiteboard)), © SQLBI, MIT
licensed. Their implementation is the authority on DAX's fiddly corners; the port keeps
its behaviours verbatim where listed below and drops everything formatter-shaped:

| Ported as-is | Deliberately not ported |
|---|---|
| `''` / `""` doubled-delimiter escapes (`scan_delimited`) | Comment attachment to tokens (formatter layout) |
| Dot-absorbing identifiers — `NORM.DIST` is one token | `TRUE`/`FALSE` upper-casing (printer normalization) |
| Dot-not-absorbed trailing dot — `'Date'.[Date]` is three tokens | Function-name canonicalization via the ~700-name list (printer) |
| Exponent lookahead — `1.5E+10` is a number, `Sales[E]` is a column | `DaxParser`, `DaxPrinter`, `Doc`, `DaxCodeFormatter` (formatter pipeline) |
| `dt"…"` date-time literals | `DaxClassifier`, `DefinedObjectName`, `IsQuery` — ingestion already knows each expression's owner |
| `//` and `--` line comments, `/* */` block comments | |
| Tolerance-first scanning: unterminated delimiters run to EOF, never fail | |

The original is hand-written (no generated grammar) and battle-tested against real-world
models; its smoke tests (`SQLBI.Whiteboard.Core.SmokeTests`, DAX section) supplied the
trickiest cases in the Rust test suite — the `Tricky :=` sample where a string contains
`--` and must not become a comment, and the `SUMX ( Sales, … )` definition that must not
mistake its own defined name for a use.

## Token grammar

Whitespace produces no tokens, so the token after any token is exactly its next
significant neighbour — the property the extraction rules rely on.

| Token | Matches | Notes |
|---|---|---|
| `Identifier` | `[A-Za-z_]\w*` with Unicode letters, internal dots absorbed | `NORM.DIST`, `CHISQ.INV.RT`, `MÅNED` |
| `QuotedTable` | `'…'` with `''` as the escape | `'Sales''s Data'` |
| `BracketName` | `[…]`, no escapes — `]` cannot appear in an object name | `[Net Price]`; measures, columns, and levels are indistinguishable here |
| `String` | `"…"` with `""` as the escape | never yields a reference |
| `DateTime` | `dt"…"` (case-insensitive prefix) | never yields a reference |
| `Number` | digits/dots, exponent only when digits follow | the `Sales[E]` trap |
| `Comment` | `// …`, `-- …`, `/* … */` | never yields a reference |
| `QueryParameter` | `@ident` | DAX queries, not model expressions |
| `Operator`, parens, braces, `,` `;` `:` `.` | the obvious spellings, two-char ops first (`==`, `<>`, `>=`, `<=`, `&&`, `||`, `=>`, `:=`) | |
| `Unknown` | anything else | scanning continues; nothing panics |
| `Eof` | — | always the last token |

Offsets are byte offsets into the expression; all token text is borrowed `&str` slices —
lexing allocates nothing but the token vector.

## Extraction rules

A reference is never a single token, so extraction merges adjacent tokens by shape
(`src/dax/refs.rs`):

1. `'Table'` + `[Name]` → qualified field reference.
2. `Table` + `[Name]` → qualified field reference.
3. `Table` alone, quoted or bare → a **table use** (`COUNTROWS(Sales)`).
4. `Name` + `(` → a **call** (function candidate).
5. `[Name]` alone → unqualified field reference.

Strings, comments, numbers, `dt"…"`, and `@parameters` never produce references. A
`. [Name]` after a reference (hierarchy-level syntax, not valid in DAX model
expressions) lexes as an independent unqualified reference — no special handling, no
corruption.

Two consequences worth stating outright:

- **The defined name is not special.** `[Sales Amount] = SUMX(…)` lexes `[Sales Amount]`
  as an unqualified reference like any other. That is correct: ingestion strips definition
  headers before expressions reach the lexer, so the owner/use distinction is already
  resolved by the AST.
- **No function-name list.** The rule "identifier + `(` is a call" needs no list of ~700
  built-ins. Extra references can only ever *reduce* false positives (they mark objects
  used), so the conservative direction needs no curation.

## The conservatism rule

The same rule as [name resolution](name-resolution.md): over-marking is harmless,
under-marking deletes live code.

- **Bare identifiers are table candidates.** `COUNTROWS(Sales)` must keep `Sales` alive
  even when no column of `Sales` is referenced anywhere. A variable or keyword that
  collides with a table name can only over-mark usage — the safe direction. This is why
  there is no `VAR` tracking.
- **Calls are function candidates.** A user-defined function (`TOM` function) called by
  name is a real dependency; if calls were skipped outright, a function used only through
  calls would be reported unused. Built-ins (`SUM`, `COUNTROWS`, …) resolve to nothing
  against the model's function table and stay behind as unresolved data.
- **Unqualified `[Name]` keeps every candidate alive** — measure and home-table column
  alike. See [name resolution](name-resolution.md) for why the ambiguity is genuine.

## Resolution is data, not errors

`dax::bind` turns one raw reference into `Binding::Bound { targets }` (every candidate)
or `Binding::Unresolved` (a stale expression, a typo, a built-in call). There is no error
variant and no panic path; the graph layer decides what an unresolvable reference means.
The lexer itself is total: malformed input yields degraded tokens, never a failure.
