# Semantic model AST

The normalized shape every source format parses into. Power BI facts that the type
definitions cannot state on their own; read this before changing the AST or adding an
ingestion format.

## What is modeled, and what is not

Modeled: tables, columns, measures, partitions, relationships, hierarchies, RLS roles,
calculation groups, KPIs, and model-level shared M expressions.

Deliberately absent: data sources, perspectives, cultures and translations, role
memberships, annotations, linguistic metadata. None of them *consume* model objects, so
none can keep an object alive, so none affect reachability. Adding them later is additive
and breaks nothing — but do not add them speculatively.

Also absent for the same reason: data types, display folders, source column names,
calculation-item precedence. They describe objects; they never reference them.

## Power BI semantics the types don't show

**A calculated table is not a flag.** It is a table whose partition source is DAX rather
than M. `Table::is_calculated()` derives it from the partition; there is no boolean to
set, and an ingestion format that invents one is wrong.

**A calculation group is a property of a table**, not a top-level object. A calculation
group table also carries synthetic columns — the group's field column and an ordinal
column — in `columns` like any other table. Nothing distinguishes them structurally, and
DAX can reference them (`'Time Intelligence'[Period] = "YTD"`), so ingestion must map
them or those references will not resolve.

**Measure names are unique across the whole model**, not per table. A measure's home
table is provenance for display, not part of its identity for lookup. See
[name-resolution.md](name-resolution.md).

**Relationships default to active.** TMDL omits the flag for active relationships, so
`Relationship::default()` sets `is_active: true`. An inactive relationship still keeps its
key columns alive — `USERELATIONSHIP` can switch it on at query time — so the flag is for
reporting and linting, never for liveness.

**A column's sort-by column is a liveness edge.** A used column keeps the column it sorts
by alive, even when nothing else references it.

**Partition sources are four, not two.** M (Power Query), DAX (calculated table), a legacy
native query in the data source's own dialect, and `Other` for DirectLake entity
partitions, inferred partitions, and kinds Microsoft has not shipped yet. `Other` and
`Query` yield no DAX and no M, and must never be guessed at — an unrecognized source
lands in `Other`, which is why its `Default` is not derived.

## Expressions hide in unobvious places

Missing one expression site means the objects it references get no edges and are reported
unused. That is a false positive, and the scan design forbids them. Beyond the obvious
measure and calculated-column expressions, DAX also lives in:

- dynamic format strings, on measures and on calculation items
- KPI target, status, and trend expressions
- detail-rows (drillthrough) definitions, on measures and on tables
- RLS filters, one per role per table
- calculation item expressions

`TabularDatabase::dax_expressions()` and `m_expressions()` are the only enumeration of
these sites. The graph layer consumes those two functions instead of walking the AST, so
a new expression-bearing field cannot be silently omitted from reachability analysis.
**Adding an expression field to the AST means adding it to the enumeration** — the tests
assert every kind is produced exactly once from a fixture that exercises all of them.

Each enumerated expression carries a home table: the row-context table used to resolve
unqualified references inside it. For an RLS filter that is the permission's target table,
not anything belonging to the role. For a calculated table's partition it is the
calculated table itself, which is deliberately conservative — unqualified columns in such
an expression usually belong to the *source* table, so this can only add candidate edges,
never drop them.
