# Graph and liveness rules

The dependency graph behind `ripbi scan` and `ripbi deps`: how it is built, each
rule that keeps an object alive, and the two verdicts beside reachability (auto
date/time and broken bindings). It is the specification for people auditing results
or changing the analyzer; changes here change what `scan` suggests reviewing. For
the user-facing summary, see
[what counts as unused](https://bgarcevic.github.io/ripbi/unused.html).

## Shape

One `DependencyGraph` per semantic model, shared by every report that connects to it.
Nodes are `ObjectId`s — every model object, report measure, and fully stale
bookmark, whether or not anything references them, so an isolated object can
be reported unused. Edges point from user to
used and carry their `Provenance` as the petgraph edge weight, written once at build
time. Reverse queries (`consumers_of`) are plain reads; a second view over the graph
(`ripbi deps`) must be pure rendering in the CLI.

Live report sites — visuals, pages, bookmarks — are not model objects and have no
`ObjectId`, so their bindings live beside the graph as `roots`, each with its
binding provenance. A fully stale bookmark is instead a dead node: its
deleted-page saved fields are non-liveness edges to their resolved targets.
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

**Dynamic M parameters.** A dynamic M query parameter is a shared expression whose
view-time values come from a model column, so a report slicer feeds the M query
directly (issue #50). The model records the binding in two halves: the column carries
an anonymous marker extended property (`ParameterMetadata` with `"kind": 1`), and the
parameter expression names the column — `parameterValuesColumn: DaysList.Days`. Only
the second half is modeled: a consumed parameter keeps its bound column alive with the
`dynamic M parameter binding` provenance, because deleting the column breaks the
binding and every partition whose M consumes the parameter. The chain is report →
consuming table → partition → parameter (M references) → bound column, so the column
needs no DAX or report reference of its own. The edge flows one way only: an
unconsumed parameter is itself dead, and its bound column dies with it; a binding
naming a column the model no longer has keeps nothing alive (misses are data). The
other `ParameterMetadata` shapes are not bindings: `"kind": 2` marks field parameters
and `"version": 0` marks what-if parameters — both stay unmodeled, since their columns
are kept alive by ordinary DAX (`SELECTEDVALUE`) and report references.

**Report bindings.** Every `ReportModel::bindings` target is a reachability root with
its provenance. Measure targets resolve report-first: within its report, a report
measure shadows a model measure of the same name. `Aggregation` unwraps to its inner
field; `HierarchyLevel` keeps the hierarchy and the level's underlying column;
`Written` falls through the same ladder as a written qualified reference. The phone
layout's pages (`ReportModel::mobile_pages`, issue #49) enumerate like any other
page's: a field bound only by a `definition.mobile/` visual is live, because phone
users see it, and its roots carry the `mobile` marker so an audit can tell the two
layouts apart.

**Fabric-app literals.** A Fabric app's `ReportModel::code` literals are roots too,
with a `BindingSite::Code { file, line }` site. A literal carrying `[` or `EVALUATE`
resolves as a DAX query (the query-log binder); a whole literal also resolves as a bare
name — a table, a measure, or a column. Bare column names are scoped: they bind the
same-named columns of the tables the app names anywhere (as a literal or inside its
DAX), falling back to every same-named column in the model when no named table has
one, because an app composing `'${table}'[${column}]` writes the column name with no
table beside it. A literal that matches nothing is text — a label, a route, a CSS
selector like `[data-id]` — and never a `BrokenBinding`: the app's literals
over-approximate what it reads, never the other way round.

Breakage is claimed only inside an *anchored* DAX literal: one carrying `[` or
`EVALUATE` in which at least one field reference resolves against the model — the
evidence that the literal is a query against this model at all. Every other field
reference in it that does not resolve becomes a `BrokenBinding` at the literal's
`Code` site (`TableNotFound`, `FieldNotFound`, or `MeasureNotFound` for an
unqualified name), and one that resolves onto a broken artifact becomes
`BoundArtifactBroken` (`graph/broken.rs`, `code_breakage`). The usual resolution
allowances apply (`field_resolves`), with one difference: the query-time names are
the whole app's. An app splits a query at its `${…}` interpolations, so a name one
literal introduces (`SUMMARIZECOLUMNS(…, "Total", …)`) is often read in another
(`ORDER BY [Total]`) or in another file, and every DAX string in every literal of the
app counts. A literal whose references all fail has nothing to anchor it and is not
read — an under-claim, as breakage must be.

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

**Kept objects (issue #151).** An object carrying the `ripbi_keep` annotation
(`TabularDatabase::kept_objects`) is a root beside the report bindings — the model's
own declaration that a consumer ripbi cannot see reads it. It is live, and so is
everything it references: the inputs of a measure Excel reads are needed too, which is
why the annotation roots instead of suppressing a finding. A kept table seeds every
member (columns, measures, hierarchies, calculation items, partitions), not just
itself, since containment only flows member → table. A kept relationship counts as
activated: its key columns seed the weak pass directly, so they stay live even when
it is inactive, but — like every relationship — it never keeps its tables alive.
Kept objects live in `DependencyGraph::kept`, not in `roots`, so `roots()` stays the
report bindings and the scan summary's root count is unchanged; `kept_by` answers
which annotation keeps an object (its own, or its kept table's).

**Report measures are nodes, not roots.** An unused report measure is dead — it is
exactly the accumulated bloat this tool looks for. Its body's references stay alive
only through it, so they die with it, annotated.

**Stale bookmarks.** A bookmark with at least one saved section, no applicable
sections, and no report-level filters is an unused node. Its saved filters and
projections on deleted pages carry `stale bookmark` edges for dead-chain
explanations, but those edges never confer liveness. A bookmark with any live
section or report-level filters remains a root site for its applicable bindings;
a bookmark with no sections is page-independent and is not a finding.

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
key columns are findings chained under it (`only used by … (also unused)`).

**Role permissions.** Roles keep their granted tables and their object-level (OLS)
columns alive — a grant and a `none` revocation both count, since dropping either object
breaks the role on deploy. A kept-alive column carries its table along through
containment for the same reason. Roles themselves are seeds, never findings, because
security configuration is not bloat.

## The relationship rule and the two-pass traversal

The subtle decision: a live table keeps its relationships and key columns alive, but a
key column kept alive *only* as a relationship endpoint does **not** keep its table
alive. A table referenced by nothing but a relationship is still unused — deleting it
together with the relationship is safe, and in a star schema where every table is
related, this is the difference between table findings and none.

The one exception is a **bridge**: a table on the filter path between two distinct live
tables. In a snowflake `Sales → Bridge → User`, a filter on `User` reaches `Sales` only
through `Bridge`, so deleting `Bridge` breaks the report even though nothing references
it. Filters run from the "to" (one) side to the "from" (many) side, and back again when
the relationship filters both ways (`crossFilteringBehavior: bothDirections`, or
one-to-one; `automatic` or an unknown value counts as both ways too). Only active or
`USERELATIONSHIP`-activated relationships carry filters. Direction matters: a dead `Date`
dimension filtering two live fact tables carries nothing *between* them, so it stays a
finding. A bridge joins the strong set with everything it keeps (its partitions and
relationships, not its other columns), and the search repeats until no new bridge
appears. Bridges are not edges, because a bridge is live only when *both* ends are, and
no single edge can say that.

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

## The inverted question: broken bindings (issue #60)

Where the liveness question is "does anything keep this alive?", the broken-bindings
records (`broken_bindings`, built in `graph/broken.rs`) ask the opposite: did a
written report binding name anything at all? During root construction every binding's
resolution is classified — resolved, resolved only to its qualifying table (the field
is gone), or resolved to nothing — and the misses become `BrokenBinding`s carrying the
binding's full provenance (`BindingEdge`), the written `FieldTarget`, and a
`BrokenReason`. A second, standalone pass walks every DAX expression the model and the
reports own. `broken_artifacts()` reports each owner with unresolved field references
once, whether or not a visual binds it; report measures are distinguished by their
position in the input report slice. A binding that *resolves* onto such an artifact
stays a root and also reports inherited breakage (`bound_artifact_broken`). Neither
finding changes reachability.

The conservatism rule inverts: a liveness claim may over-keep, a breakage claim must
under-claim — a false "broken" is itself a breakage claim. So the classification never
flags what the machinery might resolve: KPI-suffixed measure variants (`… Goal`,
`… Status`, `… Trend`, `… Value`) resolve when the base measure exists; a
variation-flavored hierarchy the variation machinery cannot resolve stays silent
(rather than risk calling auto date/time serialization drift a breakage); a
same-named hierarchy or calculation item behind a stale column qualifier resolves;
a qualified field miss on a *calculated* table resolves when the name is lexically
visible in that table's partition expression — its string literals, the constructor
defaults, the columns it writes, and every column of the tables it references, the
over-approximation of an output schema the TMDL never declares; and a `Written`
reference the parser could not structure never flags. The
qualifying-table fallback keeps its liveness role unchanged — the records ride along,
they never move a node in or out of the unused set.

## The deps view

The per-object exploration view is pure CLI rendering over the queries this
graph already answers: `producers_of` (upstream), `consumers_of` (downstream),
`roots_of` (the report bindings beside the graph), and — since the `deps`
command landed — the multi-hop slices `dependencies_of`/`impact_of`
(`graph/slice.rs`: BFS with a depth cutoff, cycle-safe, the *induced* edge
set, every edge followed — exploration wants the truth, not the liveness
policy, so the strong/weak-pass rules stay private to reachability).
`graph/provenance.rs` carries the stable snake_case machine keys beside the
`Display` phrases, and `lookup.rs` resolves user-written references to
`ObjectId`s (shared, never `deps`-specific).

## Determinism

Node and edge construction follows model and report source order; identical
`(from, to, provenance)` triples dedupe; `unused_objects` sorts by `ObjectId` (folded
names, so case never changes the order), and `broken_bindings` sorts by where the
binding lives (report, page, visual, bookmark, layout), then the written target, then
the reason. Two runs over the same input produce the same
output, byte for byte.
