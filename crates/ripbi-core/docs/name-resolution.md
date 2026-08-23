# Name resolution

How a name written in DAX or a report binding becomes a model object. The rules here are
Analysis Services semantics plus one project rule; they are not obvious from the code.

## Analysis Services naming rules

**Names are case-insensitive**, compared under the invariant culture. `'Sales'[Amount]`
and `'SALES'[amount]` are the same object.

**Non-ASCII names are normal**, not an edge case — Danish models are a primary target, so
`MÅNED` and `måned` must fold together. Folding goes through one function so the rule has
a single definition and swapping the algorithm stays a one-line change. Never fold inline
at a call site.

**Measure names are unique model-wide.** The engine enforces it, which is why a measure
resolves from its bare name with no table.

**Column names are unique only within their table**, and hierarchy names only within
theirs. A hierarchy therefore has no unqualified form and no cross-table fallback.

## The conservatism rule

Marking one object used too many is harmless. Marking one too few tells a user to delete
live code. Every ambiguous case below resolves in favour of keeping objects alive, and any
future resolution rule must do the same.

An unresolvable name is **data, not an error** — a stale expression, a typo, or a table
someone deleted by hand. Resolution returns nothing and lets the graph layer decide.
Duplicate names are invalid in a real model but do occur in hand-edited files; the first
occurrence wins and nothing panics.

## Unqualified `[Name]` is genuinely ambiguous

In DAX row context, `[Name]` binds to a column of the current table. Outside row context
it binds to the measure of that name. Both can exist at once — a measure `[Antal]` on one
table and a column `[Antal]` on another are both legal.

A lexer cannot tell the two apart without a full parse and semantic analysis, so
resolution returns **all** candidates: the model-global measure and the home table's
column. The graph layer must add an edge to every candidate. Resolving to the measure
alone would leave a live column with no incoming edge and report it unused.

A `primary()` helper exists for diagnostics and display, where a single answer is needed.
The graph layer must not use it — it drops a candidate by design.

## Qualified `Table[Name]` falls through to a global measure

The named table's columns are tried first. If none matches, a measure of that name
anywhere in the model does. This looks wrong and is deliberate: because measure names are
model-global, a reference carrying a stale or mistaken table prefix — `'Dato'[Total Sales]`
for a measure that lives on `Sales`, or a prefix naming a table that no longer exists —
still keeps that measure alive.

Column-before-measure priority matters and is pinned by a test: reversing it would resolve
`'Dato'[Antal]` to the `Sales` measure and leave the `Dato` column unreferenced.
