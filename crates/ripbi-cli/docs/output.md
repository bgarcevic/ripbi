# `ripbi scan` output

The user-facing contract for the `scan` command: what each output mode prints, which
stream it goes to, and what the exit codes mean. `render.rs` implements this; change
the two together.

```
ripbi scan [PATH] [flags]
ripbi scan --model PATH [--report PATH]... [flags]
```

`PATH` is a `.pbip` file, a project folder, a `.SemanticModel` item folder, or a
`.Report` item folder. Without PATH (and without `target` in `ripbi.toml`), scan
discovers projects in the current directory: one candidate is announced and scanned,
several prompt with a numbered picker (on a TTY stdin only — otherwise the scan fails
listing them), none is an error.

`--model PATH` names the semantic model explicitly: a `.SemanticModel` folder, its
`definition/` folder, or any folder directly containing `model.tmdl`. It disables
current-directory discovery and the `ripbi.toml` `target`, and reinterprets every
`--report` (or config `reports`) value that is not itself a report item as a **search
folder**: the folder is walked recursively for report items bound to the model. With no
report values at all, the model's parent folder is searched. Reports pair with the model
by their `definition.pbir` path first, then by the PBIP stem convention (`X.Report`
beside `X.SemanticModel`), then by the dataset name in a `byConnection` `initial
catalog`. A scan with no connected reports refuses with exit `2` and per-category counts.

## Streams

| Content | Stream | Notes |
|---|---|---|
| Findings, summary, JSON, plain records | stdout | the machine-readable side |
| Discovery/selection announce, scanning line | stderr | one line each; in `--model` mode the scanning line names every bound report |
| Model-centric pairings (`Note:` by-name matches, `Ignored … bound to other models` exclusions) | stderr | `--model` only; informational, never `--strict`-fatal |
| Coverage caveat | stderr | once per run |
| Skip notices (parser drift, stale saved state, unresolved dataset references) | stderr | grouped under one header; suppressed in `--json` mode, where the JSON carries them |
| Errors + hints | stderr | `error: …` / `hint: …` |

`-q/--quiet` suppresses everything on both streams; the exit code is the only output.

## Model-centric scans (`--model`)

The scanning line names every connected report — direct `--report` items included — and
the pairings that would otherwise be invisible are spelled out on stderr (all suppressed
by `-q`):

```text
Scanning models/Sales.SemanticModel with 3 report(s): Overview.Report, Sales.Report, Thin.Report
Note: reports/Thin.Report matched by dataset name only (byConnection 'initial catalog' = 'Sales').
Ignored 1 report(s) bound to other models: HR.Report
```

- The scanning line's names are report folder names, in ingestion order (walked reports
  by canonical path, then explicitly passed ones).
- The `Note:` line flags reports connected by dataset *name* rather than by path or stem,
  because that pairing is weaker than the written `definition.pbir` path.
- The `Ignored …` line lists report items under the search folders that resolve to a
  different existing model. It is informational: those reports are not ingested, they
  appear in no output mode, and they never fail `--strict` — a healthy multi-model folder
  must stay scannable. Report items whose reference resolves to nothing become
  `unresolved_dataset_reference` skip notices instead, and a `*.Report` folder with no
  `report.json` anchor becomes a `malformed_report_item` notice; both *do* fail `--strict`.
- Reports passed explicitly with `--report` are taken at face value: they are never
  binding-checked and produce none of these notices, exactly as in a `PATH` scan.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Clean: nothing unused, and no auto date/time table unused by reports or dead (objects suppressed by `[scan].ignore` count as handled; an *in use* auto date/time table is informational) |
| `1` | Unused objects found, or auto date/time machinery no report binds |
| `2` | Error: usage, bad PATH, model-only input, a `--model` search with no connected reports, unsupported archive, ingestion failure, ambiguous discovery off-TTY — or any skip notice under `--strict` |

The exit code describes what was *reported*: findings hidden by the type flags, and an
Auto date/time section hidden because `--tables` was not among the passed flags, cannot
fail the run. `--strict` and `-q/--quiet` are unaffected.

## Flags

| Flag | Effect |
|---|---|
| `--json` | JSON on stdout (schema below). Mutually exclusive with `--plain` and `--summary` |
| `--plain` | One `<type>\t<id>` record per finding, for grep/awk |
| `-s`, `--summary` | Counts only: the summary line and per-type totals, no findings list. Mutually exclusive with `--json` and `--plain` |
| `-q`, `--quiet` | No output; exit code only |
| `--model <PATH>` | Analyze one named semantic model (`.SemanticModel`, its `definition/`, or a folder holding `model.tmdl`). Disables cwd discovery and the `ripbi.toml` `target`; plain `--report` folders become search folders for reports bound to this model. Conflicts with `PATH` |
| `--report <PATH>` | Extra report root; repeatable. Replaces `reports` from `ripbi.toml`. With `--model`, a folder that is not itself a report item is searched recursively for reports bound to the model |
| `--measures`, `--columns`, `--hierarchies`, `--tables`, `--partitions`, `--relationships`, `--calc-items`, `--expressions`, `--functions`, `--report-measures` | Report only unused objects of the passed types; repeatable, and passed together they union (`--measures --columns`). Filters every output mode and the exit code. With none of them, everything is reported |
| `--power-query` | Also print the `⭘ Power Query also names it` annotations (human output; a no-op in `--plain`, `--json`, and `-q`, whose consumers filter themselves) |
| `--strict` | Any parser skip notice becomes exit code `2` |
| `--no-color` | Never color (color is also off off-TTY, under `NO_COLOR`, or `TERM=dumb`) |
| `--no-input` | Never prompt; fail where a picker would appear |

## Human output (default)

```text
130 objects, 74 reachable from 51 roots, 56 unused

Measures (11)
  'Sales'[Average Sales per Order]
    ← nothing references it
Columns (28)
  'Customer'[City]
    ← only used by hierarchy 'Customer'[Geography] — hierarchy level (also unused)
  'Customer'[Customer ID]
    ← nothing references it
```

- The summary line: total graph objects, how many reachability reached, from how many
  report binding roots, and the unused count. Roots are report bindings; RLS roles also
  seed reachability without counting here. When `[scan].ignore` suppressed objects, a
  second line says how many; when the type flags hid findings, a third line counts them
  (`(2 unused hidden by type filters)`) so a filtered `0 unused` never reads as a clean
  model.
- Findings are grouped by object type (measures, columns, hierarchies, tables,
  partitions, relationships, calculation items, expressions, functions, report
  measures — fixed order, empty groups omitted), sorted by object identity. The type
  flags restrict the groups to the selected kinds; empty groups are still never printed.
- Chain annotations, one per referencing object:
  - `← nothing references it` — a true orphan, deletable outright;
  - `← only used by 'X' — <where> (also unused)` — the sole (or all-identical case:
    every) consumer is itself unused, so the whole chain can go;
  - `← used by 'X' — <where>` — the consumer is live but its use could not keep this
    object alive (a key column held only by an active relationship endpoint, or the
    table of an inactive relationship nothing activates).
- The `⭘ Power Query also names it (…)` annotation appears, behind `--power-query`,
  on Data columns named by M expressions. With the flag, the last finding above
  reads:

  ```text
    'Customer'[Customer ID]
      ← nothing references it
      ⭘ Power Query also names it ('Customer' partition) — safe to stop loading; removing it from the script means editing those steps too
  ```

  It is supply-chain context, not a consumer:
  unloading the column cannot break refresh, but removing it from the Power Query
  script *entirely* means editing each named partition or expression too — cleanup-time
  guidance, so it is hidden by default and shown only on request. Columns without the
  annotation are also gone from every Power Query step — and engine-computed columns
  (calculated columns, auto date/time machinery) never carry it, because an M step can
  only name a column it produces. `--json` always carries the underlying
  `named_in_power_query` field regardless of the flag.
- The **Auto date/time** section follows the findings: one verdict per
  `LocalDateTable_*`/`DateTableTemplate_*` table, naming the user's date column the
  machinery serves. It is a *provenance* verdict, not a reachability one — the engine's
  own relationship keeps the machinery alive, so "alive" says nothing. The section is
  table-shaped: it prints (and carries its exit-code weight) only when no type flags are
  passed or `--tables` is among them; hidden, it is absent from every output mode.
  - `in use — replace with a real date table` — a report binding lands on the
    machinery (usually a visual's date hierarchy over the varied column). Informational;
    it never fails the exit code.
  - `unused by reports — disable auto date/time` — nothing binds it, yet reachability
    keeps it alive: pure bloat the findings list cannot express, because the object is
    not dead.
  - `dead` — nothing reaches it at all; the table's own finding (with its chain
    annotations) is filed here instead of the generic `Tables` group.

## `--summary`

The human mode for big models: the summary line, one `label: count` line per non-empty
group, a `Worst tables:` breakdown, and one `Auto date/time:` line when the model has
such tables (`Auto date/time: 1 in use, 2 unused by reports, 5 dead`) — with no findings
list. Type flags filter the counts and the breakdown like any other mode. Same stdout,
same exit codes, same stderr (notices still print). Use `--plain` or `--json` when you
want the individual objects.

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

One record per finding on stdout, tab-separated, greppable — followed by one
`auto_date_time:<verdict>` record per auto date/time table. Type flags filter the
finding records; the `auto_date_time:` records print only when the section does (no
type flags, or `--tables` among them):

```
measure	'Sales'[Legacy Total]
column	'Sales'[Legacy]
auto_date_time:in_use	table 'LocalDateTable_9e0bbdfc-…'
auto_date_time:dead	table 'DateTableTemplate_0039983e-…'
```

## `--json`

Pretty-printed JSON, stable field order, additive schema:

```json
{
  "schema_version": 1,
  "target": "samples/AdventureWorks Sales.SemanticModel",
  "reports": ["samples/AdventureWorks Sales.Report"],
  "summary": {
    "objects": 130,
    "reachable": 74,
    "roots": 51,
    "unused": 56,
    "unused_total": 56,
    "ignored": 0,
    "auto_date_time": {
      "in_use": 0,
      "unused_by_reports": 0,
      "dead": 0
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

- `summary.unused` is the length of `unused` — after `[scan].ignore` and the type
  flags. `summary.unused_total` counts every unused object in the model before any
  suppression, filter, or section move, so `reachable = objects − unused_total` always
  holds and a consumer can tell a filtered-away finding from an absent one.
  `summary.ignored` counts objects suppressed by `[scan].ignore`.
- `summary.auto_date_time` counts the section's rows by verdict.
- Type flags filter the `unused` array and `summary.unused`; `summary.unused_total`
  stays model-wide. The `auto_date_time` array and `summary.auto_date_time` counts
  follow the section rule: present in full when no type flags are passed or `--tables`
  is among them, empty and zero otherwise.
- `auto_date_time` carries one row per `LocalDateTable_*`/`DateTableTemplate_*` table:
  `verdict` is `in_use`, `unused_by_reports`, or `dead`; `source_column` names the
  varied user column the machinery serves (`null` when none resolves, e.g. the
  template); `finding` is the table's own unused finding — with its `used_by` chain —
  present exactly when `verdict` is `"dead"` (the row moved here from `unused`). Rows
  suppressed by `[scan].ignore` are absent entirely.
- `type` is one of `table`, `column`, `measure`, `hierarchy`, `partition`,
  `relationship`, `role`, `calculation_item`, `expression`, `function`,
  `report_measure`.
- `table` is the finding's model table, quoted (`'Sales'`) — a relationship reports
  its "from" side. It is `null` for the kinds with no model table (`role`,
  `expression`, `function`, `report_measure`). `--plain` deliberately omits it: its
  records are a two-column grep contract.
- `provenance` is the human phrase for how the use is made (e.g. `measure expression`,
  `field well 'Y' — visual 'V' on page 'P' in report 'R'`, `hierarchy level`).
- `named_in_power_query` lists the M expressions (partitions by their table, shared
  expressions by name) that mention the column — supply-chain context, never a
  consumer. Empty for every non-column finding and for columns no M step names.
- `skips.notices` carries `{path, location, kind, detail}` per parser skip; `kind` is
  one of `unknown_object`, `unknown_property`, `malformed_value`, `unresolved_alias`,
  `stale_state`, and — in `--model` mode — `unresolved_dataset_reference` (a report item
  under a search folder with no usable `datasetReference`) and `malformed_report_item`
  (an anchor-less `.Report` folder the search walk pruned). Under `--strict`,
  `count > 0` corresponds to exit code `2`.

## `ripbi.toml`

Found in the working directory or its nearest ancestor; relative paths resolve against
the file's own directory. Flags override the file; the file overrides discovery.

```toml
target = "samples/AdventureWorks Sales.SemanticModel"  # used when no PATH is given
reports = ["samples/AdventureWorks Sales.Report"]      # extra roots when discovery finds none

[scan]
ignore = ["'*Time Intelligence'[*]", "*Legacy*"]       # object-name globs, never reported unused
```

`ignore` patterns are case-insensitive globs where `*` matches any run of characters
and `?` exactly one; everything else (quotes and brackets included — they appear in
display ids) is literal. A pattern matches a finding when it matches the full display
id (`'Sales'[Draft Amount]`) or the bare object name. Suppressed objects are excluded
from the output and the exit code, and counted in `summary.ignored`. The suppression
applies before the type flags: an object matched by both is simply gone.

## Validation

`scan`'s findings on three committed samples are pinned against external unused-objects
analyses —
[`tests/fixtures/adventure-works-baseline.txt`](../tests/fixtures/adventure-works-baseline.txt),
[`tests/fixtures/regional-sales-baseline.txt`](../tests/fixtures/regional-sales-baseline.txt),
and
[`tests/fixtures/artificial-intelligence-baseline.txt`](../tests/fixtures/artificial-intelligence-baseline.txt):
every object a baseline marks dead is a finding with the same chain shape, and no live
object is ever flagged. The accepted deltas are documented in each fixture header: the
dead `Time Intelligence` field-parameter cluster on Adventure Works; fully-dead
relationship-only tables the exports don't list as rows (a table referenced by nothing
but a relationship is unused by the documented containment rule); and, on the
Artificial Intelligence sample, the auto date/time machinery of the five date columns
whose hierarchies no visual binds. The one bound table — the report's date hierarchy
over `'Opportunity Calendar'[Date]`, resolved through the model's variation
declaration — is fully live, its columns are gone from the findings, and its
`in use` verdict (with the other five tables' verdicts) is pinned by the same test
through the `auto_date_time` section. One conservatism policy
the external analyses do not share, visible in that baseline: bookmark saved filters
count as bindings (re-applying a bookmark re-binds its fields) — but only for
sections whose page still exists. Power BI leaves deleted pages' sections inside
bookmarks forever, so those sections are skipped as stale (a `stale_state`
notice, surfaced by `--strict`), and the columns their filters were the last
consumers of surface as ordinary findings; on the Artificial Intelligence
sample that closes the last bookmark-kept-alive delta with the export, at the
cost of a documented cascade (the fully-dead `'Cases'` and `'Case Calendar'`
tables and everything chained under them). Inactive relationships
are the opposite correction: they are live only when a live `USERELATIONSHIP` reference
activates them, so an unactivated one is a finding itself, with its key columns chained
under it — `only used by relationship … (also unused)`.

A fourth, uncommitted validation ran against a large production model (≈3.8k graph
objects, 14 reports, ≈2.5k columns/measures measured externally): 99.3% of the
externally-dead objects were findings with identical chain shape, and — the direction
that matters — of the objects `scan` flags that the external analysis calls live,
**none** had a live consumer. Every one was a member of a chain where every consumer
was itself unused: auto date/time clusters no report binds, and active relationships
between otherwise-dead tables. The one other historical divergence — columns referenced
only inside Power Query — closed in both directions with the M lexing of issue #39:
the pipeline is M → tables/columns → DAX → reports, so an M mention of a column is its
supply chain, not a consumer, and those columns surface as findings again, each
carrying `named_in_power_query` so the script-side steps can be cleaned up alongside.
What M *does* keep alive is what deletion would break: shared expressions and
merge-source tables named by other queries' M. Two external-analysis blind spots
surfaced the same run: a measure bound only by a
drillthrough filter on a hidden page (counted live here; the external tool skipped
hidden pages), and report-level measures the external tool judges by view telemetry,
which static analysis deliberately ignores.

## Known boundaries

- The type flags cover the ten rendered groups; a `role` finding has no flag and is
  filtered out whenever any type flag is passed.
- Only TMDL semantic models and PBIR reports can be ingested; `.pbix`/`.pbit`/
  `model.bim` are recognized and refused with a clear error until their ingestors land.
- Analysis covers only the ingested reports. External consumers — thin reports, Excel
  (Analyze in Excel), XMLA reads, other datasets' DAX — are invisible; scan prints this
  caveat on every run.
- A model-only scan is refused: with no report bindings (and no RLS roles) everything
  is formally unused, which is never the answer the user wants. Pass `--report`. In
  `--model` mode the same refusal lists how many report items were bound to other models
  and how many had unresolved dataset references.
- Folder-walking requires `--model`: a plain `--report` folder without it is
  rejected with a hint naming the mode switch (in PATH mode `--report` accepts
  report items only).
- `byConnection` pairs by dataset *name* only (case-insensitive `initial catalog` against
  the model's `.platform` display name or item stem). Service `semanticmodelid` GUIDs and
  local `.platform` `logicalId` GUIDs are disjoint namespaces, so no static GUID match
  exists; report items that name a different dataset are excluded, and ones whose shape
  cannot be matched are unresolved notices.
- A multi-model search folder includes only the reports bound to the `--model` target;
  reports bound to other models are listed and skipped.
