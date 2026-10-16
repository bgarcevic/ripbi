# `--summary` and `--plain`

## `--summary`

The human mode for big models: the summary line, one `label: count` line per non-empty
group, a `Worst tables:` breakdown, one `Broken visual bindings:` line when any are
reported (issue #60), and one `Auto date/time:` line when the model has
such tables — the machinery and the distinct date columns its local tables serve, with
the verdicts as the breakdown (`Auto date/time: 6 hidden tables over 5 date columns
(1 in use, 5 dead)`; the shared `DateTableTemplate_*` serves no column, so the columns
can be fewer than the tables, and a model whose machinery pairs with no column at all
drops the clause). No findings list. Type flags filter the counts and the breakdown
like any other mode. Same stdout, same exit codes, same stderr (notices still print).
Use `--plain` or `--json` when you want the individual objects.

```text
3781 objects, 1207 reachable from 2962 roots, 2574 unused

Measures: 214
Columns: 2211
Report measures: 149

Worst tables:
  'Sales'      412
  'Customer'   187
  'Date'       154
  ... and 14 more tables with findings
```

The `Worst tables:` block groups the same surviving findings the counts above come from
— `[scan].ignore` suppressions and type-filter-hidden objects never appear in it — and
names the tables carrying the most of them, so a big model answers "where do I start".
Rules:

- At most 10 rows; when more tables carry findings, a footer says how many.
- Ordered by count descending, then table name folded case-insensitively — the
  model's identity ordering, so the order is stable across runs.
- A dead relationship counts under its "from" table. Report measures, shared
  expressions, and functions belong to no table and are skipped here (their counts
  above still show them); the block is omitted entirely when nothing surviving has a
  table.

## `--plain`

One record per finding on stdout, tab-separated, greppable — one
`broken_visual:<reason>` record per reported broken binding (issue #60), one
`broken_artifact` record per DAX owner (issue #84) — followed by one
`auto_date_time:<verdict>` record per auto date/time table. Type flags filter the
finding records (`--broken` selects the broken ones the same way); the
`auto_date_time:` records print only when the section does (no
type selection, or `--type table` among it). A report-level `broken_artifact`
record has a third field containing the report path; model artifacts have two:

```
measure	'Sales'[Legacy Total]
column	'Sales'[Legacy]
bookmark	bookmark 'FY24 view'
broken_visual:field_not_found	'Sales'[Color]
broken_visual:bound_artifact_broken	'Sales'[Broken Total]
broken_artifact	'Sales'[Broken Total]
auto_date_time:in_use	table 'LocalDateTable_9e0bbdfc-…'
auto_date_time:dead	table 'DateTableTemplate_0039983e-…'
```

For PBIX and `.abf` models, an unused finding with a size gains a third field, its
bytes on disk (`column	'Opportunity'[Name]	25887`); the first two fields never change.

Under `--compare-root`, one `fixed:<type>\t<id>` record follows per finding of the
other checkout that is gone here (see
[Comparing against another checkout](compare.md)).
