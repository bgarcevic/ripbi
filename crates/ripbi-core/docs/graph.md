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

**M references.** A partition or shared expression keeps any shared expression whose
name appears whole-word in its M text alive. M identifiers are case-sensitive, but the
match is case-insensitive on purpose: matching too broadly only over-marks. Unlike the
DAX lexer, the scan does not skip strings or comments — a name mentioned only there
still marks its referent alive, because over-marking is the safe direction and a real
lexer could only narrow the result. Edges flow expression-to-expression too, so a
partition keeps its staging query alive and the staging query keeps the parameter it
names alive. This rule exists because a Power Query parameter referenced only by one
partition would otherwise be reported unused, and deleting it breaks the partition.

**Report bindings.** Every `ReportModel::bindings` target is a reachability root with
its provenance. Measure targets resolve report-first: within its report, a report
measure shadows a model measure of the same name. `Aggregation` unwraps to its inner
field; `HierarchyLevel` keeps the hierarchy and the level's underlying column;
`Written` falls through the same ladder as a written qualified reference.

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

**Relationships.** Live if either endpoint table is reachable, and they keep **both**
key columns alive — inactive ones included (`USERELATIONSHIP`). Roles keep their
granted tables and filtered columns alive; roles themselves are seeds, never findings,
because security configuration is not bloat.

## The relationship rule and the two-pass traversal

The subtle decision: a live table keeps its relationships and key columns alive, but a
key column kept alive *only* as a relationship endpoint does **not** keep its table
alive. A table referenced by nothing but a relationship is still unused — deleting it
together with the relationship is safe, and in a star schema where every table is
related, this is the difference between table findings and none.

One plain BFS cannot express that (containment would drag the far table in), so
reachability runs two passes and the policy falls out of what each pass excludes:

1. **Strong pass** — from the roots over every edge except relationship endpoints.
   Containment fires; the result is everything that can keep its table alive.
2. **Weak pass** — extends the strong set over every edge *except* containment. The
   relationships of live tables and both their key columns join here, without
   propagation into tables.

Unused = every node in neither pass. For any unused object, every referencing object is
provably either itself unused, or a weakly-live key column — which is exactly the
`also_unused: false` case in `UsedBy`, and the reason `unused_objects` needs no
special-casing to annotate chains.

## Zero roots means everything is unused

A model scanned with no reports and no roles has no reachability roots; every object
comes back unused. That is the honest answer, not a special case: callers (the CLI)
decide whether it is a finding or a missing report and should say so.

## Determinism

Node and edge construction follows model and report source order; identical
`(from, to, provenance)` triples dedupe; `unused_objects` sorts by `ObjectId` (folded
names, so case never changes the order). Two runs over the same input produce the same
output, byte for byte.
