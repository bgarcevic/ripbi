# JSON output (`--json`)

<!-- Maintainers: render.rs implements this contract; change both together. -->

Pretty-printed JSON, stable field order, additive schema:

```json
{
  "schema_version": 1,
  "target": "samples/AdventureWorks Sales.SemanticModel",
  "reports": ["samples/AdventureWorks Sales.Report"],
  "summary": {
    "findings": 64,
    "objects": 130,
    "reachable": 74,
    "roots": 51,
    "unused": 56,
    "unused_total": 56,
    "ignored": 0,
    "kept": 0,
    "queried": 0,
    "broken": 2,
    "broken_total": 2,
    "broken_artifacts": 1,
    "broken_artifacts_total": 1,
    "auto_date_time": {
      "hidden_tables": 6,
      "date_columns": 5,
      "member_findings": 41,
      "in_use": 1,
      "unused_by_reports": 0,
      "dead": 5
    }
  },
  "unused": [
    {
      "type": "column",
      "id": "'Sales'[Order Quantity (base)]",
      "table": "'Sales'",
      "used_by": [
        {
          "id": "'Sales'[Order Quantity]",
          "provenance": "measure expression",
          "also_unused": true
        }
      ],
      "named_in_power_query": []
    }
  ],
  "broken": [
    {
      "target": "'Sales'[Color]",
      "reason": "field_not_found",
      "bound_artifact": null,
      "bound_artifact_report": null,
      "provenance": "field well 'Values' — visual 'V2' on page 'P1'",
      "location": "field well 'Values' — card visual 'Colors sold' (id 'V2') on page 'Overview' (id 'P1')"
    },
    {
      "target": "'Sales'[Broken Total]",
      "reason": "bound_artifact_broken",
      "bound_artifact": "'Sales'[Broken Total]",
      "bound_artifact_report": null,
      "provenance": "field well 'Values' — visual 'V3' on page 'P1'",
      "location": "field well 'Values' — card visual 'V3' on page 'Overview' (id 'P1')"
    }
  ],
  "broken_artifacts": [
    {
      "id": "'Sales'[Broken Total]",
      "type": "measure",
      "report": null,
      "unresolved_references": ["'Sales'[Nope]"]
    }
  ],
  "auto_date_time": [
    {
      "verdict": "in_use",
      "id": "table 'LocalDateTable_9e0bbdfc-9803-41d0-b204-481ce398f228'",
      "source_column": "'Opportunity Calendar'[Date]",
      "finding": null
    }
  ],
  "skips": {
    "count": 0,
    "notices": []
  }
}
```

- `summary.findings` counts every reported finding: `unused`, `broken`,
  `broken_artifacts`, and the `auto_date_time` rows whose verdict is not `in_use` (a
  `dead` row counts once, its nested finding with it). It equals the number of
  unsuppressed SARIF results. Under `--compare-root` it counts only the new findings,
  since existing ones are left out of every array.
- `summary.unused` is the length of `unused` — after `[scan].ignore` and the type
  flags. `summary.unused_total` counts every unused object in the graph before any
  suppression, filter, or section move, so `reachable = objects − unused_total` always
  holds and a consumer can tell a filtered-away finding from an absent one.
  `summary.ignored` counts findings suppressed by `[scan].ignore` — unused objects,
  broken artifacts, and broken bindings alike. `summary.kept` counts the objects a
  `ripbi_keep` model annotation keeps (a kept table counts once) — roots, so never in
  `unused` (see [Keeping objects on purpose](keep.md)). `summary.queried`
  counts the objects `--queries-from` logged queries name directly — roots too, and `0`
  without a log (see [Logged queries](queries.md)). On a model with
  auto date/time machinery, the remaining gap between `unused` and `unused_total` is
  the machinery: `summary.auto_date_time.member_findings` counts its unused members
  and the `dead` verdict count its nested own findings.
- `summary.broken` is the length of `broken` — after `[scan].ignore`, the
  unknown-object suppression, and the type selection. `summary.broken_total` counts every
  broken binding
  detected, before any of those, so a consumer can tell a suppressed or filtered-away
  binding from an absent one (issue #60).
- `summary.broken_artifacts` is the length of `broken_artifacts` after ignore,
  `unknown_object` suppression, and type selection. `broken_artifacts_total`
  counts all detected artifact owners before those steps. Each owner counts once,
  even when several expressions or references are broken.
- `summary.auto_date_time` counts the section's rows by verdict, plus
  `hidden_tables` (every row, all verdicts together), `date_columns` (the distinct
  user date columns the machinery serves — the shared template serves none), and
  `member_findings` (the machinery's unused members covered by the rows, absent from
  `unused` individually).
- Type selection filters the `unused` array and `summary.unused`; `summary.unused_total`
  stays model-wide. Both breakage arrays and their reported summary counts follow the same rule:
  a `--type` selection without `--broken` empties them, while `--broken` retains them.
  The `auto_date_time` array and `summary.auto_date_time` counts are present in full
  when no type selection is passed or `--type table` is selected; otherwise they
  are empty and zero.
- `broken` (issue #60) carries one row per reported broken visual binding:
  `target` is the written field reference; `reason` is one of `table_not_found`,
  `field_not_found`, `measure_not_found`, `hierarchy_not_found`, `level_not_found`,
  or `bound_artifact_broken`; `bound_artifact` names the broken artifact the binding
  lands on (present exactly when the reason is `bound_artifact_broken`);
  `bound_artifact_report` is the report path for a report-level measure and null
  for model artifacts or other reasons; `provenance`
  is the same binding-site phrase the unused findings' `used_by` entries carry,
  `mobile layout …` prefixed for phone-layout bindings — IDs only, so it stays a
  stable key (baselines compare on it) when a visual is retitled; `location` is
  the same site with the visual's type and title and the page's display name, for
  people. For an Excel workbook (`reports` lists its path like any report's) the
  page is the sheet and the visual is the PivotTable (`pivotTable`), slicer
  (`slicer`), timeline (`timeline`), or CUBE formula cell (`cubeFormula`, named
  after the cell); the field well is `Rows`, `Columns`, `Filters`, `Values`,
  `Calculated members`, `Field`, or the CUBE function's name; and `target` is the
  MDX unique name as written, e.g. `[Product].[Category].[Category]`. Bindings suppressed by the
  clean-ingest bar or by `[scan].ignore` are absent entirely; the totals above keep
  the arithmetic.
- `broken_artifacts` carries one row per DAX owner: `id`, object `type`, nullable
  report path, and sorted `unresolved_references`. Report paths distinguish
  equally named report measures in different reports.
- `auto_date_time` carries one row per `LocalDateTable_*`/`DateTableTemplate_*` table:
  `verdict` is `in_use`, `unused_by_reports`, or `dead`; `source_column` names the
  varied user column the machinery serves (`null` when none resolves, e.g. the
  template); `finding` is the table's own unused finding — with its `used_by` chain —
  present exactly when `verdict` is `"dead"` (the row moved here from `unused`). Rows
  suppressed by `[scan].ignore` are absent entirely. The machinery's other unused
  members are in no array at all: the row covers them (issue #47), and
  `summary.auto_date_time.member_findings` counts them.
- `type` is one of `table`, `column`, `measure`, `hierarchy`, `partition`,
  `relationship`, `role`, `calculation_item`, `expression`, `function`,
  `report_measure`, `bookmark`.
- `table` is the finding's model table, quoted (`'Sales'`) — a relationship reports
  its "from" side. It is `null` for the kinds with no model table (`role`,
  `expression`, `function`, `report_measure`, `bookmark`). `--plain` deliberately omits it: its
  records keep the type and id as their first two fields.
- `provenance` is the human phrase for how the use is made (e.g. `measure expression`,
  `field well 'Y' — visual 'V' on page 'P' in report 'R'`, `hierarchy level`). A
  stale bookmark edge uses `stale bookmark` and always has `also_unused: true`. A
  binding from the phone layout prefixes `mobile layout ` (`mobile layout field well
  'Y' — visual 'V' on page 'P' in report 'R'`), so an audit names the right surface.
- `named_in_power_query` lists the M expressions (partitions by their table, shared
  expressions by name) that mention the column — supply-chain context, never a
  consumer. Empty for every non-column finding and for columns no M step names.
- Storage fields (issue #122; see [Storage sizes](storage.md)) appear only for
  PBIX and `.abf` models, or other models with a storage source attached, and are
  omitted — not `null` — everywhere else, so the output of other inputs is unchanged. An unused entry (or a dead auto date/time
  row's `finding`) that is a table, column, or relationship carries `bytes`,
  `size_basis` (`"files"`, exact; `"lower_bound"`, files missing from the backup log; or
  `"engine"`, in-memory sizes from a `.vpax`, issue #108), `rows`, and — columns only — `cardinality` (distinct values).
  `summary.unused_bytes` totals `unused`, each file once (a table covers its
  reported columns and relationships); `summary.unused_bytes_lower_bound` is `true`
  when any contributing size is a lower bound, and absent otherwise;
  `summary.model_bytes` is every data file in the model (in the storage source, when
  one is attached); `summary.stats_source` is that source's path (issue #129),
  absent for a PBIX or `.abf` model's own catalog.
- `compare` (issue #141) is present only under `--compare-root`:
  `{"root", "baseline", "baseline_detail"?, "existing", "unverified", "fixed", "fixed_uncertain"}`.
  `root` is the other checkout as given; `baseline` is `scanned`, `skipped` (the model had
  no connected reports there), or `unavailable` (the scan could not run there), with
  `baseline_detail` saying why when it is not `scanned`; `unverified` lists, under a
  `skipped` baseline, the unused findings on objects that already existed there, as
  `{type, id}` (absent from every array above and from the exit code; empty otherwise); `existing`
  counts the reportable findings that already existed there, which are absent from
  every array above and from the summary counts that mirror them; `fixed` lists the
  other checkout's findings this scan no longer detects, as `{type, id}` plus
  `reason`/`provenance` (broken bindings), `report` (report-level broken artifacts), or
  `verdict` (auto date/time tables) where they apply; `fixed_uncertain` is `true` when
  `fixed` is non-empty and this scan recorded an `unknown_object` skip notice (see
  [Comparing against another checkout](compare.md)). Without
  the flag the key is omitted, so other scans' output is unchanged.
- `skips.notices` carries `{path, location, kind, detail}` per parser skip; `kind` is
  one of `unknown_object`, `unknown_property`, `malformed_value`, `unresolved_alias`,
  `stale_state` (also: a `--queries-from` log that does not identify the model),
  `opaque_source` (an M partition calls `Value.NativeQuery` or
  `Odbc.Query`; SQL text is not analyzed), `not_analyzed` (an Excel workbook input
  ripbi cannot read statically: a CUBE formula argument computed at run time, a
  connection that does not query an OLAP model, or the workbook's own Data Model),
  `incomplete_report_set` (the model was
  fetched by `ripbi fetch` and its `.ripbi/source.json` lists downstream reports that
  could not be downloaded; see [fetch](../fetch.md)), and — when reports are discovered by walking search folders —
  `unresolved_dataset_reference` (a report item
  under a search folder with no usable `datasetReference`), `unresolved_app_binding` (a
  Fabric app under a search folder whose `fabric.yaml` names no model to pair it with),
  and `malformed_report_item`
  (an anchor-less `.Report` folder the search walk pruned). Under `--strict`,
  `count > 0` corresponds to exit code `2`.
  Each affected partition contributes one `opaque_source` notice; exact duplicate
  notices are collapsed before rendering. In `--plain` and `--summary`, these
  notices use the same stderr block as every other skip kind.
