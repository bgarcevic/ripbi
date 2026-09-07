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
| `0` | Clean: nothing unused (objects suppressed by `[scan].ignore` count as handled) |
| `1` | Unused objects found |
| `2` | Error: usage, bad PATH, model-only input, unsupported archive, ingestion failure, ambiguous discovery off-TTY — or any skip notice under `--strict` |

## Flags

| Flag | Effect |
|---|---|
| `--json` | JSON on stdout (schema below). Mutually exclusive with `--plain` |
| `--plain` | One `<type>\t<id>` record per finding, for grep/awk |
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
    object alive (a key column held only as a relationship endpoint).

## `--plain`

One record per finding on stdout, tab-separated, greppable:

```
measure	'Sales'[Legacy Total]
column	'Sales'[Legacy]
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
    "ignored": 0
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
      ]
    }
  ],
  "skips": {
    "count": 0,
    "notices": []
  }
}
```

- `summary.unused` is the length of `unused`; `summary.ignored` counts objects
  suppressed by `[scan].ignore`; `reachable = objects − (unused + ignored)`.
- `type` is one of `table`, `column`, `measure`, `hierarchy`, `partition`,
  `relationship`, `role`, `calculation_item`, `expression`, `function`,
  `report_measure`.
- `provenance` is the human phrase for how the use is made (e.g. `measure expression`,
  `field well 'Y' — visual 'V' on page 'P' in report 'R'`, `hierarchy level`).
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

`scan`'s findings on the committed Adventure Works sample are pinned against an
external unused-objects analysis in
[`tests/fixtures/adventure-works-baseline.txt`](../tests/fixtures/adventure-works-baseline.txt):
every object the baseline marks dead is a finding with the same chain shape, no live
object is ever flagged, and the only accepted extras are the dead `Time Intelligence`
field-parameter cluster (table, partition, and its calculation items) — nothing
anywhere references them, so ripbi reports them.

## Known boundaries

- Only TMDL semantic models and PBIR reports can be ingested; `.pbix`/`.pbit`/
  `model.bim` are recognized and refused with a clear error until their ingestors land.
- Analysis covers only the ingested reports. External consumers — thin reports, Excel
  (Analyze in Excel), XMLA reads, other datasets' DAX — are invisible; scan prints this
  caveat on every run.
- A model-only scan is refused: with no report bindings (and no RLS roles) everything
  is formally unused, which is never the answer the user wants. Pass `--report`.
