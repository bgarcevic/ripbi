# `ripbi scan`: find unused objects

Find model objects that none of the connected reports reach. These examples
run from the root of a clone of this repository; replace the sample path with
your own PBIP project:

```sh
ripbi scan "samples/AdventureWorks Sales.pbip"
ripbi scan "samples/AdventureWorks Sales.pbip" --summary
ripbi scan --model "samples/AdventureWorks Sales.SemanticModel" --report samples/
```

The first line of output counts objects, reachable objects, report bindings,
and unused objects. Findings below it name objects to investigate; a finding
does not prove deletion is safe when other consumers use the model. Exit `1`
means findings were reported, `0` means none were reported, and `2` means the
scan could not complete. See [what counts as unused](https://bgarcevic.github.io/ripbi/unused.html)
before cleanup.

This page covers inputs, streams, and exit codes. The rest of the scan
reference is split by topic:

| Page | What it covers |
|---|---|
| [Flags](flags.md) | Every `scan` flag in one table |
| [Human output](human-output.md) | The default output: findings, chain annotations, broken bindings, auto date/time |
| [`--summary` and `--plain`](text-formats.md) | Counts only, and one tab-separated record per finding |
| [JSON output](json.md) | The `--json` schema, field by field |
| [SARIF, Markdown, and Azure DevOps](ci-formats.md) | CI outputs: code-scanning annotations, job summaries, pipeline warnings |
| [Validation and known boundaries](boundaries.md) | How findings were checked, and what scan cannot see |

Task-oriented pages: [comparing against another checkout](compare.md),
[keeping objects on purpose](keep.md), [logged queries](queries.md),
[storage sizes](storage.md), [removing with tomix](tx-rm.md), and the
[`ripbi.toml`](../config.md) settings.

<!-- Maintainers: render.rs implements this contract; change both together. -->

## Inputs and pairing

```
ripbi scan [PATH] [flags]
ripbi scan --model PATH [--report PATH]... [flags]
```

`PATH` is a `.pbip` file, a project folder, a `.SemanticModel` item folder, or a
`.Report` item folder. A `.Report` pairs with its model by stem sibling
(`X.Report` beside `X.SemanticModel`), sole model sibling, its `definition.pbir`
path — or, when the reference is a `byConnection`, by dataset name, pairing only
when exactly one sibling model's stem or display name carries it. The same tiers
rescue a model-less project: `X.pbip` (or its project folder) with only `X.Report`
beside it pairs with the model that report's reference names. Without PATH
(and without `target` in `ripbi.toml`), scan discovers projects in the current
directory: one candidate is announced and scanned,
several prompt with a numbered picker (on a TTY stdin only — otherwise the scan fails
listing them), none is an error. When PATH (or the `ripbi.toml` `target`) names a
semantic model itself, plain `--report` folders are search folders exactly as with
`--model`; any other target accepts report items only.

`--model PATH` names the semantic model explicitly: a `.SemanticModel` folder, its
`definition/` folder, any folder directly containing `model.tmdl`, or the project's
`.pbip`. It disables
current-directory discovery and the `ripbi.toml` `target`, and reinterprets every
`--report` (or config `reports`) value that is not itself a report item as a **search
folder**: the folder is walked recursively for report items bound to the model. With no
report values at all, the model's parent folder is searched. A PATH that names a
semantic model gets the same search-folder walk for plain `--report` values, but keeps
its convention sibling pairing and does not gain a default search root. Reports pair
with the model
by their `definition.pbir` path first, then by the PBIP stem convention (`X.Report`
beside `X.SemanticModel`), then by the dataset name in a `byConnection` `initial
catalog`. A scan with no connected reports refuses with exit `2` and per-category
counts — unless `--allow-no-reports` turns the refusal into a skip: a
`Skipped …: no connected reports (…)` notice on stderr and exit `0`, with no stdout
output. A report written by `ripbi stub-report` binds nothing, so scan prints
`· X.Report is a ripbi stub (no bindings)` for it, and a model whose only
reports are stubs gets the same refusal or skip (issue #130).

With `--report` alone — no PATH, no `--model`, no config `target` — the model is
derived from the named reports' pairing (the same tiers as a `.Report` PATH, so a
`byConnection` report binds to the one sibling model its dataset name matches). Every
anchor must land on the same model, and the scan covers exactly the named reports — no
search-folder walk. A `--report` value that is a `.pbip` expands to its project's
reports.

## Streams

| Content | Stream | Notes |
|---|---|---|
| Findings, summary, JSON, plain records, SARIF, `##vso` commands, `--emit` lines | stdout | the machine-readable side |
| `--sarif-file`, `--json-file`, `--markdown-file` | the named file | written whatever stdout shows, `-q` included (see [Several outputs from one scan](ci-formats.md#several-outputs-from-one-scan)) |
| Discovery/selection announce, header line | stderr | one line each; the header names the model and counts the bound reports (`--verbose` gives the full path and names them) |
| Pairings made by a walk (`!` by-name matches, `· ignored … bound to other models` exclusions) | stderr | informational, never `--strict`-fatal; both collapse to one capped line each (`--verbose` lists every report) |
| Storage source notes (partial coverage, staleness, an unreadable `.pbi/cache.abf`) | stderr | only when a storage source applies; informational, never `--strict`-fatal (see [Storage sizes](storage.md)) |
| Skip notices (parser drift, stale saved state, unresolved dataset references, opaque native-query sources) | stderr | grouped under one header, above the report; suppressed in `--json` mode, where the JSON carries them |
| Consumer hint | stderr | under a verdict with findings, except `--json`; see [Human output](human-output.md) |
| Errors + hints | stderr | `error: …` / `hint: …` |

`-q/--quiet` suppresses everything on both streams; the exit code is the only output.
Files named by `--sarif-file`, `--json-file`, and `--markdown-file` are still written.

## Model-centric scans (`--model`, or a PATH naming a semantic model)

Whenever reports are discovered by walking search folders — under `--model`, or under a
PATH that names a semantic model with plain `--report` folders (issue #67) — the
pairings that would otherwise be invisible are summarized on stderr (all suppressed by
`-q`). The default reads one line per pairing fact; `--verbose` expands each into the
per-report audit trail:

```text
Sales.SemanticModel · 3 reports
  ! 1 report(s) matched by dataset name only (byConnection 'initial catalog' = 'Sales'): Thin.Report
  · ignored 1 report(s) bound to other models: HR.Report
```

- The header counts the connected reports — direct `--report` items included.
  `--verbose` shows the model's full path and appends the report folder names, in ingestion order: under `--model`,
  walked reports by canonical path, then explicitly passed ones; under a model-naming
  PATH, the convention siblings and explicit items first, then the walked ones.
- The `!` line flags reports connected by dataset *name* rather than by path or stem,
  because that pairing is weaker than the written `definition.pbir` path — a wrong
  pairing means wrong findings, so the fact always shows. One line per `initial catalog`,
  with the count and up to three names; when a tail is capped, the line points at
  `--verbose`:

  ```text
    ! 26 report(s) matched by dataset name only (byConnection 'initial catalog' = 'Sales'): Thin1.Report, Thin2.Report, Thin3.Report, … and 23 more — rerun with --verbose to list them
  ```

  `--verbose` lists one line per report instead, with its full path.
- The `· ignored …` line counts report items under the search folders that resolve to a
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
| `0` | Clean: nothing unused, no broken finding gates the run, and no auto date/time table unused by reports or dead (objects suppressed by `[scan].ignore` count as handled; an *in use* auto date/time table is informational) |
| `1` | Unused objects found, auto date/time machinery no report binds — or, under `--broken`, broken visual bindings or DAX artifacts found |
| `2` | Error: usage, bad PATH, model-only input, a `--model` search with no connected reports (unless `--allow-no-reports` skips it), unsupported archive, ingestion failure, ambiguous discovery off-TTY — or any skip notice under `--strict` |

Under `--compare-root`, findings that already existed in the other checkout are not
reported, so they cannot fail the run either (see
[Comparing against another checkout](compare.md)).

The exit code describes what was *reported*: findings hidden by type selection, and an
Auto date/time section hidden because `--type table` was not selected, cannot fail the
run. Broken visual bindings and DAX artifacts (issues #60/#84) are advisory kinds: they are
*reported* by default, but they gate the exit code only when `--broken` selects them —
an unused-only gate must not start failing because one visual is broken, and a
`--broken` gate must not fail on unused findings. `--strict` and `-q/--quiet` are
unaffected.
