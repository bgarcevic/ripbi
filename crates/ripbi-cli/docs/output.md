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
listing them), none is an error. When PATH (or the `ripbi.toml` `target`) names a
semantic model itself, plain `--report` folders are search folders exactly as with
`--model`; any other target accepts report items only.

`--model PATH` names the semantic model explicitly: a `.SemanticModel` folder, its
`definition/` folder, or any folder directly containing `model.tmdl`. It disables
current-directory discovery and the `ripbi.toml` `target`, and reinterprets every
`--report` (or config `reports`) value that is not itself a report item as a **search
folder**: the folder is walked recursively for report items bound to the model. With no
report values at all, the model's parent folder is searched. A PATH that names a
semantic model gets the same search-folder walk for plain `--report` values, but keeps
its convention sibling pairing and does not gain a default search root. Reports pair
with the model
by their `definition.pbir` path first, then by the PBIP stem convention (`X.Report`
beside `X.SemanticModel`), then by the dataset name in a `byConnection` `initial
catalog`. A scan with no connected reports refuses with exit `2` and per-category counts.

## Streams

| Content | Stream | Notes |
|---|---|---|
| Findings, summary, JSON, plain records | stdout | the machine-readable side |
| Discovery/selection announce, scanning line | stderr | one line each; the scanning line counts the bound reports (`--verbose` names them) |
| Pairings made by a walk (`Note:` by-name matches, `Ignored … bound to other models` exclusions) | stderr | informational, never `--strict`-fatal; both collapse to one capped line each (`--verbose` lists every report) |
| Coverage caveat | stderr | once per run |
| Skip notices (parser drift, stale saved state, unresolved dataset references) | stderr | grouped under one header; suppressed in `--json` mode, where the JSON carries them |
| Errors + hints | stderr | `error: …` / `hint: …` |

`-q/--quiet` suppresses everything on both streams; the exit code is the only output.

## Model-centric scans (`--model`, or a PATH naming a semantic model)

Whenever reports are discovered by walking search folders — under `--model`, or under a
PATH that names a semantic model with plain `--report` folders (issue #67) — the
pairings that would otherwise be invisible are summarized on stderr (all suppressed by
`-q`). The default reads one line per pairing fact; `--verbose` expands each into the
per-report audit trail:

```text
Scanning models/Sales.SemanticModel with 3 report(s)
Note: 1 report(s) matched by dataset name only (byConnection 'initial catalog' = 'Sales'): Thin.Report
Ignored 1 report(s) bound to other models: HR.Report
```

- The scanning line counts the connected reports — direct `--report` items included.
  `--verbose` appends the report folder names, in ingestion order: under `--model`,
  walked reports by canonical path, then explicitly passed ones; under a model-naming
  PATH, the convention siblings and explicit items first, then the walked ones.
- The `Note:` line flags reports connected by dataset *name* rather than by path or stem,
  because that pairing is weaker than the written `definition.pbir` path — a wrong
  pairing means wrong findings, so the fact always shows. One line per `initial catalog`,
  with the count and up to three names; when a tail is capped, the line points at
  `--verbose`:

  ```text
  Note: 26 report(s) matched by dataset name only (byConnection 'initial catalog' = 'Sales'): Thin1.Report, Thin2.Report, Thin3.Report, … and 23 more — rerun with --verbose to list them
  ```

  `--verbose` lists one line per report instead, with its full path.
- The `Ignored …` line counts report items under the search folders that resolve to a
  different existing model, with up to three names — capped the same way, with the same
  `--verbose` pointer; the flag lists every one. It is informational: those reports are
  not ingested, they
  appear in no output mode, and they never fail `--strict` — a healthy multi-model folder
  must stay scannable. Report items whose reference resolves to nothing become
  `unresolved_dataset_reference` skip notices instead, and a `*.Report` folder with no
  `report.json` anchor becomes a `malformed_report_item` notice; both *do* fail `--strict`.
- Reports passed explicitly with `--report` are taken at face value: they are never
  binding-checked and produce none of these notices. Under a model-naming PATH the
  convention siblings count as explicit in the same sense.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Clean: nothing unused, no broken visual binding gates the run, and no auto date/time table unused by reports or dead (objects suppressed by `[scan].ignore` count as handled; an *in use* auto date/time table is informational) |
| `1` | Unused objects found, auto date/time machinery no report binds — or, under `--broken`, broken visual bindings found |
| `2` | Error: usage, bad PATH, model-only input, a `--model` search with no connected reports, unsupported archive, ingestion failure, ambiguous discovery off-TTY — or any skip notice under `--strict` |

The exit code describes what was *reported*: findings hidden by the type flags, and an
Auto date/time section hidden because `--tables` was not among the passed flags, cannot
fail the run. Broken visual bindings (issue #60) are the one advisory kind: they are
*reported* by default, but they gate the exit code only when `--broken` selects them —
an unused-only gate must not start failing because one visual is broken, and a
`--broken` gate must not fail on unused findings. `--strict` and `-q/--quiet` are
unaffected.

## Flags

| Flag | Effect |
|---|---|
| `--json` | JSON on stdout (schema below). Mutually exclusive with `--plain` and `--summary` |
| `--plain` | One `<type>\t<id>` record per finding, for grep/awk |
| `-s`, `--summary` | Counts only: the summary line and per-type totals, no findings list. Mutually exclusive with `--json` and `--plain` |
| `-q`, `--quiet` | No output; exit code only |
| `-v`, `--verbose` | Full pairing audit trail on stderr: every report's name in the scanning line, one pairing note per by-name-matched report, the complete ignored-reports list. The default caps each to one line |
| `--model <PATH>` | Analyze one named semantic model (`.SemanticModel`, its `definition/`, or a folder holding `model.tmdl`). Disables cwd discovery and the `ripbi.toml` `target`; plain `--report` folders become search folders for reports bound to this model. Conflicts with `PATH` |
| `--report <PATH>` | Extra report root; repeatable. Replaces `reports` from `ripbi.toml`. When the target is `--model` or a PATH naming a semantic model, a folder that is not itself a report item is searched recursively for reports bound to the model |
| `--measures`, `--columns`, `--hierarchies`, `--tables`, `--partitions`, `--relationships`, `--calc-items`, `--expressions`, `--functions`, `--report-measures` | Report only unused objects of the passed types; repeatable, and passed together they union (`--measures --columns`). Filters every output mode and the exit code. With none of them, everything is reported |
| `--broken` | Report only broken visual bindings (issue #60) — field references that no longer resolve in the model. Unions with the type flags (`--broken --measures` gates on both); alone, it scopes the run to breakage so a pipeline can gate on it separately. Without `--broken` (and without any other type flag) breakage is still reported, but never changes the exit code. When the model ingest recorded `unknown_object` skips, breakage is suppressed entirely — see the precision bar under Human output |
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
  report binding roots, and the unused count. Roots are report bindings — the desktop
  tree and the phone layout (`definition.mobile/`, issue #49) alike; RLS roles also
  seed reachability without counting here. When `[scan].ignore` suppressed objects, a
  second line says how many; when the type flags hid findings, a third line counts them
  (`(2 unused hidden by type filters)`); and when auto date/time machinery members are
  covered by the section's table verdicts, a fourth counts them, so `0 unused` from a
  filtered or machinery-heavy model never reads as a clean one by accident.
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
- The **Broken visual bindings** section follows the findings (issue #60): one row per
  report binding whose written field reference resolves to nothing in the model, or
  that lands on an artifact whose own DAX no longer resolves — the static form of the
  error state the service would render. Each row names the written reference, the
  reason, and the binding's site:

  ```text
  Broken visual bindings (2)
    'Sales'[Color]
      ← field not found in the model — field well 'Values' — visual 'V2' on page 'P1'
    'Sales'[Broken Total]
      ← bound artifact 'Sales'[Broken Total] has unresolvable references — field well 'Values' — visual 'V3' on page 'P1'
  ```

  The section is a different verdict than reachability's — written references, not
  graph liveness — so it prints even on an otherwise clean `No unused objects.` scan,
  and it is *advisory*: it never changes the exit code unless `--broken` selects it.
  Hidden bindings (a type flag without `--broken`) and suppressed ones are accounted
  for in the summary's arithmetic lines: `(2 broken-visual bindings hidden by type
  filters)` and `(N possible broken-visual bindings suppressed — the model ingest
  reported skips, listed on stderr; --strict fails on those skips)`.

  The **precision bar is the mirror image of the unused findings'**: a "broken" claim
  is itself a breakage claim, so it fires only when nothing in the model ingest could
  have hidden the name the binding wrote. That is exactly the `unknown_object` skip
  kind — a table or column the parser skipped never became a node, so a "field not
  found" verdict would be a guess — and only that kind: an `unknown_property` notice
  means the object was parsed with its name regardless, so it does not suppress. The
  suppressed count rides on the summary line, and `--strict` surfaces the skips behind
  it. Report-side skips do not suppress either way: a half-parsed
  report can only under-report breakage, never fabricate it. The remaining
  under-claiming is deliberate:

  - a KPI visual's synthesized variants (`… Goal`, `… Status`, `… Trend`, `… Value`)
    resolve, not flag, whenever the base measure exists — the engine materializes them;
  - an artifact's own DAX resolving through query-time constructs never flags: a
    report measure referencing a sibling report measure (the graph's ordinary
    report-measure edge), `@`-prefixed extension columns (`ADDCOLUMNS(…, "@Krav", …)`
    read back as `[@Krav]`), and the column names query time introduces in the same
    expression — string-literal extension names (`SELECTCOLUMNS(t, "Ordning", …)`,
    `GROUPBY`) and the table constructor's fixed `Value`/`Value1…N` defaults.
    Verifying these needs DAX scope analysis the lexer deliberately does not do, so
    under-claim applies; the cost is that a typo coinciding with a string in the same
    measure goes unflagged;
  - a qualified field miss on a **calculated table** resolves when the partition
    expression makes the name lexically visible: string literals (`DATATABLE` headers,
    `ADDCOLUMNS`/`SELECTCOLUMNS` names), the constructor defaults, the columns written
    in the expression, and every column of the tables it references (the wrapped-table
    shape `FILTER`/`VALUES` passes through). A calculated table has no declared schema —
    its columns are what the expression returns — so only names visible *nowhere* in
    the expression flag; renaming a header out from under a binding still does;
  - field parameters resolve like any other field (#52), and auto date/time hierarchy
    references resolve through the variation machinery (#47); a variation-flavored
    hierarchy the machinery cannot resolve stays silent rather than risk calling
    serialization drift a breakage;
  - a measure referenced through a stale or wrong table qualifier still resolves
    (measure names are model-global), and a same-named hierarchy or calculation item
    behind a stale column name resolves too;
  - `Written` references the parser could not structure (legacy layouts, unresolved
    query aliases) resolve as always but never flag — the written form is too loose
    for a breakage claim.

  Bookmarks on deleted pages never reach this check: their sections are skipped as
  stale at ingestion. The phone layout binds identically to the desktop tree, so a
  broken binding there reports with `mobile layout …` provenance. A binding onto a
  broken artifact still counts as a root (it resolves — that is what it does), so the
  artifact itself is not reported unused by it; the artifact's *own* finding kind,
  bound or not, is issue #84's scope.
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

  The section is also the only place the machinery appears (issue #47): its unused
  members — the GUID-named columns, hierarchies, and partitions under those tables —
  are never generic findings, because they are not separately actionable. Removing the
  table removes them, and disabling auto date/time on the named column is the fix; the
  summary line accounts for them (`(41 unused auto date/time members covered by their
  tables' verdicts)`), and `--json` reports the count as
  `summary.auto_date_time.member_findings`.
- Auto date/time is a recommendation, not a law: a legacy model that keeps the feature
  can silence the section through the ordinary `[scan].ignore` globs —

  ```toml
  [scan]
  ignore = ["LocalDateTable_*", "DateTableTemplate_*"]
  ```

  Suppressed tables count as handled, so they cannot fail the exit code, and a dead
  table's own finding — suppressed with its row — counts in `summary.ignored`. The
  trade-off: the recipe also hides the
  `in use` tables' advice, which is the part worth reading when you plan the migration
  to a real date table.

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
`broken_visual:<reason>` record per reported broken binding (issue #60) — followed by one
`auto_date_time:<verdict>` record per auto date/time table. Type flags filter the
finding records (`--broken` selects the broken ones the same way); the
`auto_date_time:` records print only when the section does (no
type flags, or `--tables` among them):

```
measure	'Sales'[Legacy Total]
column	'Sales'[Legacy]
broken_visual:field_not_found	'Sales'[Color]
broken_visual:bound_artifact_broken	'Sales'[Broken Total]
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
    "broken": 2,
    "broken_total": 2,
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
      "provenance": "field well 'Values' — visual 'V2' on page 'P1'"
    },
    {
      "target": "'Sales'[Broken Total]",
      "reason": "bound_artifact_broken",
      "bound_artifact": "'Sales'[Broken Total]",
      "provenance": "field well 'Values' — visual 'V3' on page 'P1'"
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
  `summary.ignored` counts findings suppressed by `[scan].ignore` — unused objects and
  broken bindings alike. On a model with
  auto date/time machinery, the remaining gap between `unused` and `unused_total` is
  the machinery: `summary.auto_date_time.member_findings` counts its unused members
  and the `dead` verdict count its nested own findings.
- `summary.broken` is the length of `broken` — after `[scan].ignore`, the
  unknown-object suppression, and the type flags. `summary.broken_total` counts every
  broken binding
  detected, before any of those, so a consumer can tell a suppressed or filtered-away
  binding from an absent one (issue #60).
- `summary.auto_date_time` counts the section's rows by verdict, plus
  `hidden_tables` (every row, all verdicts together), `date_columns` (the distinct
  user date columns the machinery serves — the shared template serves none), and
  `member_findings` (the machinery's unused members covered by the rows, absent from
  `unused` individually).
- Type flags filter the `unused` array and `summary.unused`; `summary.unused_total`
  stays model-wide. The `broken` array and `summary.broken` follow the same rule:
  every type flag without `--broken` empties them, `--broken` keeps only them. The
  `auto_date_time` array and `summary.auto_date_time` counts
  follow the section rule: present in full when no type flags are passed or `--tables`
  is among them, empty and zero otherwise.
- `broken` (issue #60) carries one row per reported broken visual binding:
  `target` is the written field reference; `reason` is one of `table_not_found`,
  `field_not_found`, `measure_not_found`, `hierarchy_not_found`, `level_not_found`,
  or `bound_artifact_broken`; `bound_artifact` names the broken artifact the binding
  lands on (present exactly when the reason is `bound_artifact_broken`); `provenance`
  is the same binding-site phrase the unused findings' `used_by` entries carry,
  `mobile layout …` prefixed for phone-layout bindings. Bindings suppressed by the
  clean-ingest bar or by `[scan].ignore` are absent entirely; the totals above keep
  the arithmetic.
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
  `report_measure`.
- `table` is the finding's model table, quoted (`'Sales'`) — a relationship reports
  its "from" side. It is `null` for the kinds with no model table (`role`,
  `expression`, `function`, `report_measure`). `--plain` deliberately omits it: its
  records are a two-column grep contract.
- `provenance` is the human phrase for how the use is made (e.g. `measure expression`,
  `field well 'Y' — visual 'V' on page 'P' in report 'R'`, `hierarchy level`). A
  binding from the phone layout prefixes `mobile layout ` (`mobile layout field well
  'Y' — visual 'V' on page 'P' in report 'R'`), so an audit names the right surface.
- `named_in_power_query` lists the M expressions (partitions by their table, shared
  expressions by name) that mention the column — supply-chain context, never a
  consumer. Empty for every non-column finding and for columns no M step names.
- `skips.notices` carries `{path, location, kind, detail}` per parser skip; `kind` is
  one of `unknown_object`, `unknown_property`, `malformed_value`, `unresolved_alias`,
  `stale_state`, and — when reports are discovered by walking search folders —
  `unresolved_dataset_reference` (a report item
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
id (`'Sales'[Draft Amount]`) or the bare object name; for a broken binding it matches
the written reference whole (`'Sales'[Color]` — a binding has no bare name of its own).
Suppressed findings are excluded
from the output and the exit code, and counted in `summary.ignored`. The suppression
applies before the type flags: an object matched by both is simply gone. The auto
date/time machinery can be silenced wholesale this way — see the recipe under the Auto
date/time section.

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
whose hierarchies no visual binds — reported only through the `auto_date_time` section
(its members are covered by the per-table verdicts, and the baseline's machinery rows
are asserted *absent* from the generic findings). The one bound table — the report's
date hierarchy over `'Opportunity Calendar'[Date]`, resolved through the model's
variation declaration — is fully live, its columns are gone from the findings, and its
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

The phone layout is the same conservatism on the report side (issue #49): a report can
ship a `definition.mobile/` tree beside `definition/`, and its visuals render on
phones, so its bindings enumerate as roots exactly like the desktop tree's — a field
referenced only there never surfaces as a finding, because deleting it would break the
report for phone users. Its provenance reads `mobile layout …`, and the summary's root
count includes it. Page visibility stays display-only in the phone layout, as on
desktop; report-level state (report measures, bookmarks) remains desktop-only.

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
merge-source tables named by other queries' M. The incremental refresh policy
extends the same rule to refresh time (issue #53): its change-detection
expression and source expression are evaluated by the engine at every policy
refresh, so the measures and shared expressions they name — a measure-based
"detect data changes" polling expression, or the RangeStart/RangeEnd
parameters named only by the policy's source in the Desktop "Full DataView"
shape — stay live while the policy's partition is, with `change detection
expression` as the provenance; the policy's scalar vocabulary (periods,
granularities) names nothing and stays silent. Two external-analysis blind spots
surfaced the same run: a measure bound only by a
drillthrough filter on a hidden page (counted live here; the external tool skipped
hidden pages), and report-level measures the external tool judges by view telemetry,
which static analysis deliberately ignores.

## Known boundaries

- The type flags cover the ten object-type groups plus `--broken` (issue #60); a
  `role` finding has no flag and is filtered out whenever any type flag is passed.
  A broken binding's kind (`broken_visual`) also never appears in the `unused` array
  or the generic groups — it has no `ObjectId` of its own.
- Only TMDL semantic models and PBIR reports can be ingested; `.pbix`/`.pbit`/
  `model.bim` are recognized and refused with a clear error until their ingestors land.
- Analysis covers only the ingested reports. External consumers — thin reports, Excel
  (Analyze in Excel), XMLA reads, other datasets' DAX — are invisible; scan prints this
  caveat on every run.
- A model-only scan is refused: with no report bindings (and no RLS roles) everything
  is formally unused, which is never the answer the user wants. Pass `--report`. When
  search folders are walked, the same refusal lists how many report items were bound to
  other models and how many had unresolved dataset references.
- Folder-walking outside `--model` needs a PATH (or `ripbi.toml` `target`) that names a
  semantic model itself. Any other target — a `.pbip`, project folder, `.Report`, or a
  cwd-discovered project — rejects a plain `--report` folder with a hint naming the
  mode switch (`--report` accepts report items only).
- `byConnection` pairs by dataset *name* only (case-insensitive `initial catalog` against
  the model's `.platform` display name or item stem). Service `semanticmodelid` GUIDs and
  local `.platform` `logicalId` GUIDs are disjoint namespaces, so no static GUID match
  exists; report items that name a different dataset are excluded, and ones whose shape
  cannot be matched are unresolved notices.
- A multi-model search folder includes only the reports bound to the `--model` target;
  reports bound to other models are listed and skipped.
