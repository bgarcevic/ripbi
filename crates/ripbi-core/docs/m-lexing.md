# Power Query (M) lexing and reference resolution

How an M expression becomes a list of object references. A lexer plus resolution — not a
full parser: nothing here builds a parse tree, evaluates anything, or understands M
semantics beyond "this shape names an object". The module lives beside its DAX sibling
(`src/m.rs`, `src/m/lexer.rs`, `src/m/refs.rs`) and exists for one reason: whether (and
how) a Power Query mention of an object constrains deleting it (issue #39).

## Provenance

The tokenizer follows the lexical grammar of the official Power Query specification
([M language specification, Lexical Structure](https://learn.microsoft.com/en-us/powerquery-m/m-spec-lexical-structure)),
with [microsoft/powerquery-parser](https://github.com/microsoft/powerquery-parser)
(MIT) as the battle-tested reference for what the grammar means in practice. Both are
references only — ripbi is a single static Rust binary, so the TypeScript parser is not
a dependency, the same relationship the DAX lexer has to SQLBI's MIT lexer.

Two spec facts shaped the token set, and both are worth stating because intuition from
other languages is wrong here:

- **`#"…"` is only ever a quoted identifier.** M has no interpolated strings;
  `#"Amount {x}"` is one identifier token, braces and all. powerquery-parser lexes it
  the same way.
- **Regular identifiers absorb internal dots** (`available-identifier dot-character
  regular-identifier`). `Table.SelectRows` is one token, and so is a dotted parameter
  use such as `Server.Name`.

| Ported from the spec/parser | Deliberately not ported |
|---|---|
| `""` doubled-quote escapes in strings and quoted identifiers | Line-mode lexing for multi-line literals (error recovery) |
| Dot-absorbing identifiers (`Table.SelectRows`, `Server.Name`) | The `#(...)` character-escape decoding — the token swallows them verbatim |
| The `#`-keyword family (`#table`, `#date`, `#shared`, …) as identifier-shaped tokens | The full keyword list — M keywords are contextual, so the lexer emits identifiers and extraction carries a small keyword set |
| `#!"…"` verbatim literals | Section documents, `section`/`shared` headers — ingestion already knows each expression's owner |
| `//` and `/* */` comments, hex `0x` numbers, `??`/`=>`/`..` operators | The parser itself (naive or combinator) — reference extraction never needs a parse tree |
| Tolerance-first scanning: unterminated delimiters run to EOF, never fail | |

## Token grammar

Whitespace produces no tokens, so the token after any token is exactly its next
significant neighbour — the property the extraction rules rely on.

| Token | Matches | Notes |
|---|---|---|
| `Identifier` | `[A-Za-z_]\w*` with Unicode letters, internal dots absorbed; `#name` included | `Source`, `let`, `Table.SelectRows`, `#table`, `Server.Name` |
| `QuotedIdentifier` | `#"…"` with `""` as the escape | `#"1998 Sales"` — never an interpolated string |
| `Verbatim` | `#!"…"` | a literal that errors at runtime; never yields a reference |
| `String` | `"…"` with `""` as the escape | only yields a reference inside a whitelisted call |
| `Number` | digits/dots, `0x` hex, exponent only when digits follow | `{1..5}` lexes tolerantly; numbers never yield references |
| `Comment` | `// …`, `/* … */` | never yields a reference |
| `[` `]` | punctuator tokens, **not** swallowed | the extractor tells field access from record literals |
| `Operator`, parens, braces, `,` `;` `.` | longest operator first (`...`, `..`, `??`, `<>`, `<=`, `>=`, `=>`) | `@` is a plain M punctuator |
| `Unknown` | anything else | scanning continues; nothing panics |
| `Eof` | — | always the last token |

Offsets are byte offsets into the expression; all token text is borrowed `&str` slices —
lexing allocates nothing but the token vector.

## Extraction rules

Three shapes (`src/m/refs.rs`), each conservative in the same direction:

1. **Field access.** `[Amount]`, `#"Sales"[Amount]`, `Source[Amount]`, `each [Amount]`.
   A bracket group is a field access exactly when its contents are one *generalized
   identifier* — identifiers, dots, digits, quoted identifiers, blanks. A group like
   `[CommandTimeout = 30]` is a record literal and names nothing; the `=` (any
   non-GID token, really) is the discriminator. Contents are walked either way, so
   strings inside a whitelisted call's record arguments still get harvested.
2. **Names.** Every bare or quoted identifier is a table/shared-expression candidate —
   most are `let` variables that resolve to nothing, which is data. Call names are
   candidates too: shared expressions frequently hold user-defined functions
   (`fnEasterSunday(year)`), and skipping calls would be the one direction this module
   must never err in. Dotted identifiers additionally emit their dot-separated parts,
   preserving the old substring matcher's deliberate `Server`-inside-`Server.Name`
   over-marking. Bare M keywords (`each`, `let`, …) are never candidates — `each
   [Amount]` is row context, not a table named `each`. The one special qualifier is
   `#shared[Name]`: its pieces are query names, not fields.
3. **Column strings.** The string arguments of a curated whitelist of column-centric
   built-ins (`Table.ExpandTableColumn`, `Table.NestedJoin`, `Table.TransformColumnTypes`,
   `Table.ReplaceValue`, `#table`, …) become column candidates. Harvesting is
   nesting-aware, so pair lists (`{{"Amount", type text}}`) count. Functions whose
   string arguments are *data* (`Table.SelectRows`, `Text.From`, `Sql.Database`) are
   deliberately absent — only a real column reference may keep a column alive.
   `Table.ReplaceValue` is the judgment call: most of its arguments are values, but its
   trailing column list is not, and a missed column is the unsafe direction.

Strings and comments never yield references by themselves: a name inside a comment or
an unrelated string is not a use. That is the one deliberate narrowing against the old
whole-word substring matcher, and it is visible in scans — a shared expression named
only inside a comment is now correctly reported unused.

## Binding

`m::bind` resolves one raw reference against the model (`Binding::Bound { targets }` /
`Binding::Unresolved`, same shape as `dax::bind`). Naming, not keeping alive — what a
target is worth is the graph layer's call (next section):

- **Qualified** `#"Sales"[Amount]` → that table **and** its column, via
  `ModelIndex::resolve_table` + `resolve_column`. No measure fallback: an M expression
  cannot reference a measure, unlike DAX where a stale qualifier keeps a same-named
  measure alive.
- **Unqualified `[Name]` and column strings** → *every* column of that name model-wide
  (`ModelIndex::resolve_columns`). M string arguments carry no row context, so the
  conservative set is wider than the DAX home-table rule.
- **Names** → the table and/or shared expression of that name.

## What a mention is worth: supply chain vs liveness

The pipeline is M → tables/columns → DAX → reports, and deletion never breaks upstream.
A partition's M reads the *source* and produces an output table; each model column maps
onto that output by name (`sourceColumn`). Deleting a model column leaves the M
untouched — the query still runs, still outputs the column, and the unmapped output is
ignored. Refresh breaks only in the other direction: a model column whose
`sourceColumn` is missing from the M output, caused by editing the M or by source
drift, never by deleting the column.

So a **column** named in M — a Changed Type enumeration, an `each [Region]` filter, a
join-key string — is that column's *supply chain*, not a consumer, and creates
deliberately **no liveness edge**. Unloading the column is always safe; removing it
*entirely* (model and script) means editing the steps that name it, which is the
context that rides on the finding (`UnusedObject::named_by_m`, the
`named_in_power_query` JSON field, the `⭘ Power Query also names it` annotation).
The binding itself stays name-conservative; the graph applies one production rule on
top — only **Data** columns carry the context, because an M step can only name a
column it produces, so a calculated column matching an M name (the auto date/time
columns vs Desktop's date-template query) is coincidence. Measure Killer's
classification agrees column-for-column here.

A **table or shared expression** named in M is different: deleting it deletes the query
the expression reads or joins, and *that* breaks refresh. Those stay real
`Provenance::M` edges — a merge source like `#"Dim Lookup"` in a `Table.NestedJoin`
keeps the whole table alive, and so does a qualified `#"Dim Lookup"[Key]` field access.

## The conservatism rule

Over-marking is harmless; under-marking deletes live code.

- **A keep flows through its owner.** The liveness edges (table and expression targets)
  are ordinary `Provenance::M` edges from the partition or shared expression, so a dead
  table's partition keeps nothing alive — a reference never marks anything live on its
  own.
- **A partition naming its own table** creates no edge (no information, and it would
  cycle with the table-partition edge). Its naming its own *columns* is the supply-chain
  context, never a keep.
- **What is not modeled, on purpose:** `let`-binding dataflow (resolving which table a
  variable denotes), a full M parser, section documents, and `.pbix` DataMashup
  ingestion. The `RawRef` surface is the seam a fuller parser could replace without
  touching the graph.
