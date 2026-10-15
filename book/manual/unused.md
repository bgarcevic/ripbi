# What counts as unused

`ripbi scan` starts with the fields used by the reports it can read, then
follows the model expressions those fields depend on. An object is reported
unused when no path from those report bindings reaches it.

For example, if a visual uses measure `[B]` and `[B]` uses `[A]`, both measures
are live. If nothing in the reports reaches `[B]`, both can be reported unused,
even though `[B]` refers to `[A]`. The finding describes the model and reports
that ripbi analyzed; it is a starting point for review, not a deletion command.

This page summarizes the rules. [Graph and liveness rules](graph.md) is the full
specification.

## What keeps an object alive

The starting points, called *roots*:

- **Report bindings:** every field a visual, filter, slicer, sort, conditional
  format, drillthrough page, or bookmark uses — in the desktop layout and the
  phone layout alike.
- **Row-level security roles:** their filters and the tables and columns they
  grant or revoke. Roles are never findings themselves.
- **`ripbi_keep` annotations:** an object the model marks as used by something
  ripbi cannot see. See [Keeping objects on purpose](scan/keep.md).
- **Logged queries:** objects that DAX or MDX queries in a workspace-monitoring
  log name. See [Logged queries](scan/queries.md).

From the roots, liveness spreads along these paths:

- **DAX references.** A live measure, calculated column, calculation item, KPI,
  or function keeps everything its DAX names alive. When a bare `[Name]` could
  mean several objects, all of them stay alive.
- **Tables and their members.** A live column, measure, or hierarchy keeps its
  table alive. The reverse does not hold: a live table does *not* keep its
  ordinary columns alive, which is why unused columns in used tables are the most
  common finding. A live table does keep its partitions and engine-computed
  columns (calculated-table and calculation-group columns).
- **Sort-by, group-by, and hierarchy levels.** A live column keeps its sort-by and
  group-by columns; a live hierarchy keeps its level columns.
- **Relationships.** A relationship is live when either of its tables is. An
  active relationship keeps both key columns alive. An inactive one is live only
  when live DAX switches it on with `USERELATIONSHIP`. A relationship never keeps
  a table alive, so a table that nothing but a relationship touches is unused.
- **Power Query.** A query that reads another query or a parameter, or merges
  another table, keeps it alive — removing it would break refresh. A column named
  in Power Query is *not* kept alive: the query can keep producing the column
  while the model stops loading it. Such findings can show a
  `Power Query also names it` note (with `--power-query`).
- **Report measures and bookmarks** are ordinary objects: an unused report measure
  is a finding, and so is a bookmark that only points at deleted pages.

Where a rule could go either way, it keeps the object alive. A false "used" only
costs a missed cleanup; a false "unused" would suggest deleting something that
is needed.

## Reading a finding

Each finding says why nothing keeps it alive:

| Annotation | Meaning |
|---|---|
| `← nothing references it` | No object refers to it at all. |
| `← only used by X (also unused)` | Every object that refers to it is itself unused, so the whole chain can go. |
| `← used by X` | The user is live, but its use cannot keep this object alive — for example a key column of an inactive relationship. |

[Human output](scan/human-output.md) describes every annotation and section.

## Two other verdicts

- **Auto date/time.** The hidden `LocalDateTable_*` tables that Power BI creates
  for date columns are always technically reachable, so the scan asks a separate
  question: does any report use them? Each table gets one verdict: in use, unused
  by reports, or dead.
- **Broken bindings.** The scan also reports the opposite problem: report fields
  and DAX expressions that name something the model no longer has. It flags these
  only when sure, because a false "broken" is as misleading as a false "unused".

## What the scan cannot see

Findings cover only the reports and logs you give the scan. Thin reports in other
workspaces, Analyze in Excel, XMLA clients, and other datasets' DAX are invisible
unless you include their reports, mark what they use with `ripbi_keep`, or pass a
query log. A model with no connected reports is refused, because without roots
every object would come back unused. See
[known boundaries](scan/boundaries.md#known-boundaries) before deleting anything,
and use [`ripbi deps`](deps.md) to see what a finding relies on and what relies on
it.
