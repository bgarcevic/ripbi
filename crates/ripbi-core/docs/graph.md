# Dependency graph and reachability

How the model and report ASTs become one graph, and what "unused" means. Every rule here
is a decision someone made on purpose; changing one changes what `scan` tells users to
delete. Read this before touching `src/graph.rs` or its submodules.

## Shape

One `DependencyGraph` per semantic model, shared by every report that connects to it.
Nodes are `ObjectId`s — every model object and report measure, whether or not anything
references them, so an isolated object can be reported unused. Edges point from user to
used and carry their `Provenance` as the petgraph edge weight, written once at build
time. Reverse queries (`consumers_of`) are plain reads; a second view over the graph
(`ripbi deps`) must be pure rendering in the CLI.

Report sites — visuals, pages, bookmarks — are not model objects and have no `ObjectId`,
so report bindings live beside the graph as `roots`, each with its binding provenance.
An `RLS role 'Reader' filter` edge, by contrast, *is* an object-to-object edge: the role
is a node.

## The conservatism rule

Marking an object used too many is harmless. Marking one too few tells a user to delete
live code. Where the rules below could go either way, they go wide. In particular:
an unqualified `[Name]` keeps **every** candidate alive (never
`UnqualifiedMatches::primary`), and an extended resolution adds candidates the plain
binder does not know rather than fewer.

## The edge catalog

**DAX references.** For every expression from `TabularDatabase::dax_expressions` and
`ReportModel::dax_expressions`, every reference binds through `dax::bind` and edges to
each target carry the expression kind as provenance. This is the only path a measure's
body, a calculated column, a KPI, an RLS filter, a calculation item, or a function body
can keep anything alive.

**Extended resolution.** Beyond the binder's own answers, a qualified `'Table'[Name]`
also keeps a same-named hierarchy (`ISINSCOPE('Date'[Calendar])`) and, when the table is
a calculation group, a same-named calculation item
(`'Time Intelligence'[YTD]`) alive. A qualified reference matching **nothing** keeps its
qualifying table alive — the nearest resolvable candidate the written form asserts.
Unqualified, bare, and call references that match nothing keep nothing alive: there is
nothing resolvable to point at.

**M references.** Every M expression — a table partition or a shared expression — is
lexed (`m::bind`, see [m-lexing.md](m-lexing.md)). The pipeline is M → tables/columns →
DAX → reports, so deletion never breaks upstream, and the two directions of a mention
split:

- a **shared expression** named in the text (a parameter, a staging query) keeps alive —
  deleting it deletes the query the expression reads, which breaks refresh. M-to-M
  chains flow one hop per edge;
- a **table** named as a merge source (`Table.NestedJoin(…, #"Dim Lookup", …)`) or as a
  qualified field access (`#"Dim Lookup"[Key]`) keeps alive, for the same reason. A
  partition naming its *own* table creates no edge, and a dead table's partition keeps
  nothing alive, because every M edge flows from its owner;
- a **column** named by bracket field access (`each [Amount]`) or by the string
  arguments of the column-centric built-ins
  (`Table.ExpandTableColumn(Source, "Amount")`) keeps **nothing** alive. The column's
  M mention is its supply chain — the query keeps producing the column and the model
  just stops mapping it — so unloading it cannot break refresh. The naming expressions
  travel with the finding instead (`UnusedObject::named_by_m`, rendered as the
  `named_in_power_query` JSON field and the `⭘ Power Query also names it` annotation):
  unloading is safe, and removing the column from the script *entirely* means editing
  those steps too. Only **Data** columns carry that context — an M step can only name
  a column it produces, so a calculated column matching an M name is coincidence
  (auto date/time columns named like Desktop's date-template query), never supply
  chain.

Matching is by identifier tokens, case-insensitively: names inside comments and
unrelated strings do not count (the one deliberate narrowing against the old substring
scan, which this rule replaced), while bare identifiers still over-mark — most are
`let` variables that resolve to nothing. Unresolved references stay data, never errors.

**Report bindings.** Every `ReportModel::bindings` target is a reachability root with
its provenance. Measure targets resolve report-first: within its report, a report
measure shadows a model measure of the same name. `Aggregation` unwraps to its inner
field; `HierarchyLevel` keeps the hierarchy and the level's underlying column;
`Written` falls through the same ladder as a written qualified reference.

**Date-hierarchy bindings over a variation.** A visual's date hierarchy under auto
date/time is written against the *varied* (base) table — `HierarchyLevel` with a
`PropertyVariationSource` — but the hierarchy lives on the engine's hidden
`LocalDateTable_*`. When the named table carries no such hierarchy, resolution follows
the model's declaration: the varied column's `variation` names the relationship (and
the default hierarchy), and the binding lands on the related date table's hierarchy
and level column. If the serialization dropped the variation object, the relationship
is found by shape instead — the one touching the varied column whose other endpoint is
a flagged auto date/time table. Without a resolution the coarse fallback applies: the
table the binding names stays alive.

**Calculation-item selection.** A report binding that lands on a calculation-group
column — a slicer over the field column, a filter naming an item — can select any of
the group's items at query time, so it keeps every item of the group alive, carrying
the binding's provenance. Written uses only: structural liveness of the group (its
table kept alive by one explicitly named item) deliberately does not spread to the
unselected items. DAX that references the column without naming an item
(`'Date Role'[Date Role] = "By Ship Date"`) currently keeps only the column alive —
the string is data, not a reference the lexer can bind.

**Report measures are nodes, not roots.** An unused report measure is dead — it is
exactly the accumulated bloat this tool looks for. Its body's references stay alive
only through it, so they die with it, annotated.

**Containment.** A used member — column, measure, hierarchy, calculation item — keeps
its table alive. A used table keeps its partitions, its relationships, and its
engine-managed columns alive (calculated-table columns, calculation-group columns, and
calendar columns are materialized with the table and cannot be dropped independently).
The flow is one-directional on purpose: a table does not keep its ordinary columns
alive, because unused columns in used tables are the bread and butter of the findings.

**Sort-by, group-by, hierarchy levels.** A used column keeps its `sortByColumn` and
`groupByColumns` alive; a used hierarchy keeps its levels' columns alive. Dead chains
form the other way: an unused sorted column drags its unused sort column along,
annotated.

**Relationships.** Live if either endpoint table is reachable. An **active** relationship
keeps **both** key columns alive; an **inactive** one is live only when a live DAX
reference (`USERELATIONSHIP`) activates it — switching one on at query time is DAX's
job, and nothing else can. Unactivated, the relationship is itself a finding, and its
key columns are findings chained under it (`only used by … (also unused)`). Roles keep
their granted tables and filtered columns alive; roles themselves are seeds, never
findings, because security configuration is not bloat.

## The relationship rule and the two-pass traversal

The subtle decision: a live table keeps its relationships and key columns alive, but a
key column kept alive *only* as a relationship endpoint does **not** keep its table
alive. A table referenced by nothing but a relationship is still unused — deleting it
together with the relationship is safe, and in a star schema where every table is
related, this is the difference between table findings and none.

One plain BFS cannot express that (containment would drag the far table in), so
reachability runs two passes and the policy falls out of what each pass excludes:

1. **Strong pass** — from the roots over every edge except relationship endpoints and
   the inactive-relationship edges. Containment fires; the result is everything that
   can keep its table alive.
2. **Weak pass** — extends the strong set over every edge *except* containment and the
   inactive-relationship edges. The relationships of live tables and their active key
   columns join here, without propagation into tables; an inactive relationship joins
   only through a live `USERELATIONSHIP` reference, whose Dax edge the strong pass
   already carries.

Unused = every node in neither pass. For any unused object, every referencing object is
provably either itself unused, a weakly-live key column, or the table of an inactive
relationship the relationship cannot keep alive — which is exactly the
`also_unused: false` case in `UsedBy`, and the reason `unused_objects` needs no
special-casing to annotate chains.

## Zero roots means everything is unused

A model scanned with no reports and no roles has no reachability roots; every object
comes back unused. That is the honest answer, not a special case: callers (the CLI)
decide whether it is a finding or a missing report and should say so.

## The second verdict: auto date/time is a provenance question

Reachability answers "does anything keep this alive?" For the engine's auto date/time
machinery (`LocalDateTable_*` / `DateTableTemplate_*`) that answer is misleading, because
the framework relationship to the user's date column keeps the machinery alive for as
long as that column is used — a framework-generated edge, not a real consumer. So the
graph carries a second, deliberately *non-reachability* verdict beside `unused_objects`:
`auto_date_time_tables` reads the same resolved roots and the same reachability set with
a different question — does a *report binding* land on the machinery?

- **In use** — a `Provenance::Binding` root (or selection edge) lands on the table or
  one of its members. Alive and used; the advice is to replace it with a real date table.
- **Unused by reports** — nothing binds it, yet reachability keeps it alive: pure bloat
  no dead-code finding can express, because the object is not dead. Disable auto date/time.
- **Dead** — reachability never reached it; its own finding moves into the section so
  the verdict and the dead chain read together.

The flags identifying the machinery (`is_local_date_table`, `is_template_date_table`,
`is_private`) are display-only metadata and never touch either verdict's inputs.

## Determinism

Node and edge construction follows model and report source order; identical
`(from, to, provenance)` triples dedupe; `unused_objects` sorts by `ObjectId` (folded
names, so case never changes the order). Two runs over the same input produce the same
output, byte for byte.
