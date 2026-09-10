# `ripbi scan` output

The user-facing contract for the `scan` command: what each output mode prints, which
stream it goes to, and what the exit codes mean. `render.rs` implements this; change
the two together.

```
ripbi scan [PATH] [flags]
```

`PATH` is a `.pbip` file, a project folder, a `.SemanticModel` item folder, or a
`.Report` item folder. Without PATH (and without `target` in `ripbi.toml`), scan
discovers projects in the current directory: one candidate is announced and scanned,
several prompt with a numbered picker (on a TTY stdin only — otherwise the scan fails
listing them), none is an error.

## Streams

| Content | Stream | Notes |
|---|---|---|
| Findings, summary, JSON, plain records | stdout | the machine-readable side |
| Discovery/selection announce, scanning line | stderr | one line each |
| Coverage caveat | stderr | once per run |
| Skip notices (parser drift) | stderr | grouped under one header; suppressed in `--json` mode, where the JSON carries them |
| Errors + hints | stderr | `error: …` / `hint: …` |

`-q/--quiet` suppresses everything on both streams; the exit code is the only output.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Clean: nothing unused, and no auto date/time table unused by reports or dead (objects suppressed by `[scan].ignore` count as handled; an *in use* auto date/time table is informational) |
| `1` | Unused objects found, or auto date/time machinery no report binds |
| `2` | Error: usage, bad PATH, model-only input, unsupported archive, ingestion failure, ambiguous discovery off-TTY — or any skip notice under `--strict` |

## Flags

| Flag | Effect |
|---|---|
| `--json` | JSON on stdout (schema below). Mutually exclusive with `--plain` and `--summary` |
| `--plain` | One `<type>\t<id>` record per finding, for grep/awk |
| `-s`, `--summary` | Counts only: the summary line and per-type totals, no findings list. Mutually exclusive with `--json` and `--plain` |
| `-q`, `--quiet` | No output; exit code only |
| `--report <PATH>` | Extra report root; repeatable. Replaces `reports` from `ripbi.toml` |
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
    ⭘ Power Query also names it ('Customer' partition) — safe to stop loading; removing it from the script means editing those steps too
```

- The summary line: total graph objects, how many reachability reached, from how many
  report binding roots, and the unused count. Roots are report bindings; RLS roles also
  seed reachability without counting here. When `[scan].ignore` suppressed objects, a
  second line says how many.
- Findings are grouped by object type (measures, columns, hierarchies, tables,
  partitions, relationships, calculation items, expressions, functions, report
  measures — fixed order, empty groups omitted), sorted by object identity.
- Chain annotations, one per referencing object:
  - `← nothing references it` — a true orphan, deletable outright;
  - `← only used by 'X' — <where> (also unused)` — the sole (or all-identical case:
    every) consumer is itself unused, so the whole chain can go;
  - `← used by 'X' — <where>` — the consumer is live but its use could not keep this
    object alive (a key column held only by an active relationship endpoint, or the
    table of an inactive relationship nothing activates).
- The `⭘ Power Query also names it (…)` annotation appears on Data columns named by M
  expressions. It is supply-chain context, not a consumer: unloading the column cannot
  break refresh, but removing it from the Power Query script *entirely* means editing
  each named partition or expression too. Columns without the annotation are also gone
  from every Power Query step — and engine-computed columns (calculated columns, auto
  date/time machinery) never carry it, because an M step can only name a column it
  produces.
- The **Auto date/time** section follows the findings: one verdict per
  `LocalDateTable_*`/`DateTableTemplate_*` table, naming the user's date column the
  machinery serves. It is a *provenance* verdict, not a reachability one — the engine's
  own relationship keeps the machinery alive, so "alive" says nothing:
  - `in use — replace with a real date table` — a report binding lands on the
    machinery (usually a visual's date hierarchy over the varied column). Informational;
    it never fails the exit code.
  - `unused by reports — disable auto date/time` — nothing binds it, yet reachability
    keeps it alive: pure bloat the findings list cannot express, because the object is
    not dead.
  - `dead` — nothing reaches it at all; the table's own finding (with its chain
    annotations) is filed here instead of the generic `Tables` group.

## `--summary`

The human mode for big models: the summary line and one `label: count` line per
non-empty group, plus one `Auto date/time:` line when the model has such tables
(`Auto date/time: 1 in use, 2 unused by reports, 5 dead`), with no findings list. Same
stdout, same exit codes, same stderr (notices still print). Use `--plain` or `--json`
when you want the individual objects.

```text
3781 objects, 1207 reachable from 2962 roots, 2574 unused

Measures: 214
Columns: 2211
Report measures: 149
```

## `--plain`

One record per finding on stdout, tab-separated, greppable — followed by one
`auto_date_time:<verdict>` record per auto date/time table:

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

- `summary.unused` is the length of `unused`; `summary.ignored` counts objects
  suppressed by `[scan].ignore`; `reachable = objects − (unused + ignored +
  summary.auto_date_time.dead)` — a dead auto date/time table's own row moves out of
  `unused` into the section below, and `summary.auto_date_time.dead` counts those rows.
- `summary.auto_date_time` counts the section's rows by verdict.
- `auto_date_time` carries one row per `LocalDateTable_*`/`DateTableTemplate_*` table:
  `verdict` is `in_use`, `unused_by_reports`, or `dead`; `source_column` names the
  varied user column the machinery serves (`null` when none resolves, e.g. the
  template); `finding` is the table's own unused finding — with its `used_by` chain —
  present exactly when `verdict` is `"dead"` (the row moved here from `unused`). Rows
  suppressed by `[scan].ignore` are absent entirely.
- `type` is one of `table`, `column`, `measure`, `hierarchy`, `partition`,
  `relationship`, `role`, `calculation_item`, `expression`, `function`,
  `report_measure`.
- `provenance` is the human phrase for how the use is made (e.g. `measure expression`,
  `field well 'Y' — visual 'V' on page 'P' in report 'R'`, `hierarchy level`).
- `named_in_power_query` lists the M expressions (partitions by their table, shared
  expressions by name) that mention the column — supply-chain context, never a
  consumer. Empty for every non-column finding and for columns no M step names.
- `skips.notices` carries `{path, location, kind, detail}` per parser skip; `kind` is
  one of `unknown_object`, `unknown_property`, `malformed_value`, `unresolved_alias`.
  Under `--strict`, `count > 0` corresponds to exit code `2`.

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
from the output and the exit code, and counted in `summary.ignored`.

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
count as bindings (re-applying a bookmark re-binds its fields). Inactive relationships
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

- Only TMDL semantic models and PBIR reports can be ingested; `.pbix`/`.pbit`/
  `model.bim` are recognized and refused with a clear error until their ingestors land.
- Analysis covers only the ingested reports. External consumers — thin reports, Excel
  (Analyze in Excel), XMLA reads, other datasets' DAX — are invisible; scan prints this
  caveat on every run.
- A model-only scan is refused: with no report bindings (and no RLS roles) everything
  is formally unused, which is never the answer the user wants. Pass `--report`.
