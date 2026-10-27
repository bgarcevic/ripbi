# ripbi-app — CONTEXT

The application layer between `ripbi-core` and every front end. It turns "scan
this model against these reports" into typed results: it finds the model and
its reports, reads `ripbi.toml`, runs ingestion and the dependency graph
through core, and shapes what the graph finds into a `ScanOutput` (`scan`), an
`Inventory` (`report`), or a `DepsOutput` (`deps`), and writes the machine
formats (JSON, SARIF, `##vso`, Markdown, tx-rm). The CLI (`ripbi-cli`) adds
the human renderings and exit codes; the desktop UIs render them as widgets.

## Rules

- **Never prints, never exits.** Progress, notes, the pairing header, and the
  project picker go through the caller's `scan::Observer`; failures are
  `ScanError` values (message + hint). Formatting, color, and exit codes are
  the front end's.
- **Typed in, typed out.** `ScanRequest` holds what a front end asks for —
  no clap types — and `Scan`/`ScanOutcome` hold what came back. A new front
  end needs nothing from `ripbi-cli`.
- **Notes carry text, not style.** A `Note` is a level (`Info`/`Warn`) and a
  sentence; the CLI draws them as its `·`/`!` lines. Their text is part of the
  CLI's byte-for-byte output, so wording changes are output changes.
- **No analysis logic here either.** Ingestion, lexing, and graph work stay in
  `ripbi-core`; this crate decides which folders to feed it and how to bucket
  what it returns.
- **Native only.** This crate targets native front ends (CLI, desktop), not
  wasm: it may use `std::fs`, threads, processes, and `ripbi-service` freely,
  with no filesystem abstraction or wasm feature gates. A browser build, if one
  ever comes, starts from `ripbi-core` (see its CONTEXT), not from here.
- `#![forbid(unsafe_code)]`.

## Entry point

`scan::run(&ScanRequest, &mut dyn Observer) -> Result<Scan, ScanError>`; `&mut ()`
is a silent observer. `Scan::Skipped` is the `allow_no_reports` outcome; every
other result is `Scan::Done(ScanOutcome)`: the `output`, the findings that
already existed under a comparison (`existing`), source `locator` sites, the
`removable` ids for `--emit tx-rm`, the ingested `model`, the `config` read,
and the `selected` kinds. Observer calls arrive in pipeline order — compare
notes, `pick`, `stage(Model)`, `resolved`, storage notes, `stage(Reports)`,
stub notes, query-log notes, `stage(Graph)`, `stage(Findings)`, the sort note —
which is what keeps the CLI's stderr identical to the pre-split binary.

The other two commands follow the same shape, on the same `Observer`:

- `report::run(&ReportRequest, …) -> ReportOutcome` — the inventory, already
  narrowed by the request's filters, and whether a model paired. A model
  pairs → `resolved`; under `allow_no_model` with none → `unpaired` plus a
  warning note. `report::empty_note` is the "filters matched nothing" text.
- `deps::run(&DepsRequest, …) -> DepsOutcome` — the overview or the focused
  slices (`DepsOutput`), the skip notices (the deps JSON documents the graph,
  not the run, so the front end shows them), and whether a query log was
  read.

`ScanOutcome::verdict(strict)` is the gate every front end shares: `Clean`,
`Findings`, or `Incomplete` (`strict` with skip notices); breakage gates only
when the selection asked for it (issue #60). `broken_only` tells a renderer the
selection was breakage alone.

`fetch::run(&FetchRequest, &Service, &mut dyn fetch::Observer) -> FetchOutcome`
has its own observer: `signed_in`, `pick`, `forecast`, `confirm_incomplete`,
`exporting`, `writing`. The incomplete-set rule lives here, so no front end
can download a partial report set without asking or being told to.

`resolve_target` and `load_query_log` stay public for any new command.

## Layout

| Module | Role |
|---|---|
| `error.rs` | `ScanError`: a message plus an optional hint naming the fix, per `docs/cli-ux-guidelines.md` — what every front end shows on failure |
| `output.rs` | `ScanOutput` and its rows (`Finding`, `BrokenOut`, `AutoDateTimeRow`, `SkipNoticeOut`, `CompareOut`, …): the presentation-ready result every renderer reads. `KINDS` is the machine kind vocabulary `--type` validates against, locked to `kind_of` by test. Shared wording and grouping every renderer uses live here too: `GROUPS` (section order and labels), `reason_phrase`, `format_bytes`, `KEEP_HINT`, and `worst_tables` |
| `json.rs` | the scan JSON contract, schema v1 (`crates/ripbi-cli/docs/scan/json.md`): `ripbi scan --json`, `--json-file`, and the desktop export all write it |
| `sarif.rs` | `scan --sarif` (issue #142): the SARIF 2.1.0 log. A fixed `RULES` catalogue (ids are the contract, order is `ruleIndex`); a `Locator` that `scan.rs` feeds per finding (the TMDL declaration line from `ripbi_core::ingest::source_locations`, else the model file or the report's `report.json`); fingerprints hashed from `compare::Key`; and `Existing`: under `--compare-root`, `scan.rs` keeps the findings the other modes drop so they render as `external` suppressions. Relative URIs are anchored at the scan's cwd (`%SRCROOT%`) |
| `tx_rm.rs` | `scan --emit tx-rm`: the reported findings plus dead auto date/time tables as JSON Lines for tomix's batch `tx rm --from` — `{type, table?, name, id}` per line (`serde_json`, never hand-built; non-ASCII as `\uXXXX` so the pipe survives PowerShell re-encoding), an unnamed relationship as `{type, path}` in endpoint form, report measures and bookmarks as `# skipped` comments. No ordering: `tx` judges the whole set. Contract: `docs/scan/tx-rm.md` |
| `vso.rs` | `scan --azure-devops`: one `##vso[task.logissue]` line per finding, rendered from `sarif::issues` (the same rule ids, messages, and sites; `sourcepath` is cwd-relative). Drops `--compare-root` existing findings like the non-SARIF modes; escaping follows azure-pipelines-task-lib |
| `markdown.rs` | `scan --markdown-file` (issue #143): the Markdown job summary / PR comment — headline counts (new · fixed · existing under `--compare-root`), per-type totals, the findings and `Fixed since` tables capped at `MAX_ROWS`, and `output::worst_tables`. Rendered from `ScanOutput` alone; ids go through `code`, which escapes pipes for GFM tables\|` for GFM tables. The CLI writes it as a side output |
| `locate.rs` | `Locator`: the TMDL declaration line or report file behind each finding, keyed by its comparison fingerprint (SARIF and `##vso` point annotations at it); `Existing`: the findings a comparison kept for SARIF suppressions |
| `fetch.rs` + `fetch/layout.rs` | tenant fetch (contract: `crates/ripbi-cli/docs/fetch.md`), over `ripbi-service`. `token_source` picks the sign-in (`AuthMode`; env read through a closure so tests inject it); `resolve_model` takes a portal link, workspace/model names or ids (no workspace: search all, refuse on ambiguity), or the observer's `pick`. `downstream::discover` + optional `verify` reach the observer as the forecast; a non-downloadable model is an error, and an incomplete report set asks `confirm_incomplete` when interactive and is refused otherwise unless `allow_incomplete`. `layout.rs` checks the output dir (empty, an earlier fetch of the same model, or `force`), writes parts through `safe_join` (untrusted paths), skips `StaticResources/`, and writes `ripbi.toml` (marker line; a foreign one is kept) and the `Manifest`. `refused` marks a run where more reports failed than forecast and nobody accepted that |
| `manifest.rs` | `.ripbi/source.json`, the manifest `fetch` writes (schema in `crates/ripbi-cli/docs/fetch.md`); `scan` reads it for the `incomplete_report_set` skip notice |
| `scan.rs` | the pipeline (`run`, `ScanRequest`, `Observer`, `Scan`/`ScanOutcome`): resolve → ingest (core) → graph (core) → ignore filter → type filter → `ScanOutput`. Auto date/time verdict rows are assembled here (a dead table's own finding moves under its verdict; the section prints only when no type filter runs without `--type table`). Broken bindings (`graph.broken_bindings`) are bucketed here too: `[scan].ignore` → hidden-by-type-selection → suppressed (model ingest reported `unknown_object` skips — the issue #60 clean-ingest bar: only drift that can hide a name) → reported; gating is `--broken`-only and deliberately distinct from reporting. The zero-connected-report refusal (exit `2`) and the `--allow-no-reports` skip (`Skipped …`, exit `0`) share one detail helper, so both explain the empty walk identically. Also owns `resolve_target` — the one target-resolution entry point `scan`, `report`, and `deps` share (`--model` mode, else PATH/config target plus `--report` anchors, else model derivation from `--report` anchors alone, else cwd discovery); a new command calls it and inherits every input shape. Skip-notice helpers (`dedupe`, `skip_notice_out`) live here too. `attach_storage` applies the issue #129 storage source (`--stats-from` > `[scan].stats_from` > `.pbi/cache.abf`) after model ingest and emits its coverage/staleness notes; an explicit source that fails is an error, an auto-detected one only a note. `load_query_log` (shared with `deps`) reads the `--queries-from`/`queries_from` workspace-monitoring export for `DependencyGraph::build_with_queries`: an unreadable log is an error, its size and window are a note; a log covering several models (`ItemName`/`ItemId`) is narrowed to this one (`--queries-item`, else the model's folder/file name) with a note, and counts none of its queries when none matches; that and a single-model log under another name are `stale_state` skip notices (never an exit code, but `--strict` gates on them), and `coverage_caveat` rewords the caveat when a log is in play. Excel workbooks (issue #172) are direct `--report`/`reports` items only (`is_report_item`), never search-walk finds; `.xls`/`.xlsb` are refused (`binary_workbook_error`), a workbook alone cannot derive the model, and `ScanOutcome::workbooks` (and `DepsOutcome::workbooks`) tells the front end one was read |
| `report.rs` + `report/json.rs` | `ripbi report`'s inventory (issue #33): resolve like `scan`, build the graph to read `broken_bindings` (attributed to visuals, pages, and the report by name; the same `unknown_object` suppression bar) and reachability (`used`). The glob filters (`page`/`visual`/`site`/`kind`/`matches`/`broken`) prune the tree and recompute counts. `allow_no_model` inventories reports with no paired model — listing only, `used`/`broken` refused; it rescues the typed no-model pairing failures (`ScanError::is_no_model`), never mistyped paths. `json.rs` is the `schema_version: 1` contract (`crates/ripbi-cli/docs/report.md`) |
| `deps.rs` + `deps/json.rs` | `ripbi deps`'s exploration (issue #20): resolve like `scan` **minus the zero-report refusal** (model-only exploration is valid), then core's `dependencies_of`/`impact_of` slices and `lookup::resolve_reference` (ambiguity lists candidates, typos offer the nearest match). `--consumer`/`--in-report`/`--on-page` restrict results after full traversal; `kept` (`ripbi_keep`, issue #151) and `queried` (`--queries-from`) ride on the impact slice and drop under any filter. `json.rs` is the `schema_version: 1` graph contract (`crates/ripbi-cli/docs/deps.md`); the human trees and `--graph` diagram stay in the CLI |
| `config.rs` | `ripbi.toml` discovery (cwd upward), parsing, path resolution; `StatsSetting` is the shared `--stats-from`/`[scan].stats_from` value (`none` or a path); top-level `queries_from` is the query log both `scan` and `deps` read, and `queries_item` picks its model |
| `discover.rs` | PATH classification, PBIP stem pairing, cwd candidate discovery; model target resolution (`.SemanticModel`/`definition/`/`model.tmdl` folders, the `.pbip` naming the project, and model files: `model.bim`, `.abf`, a PBIT/PBIX embedding its model — `is_model_file`; a model-bearing PBIX pairs with itself, a thin one with a sibling model) and bound-report folder walking (byPath → stem → `byConnection` name). Report→model pairing climbs stem sibling → sole `.SemanticModel` sibling → `definition.pbir` `byPath` → `byConnection` dataset name (pairing only when exactly one sibling model's stem or `.platform` display name carries it); a project with no model folder of its own pairs through its stem-named report's reference |
| `compare.rs` | `scan --compare-root` fingerprints (issue #141): `Entry` (kind + display id, plus reason/site, report name, or verdict), matched case-folded through `Key` and never by path; `Detected` (the ordered map one scan fills) and `fixed`. `scan.rs` runs the "before" pass as the same scan in the other root (`baseline`: the caller's `CompareRequest` carries the rebased request, the observer stays silent, `probe` collects what it detected), fingerprints every finding before its own filters (so a filtered finding is never "fixed"), and drops existing ones after them, counting `existing` |
| `glob.rs` | case-insensitive `*`/`?` matcher for `[scan].ignore` |
