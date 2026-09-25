# ripbi-cli — CONTEXT

The `ripbi` binary. Thin orchestration layer over `ripbi-core`:
argument parsing (clap), source-format auto-detection dispatch, human-readable and
CI-friendly output, and exit codes.

## Rules

- **No analysis logic here.** All ingestion, lexing, and graph work lives in
  `ripbi-core`; this crate only calls it and presents results.
- This is the only crate allowed to print to stdout/stderr or set exit codes.
- Exit codes: `0` clean, `1` unused objects found, auto date/time machinery no
  report binds, or — only under `--broken` — broken visual bindings or DAX artifacts (issues #60/#84:
  breakage is reported by default but gates only when selected) (CI gating), `2`
  error (usage, discovery, ingestion, or `--strict`
  skip notices). A model with no connected reports refuses with `2` unless
  `--allow-no-reports` skips it: a `Skipped …` notice on stderr, exit `0`, no
  stdout output. `update` reuses the same 0/1/2: `1` only from `--check` when a
  newer release exists; a completed self-update is `0`. `report` is informational:
  `0` for any produced inventory, `2` on error — it never gates on what it lists.
  `deps` shares that contract: `0` for any produced view (empty impact included),
  `2` on usage, discovery, lookup, or analysis error. `report --allow-no-model`
  trades the pairing refusal for a model-less listing (exit `0`); everything
  else it lists is informational and never gates.
- Results to stdout; messages, notices, and errors to stderr. `-q` suppresses both.
- `#![forbid(unsafe_code)]`.

## Layout

`src/lib.rs` wires the modules; `main.rs` only parses and maps the returned code to
`ExitCode`. The lib target exists so integration tests drive `scan` and `update`
without spawning a process (`Streams` injects stdout/stderr/stdin and the
stdin/stderr-are-TTY bits; `ReleaseClient` fakes the network).
The short alias `rib` (`src/bin/rib.rs`) shares `entry::run` with `main.rs`; the clap
command name is pinned to `ripbi` (`cli.rs`), so help and `--version` always say
`ripbi`, whichever binary was typed.

| Module | Role |
|---|---|
| `cli.rs` | clap definitions — the user-facing interface. Scan's selection resolves through `selected_kinds` (`--type`, including `bookmark`, validated against `render::KINDS`; `--broken` selects `broken_visual` and `broken_artifact` and is the only breakage gate). Two gates keep the surface consistent: help text stays one line ≤100 chars, and every command with a `--model` flag declares `--model`/`--report` first |
| `config.rs` | `ripbi.toml` discovery (cwd upward), parsing, path resolution |
| `discover.rs` | PATH classification, PBIP stem pairing, cwd candidate discovery; model target resolution (`.SemanticModel`/`definition/`/`model.tmdl` folders, the `.pbip` naming the project, and model files: `model.bim`, `.abf`, a PBIT/PBIX embedding its model — `is_model_file`; a model-bearing PBIX pairs with itself, a thin one with a sibling model) and bound-report folder walking (byPath → stem → `byConnection` name). Report→model pairing climbs stem sibling → sole `.SemanticModel` sibling → `definition.pbir` `byPath` → `byConnection` dataset name (pairing only when exactly one sibling model's stem or `.platform` display name carries it); a project with no model folder of its own pairs through its stem-named report's reference |
| `entry.rs` | process entry shared by the `ripbi` and `rib` binaries: real stdio → one command → exit code → the update notifier hook |
| `scan.rs` | orchestration: resolve → ingest (core) → graph (core) → ignore filter → type filter → render. Auto date/time verdict rows are assembled here (a dead table's own finding moves under its verdict; the section prints only when no type filter runs without `--type table`). Broken bindings (`graph.broken_bindings`) are bucketed here too: `[scan].ignore` → hidden-by-type-selection → suppressed (model ingest reported `unknown_object` skips — the issue #60 clean-ingest bar: only drift that can hide a name) → reported; gating is `--broken`-only and deliberately distinct from reporting. The zero-connected-report refusal (exit `2`) and the `--allow-no-reports` skip (`Skipped …`, exit `0`) share one detail helper, so both explain the empty walk identically. The pairing announce is capped to one line per fact in every mode (`--verbose` expands it). Also owns `resolve_target` — the one target-resolution entry point `scan`, `report`, and `deps` share (`--model` mode, else PATH/config target plus `--report` anchors, else model derivation from `--report` anchors alone, else cwd discovery); a new command calls it and inherits every input shape. Skip-notice helpers (`dedupe`, `skip_notice_out`, `write_skip_notices`) stay here too |
| `report.rs` + `report/render.rs` | `ripbi report` (issue #33): the report inventory — pages → visuals → fields — as data. Resolves and ingests exactly like `scan`, builds the graph to read `broken_bindings` (attributed to visuals, pages, and the report by name; the same `unknown_object` suppression bar) and reachability (`--used`). Four list views (`--pages`/`--visuals`/`--fields`/`--used`) hand-roll aligned tables via one `write_table` helper; six glob filters (`--page`/`--visual`/`--site`/`--kind`/`--match`/`--broken`) prune the inventory and recompute counts for every mode. Human tree (connected pages and visuals, `filters (n)`/`unresolved (n)` group nodes, per-block label columns), `--plain` typed records, `--json` `schema_version: 1` (contract: `docs/report.md`). `--allow-no-model` inventories a report with no paired model — report-side listing only, `--used`/`--broken` refused; it rescues the typed no-model pairing failures (see `error.rs`) never mistyped paths |
| `deps.rs` + `deps/` | `ripbi deps` (issue #20): dependency and impact exploration. Resolves and ingests exactly like `scan` through the shared target-resolution helpers — **minus the zero-report refusal** (model-only exploration is valid). Analysis stays in core: `DependencyGraph::dependencies_of`/`impact_of` slices + `lookup::resolve_reference` (ambiguity lists candidates, typos offer the nearest match). `deps/tree.rs` projects a slice into the human tree (`↩ already shown`, `↺ cycle`, a fixed expansion budget with an explicit footer); `deps/graph_view.rs` draws the `--graph` topology diagram (BFS layers, barycenter rows, deterministic, degrades past 24 nodes); `deps/render.rs` owns all three output modes — human, `--plain`, `--json` `schema_version: 1` (contract: `docs/deps.md`), which never truncate. Filters (`--consumer`/`--in-report`/`--on-page`) restrict results after full traversal |
| `update.rs` | `ripbi update`: GitHub release lookup (ureq), `sha256sums.txt` verification, tar.gz/zip extraction, channel detection, atomic replacement of `ripbi`/`rib` — the running binary last, and on Windows a locked target (the install scripts hard-link `rib` to `ripbi`) is moved aside to `<name>.old` instead of failing. `ReleaseClient` is the network seam; `run_in_with_home` injects the exe and cargo home |
| `notify.rs` | the ambient daily check: XDG/LOCALAPPDATA state file, detached `__update-check` child, the `after_command` notice hook, and env/CI/TTY/`-q` suppression. `NotifyContext` is the test seam |
| `render.rs` | the three output modes: human, `--plain`, `--json` — plus the Auto date/time section rendered in all of them. `GROUPS` is the fixed kind order the type selection prints in; `KINDS` is the machine vocabulary `scan --type` and `deps --type`/`--consumer` validate against (locked to `kind_of` by test). A lone `--broken` uses `No broken reports or artifacts.` when both breakage sections are empty. `Finding.table` (`ObjectId::owning_table`, set in `scan.rs` only for `--json`/`--summary`) feeds the JSON `table` field and `--summary`'s worst-tables breakdown |
| `style.rs` | ANSI palette gated on TTY / `NO_COLOR` / `TERM=dumb` / `--no-color` |
| `glob.rs` | case-insensitive `*`/`?` matcher for `[scan].ignore` |

## Routing

| Task | Read | Notes |
|---|---|---|
| UX conventions (help, output, flags, errors) | [../../docs/cli-ux-guidelines.md](../../docs/cli-ux-guidelines.md) | Follow for every CLI change |
| Output formats (text/plain/JSON), exit codes, config | [docs/output.md](docs/output.md) | The full user-facing contract; keep in sync with `render.rs` |
| The `report` command's output contract | [docs/report.md](docs/report.md) | Inventory shapes, plain records, JSON schema; keep in sync with `report/render.rs` |
| The `deps` command's output contract | [docs/deps.md](docs/deps.md) | Tree/diagram/overview shapes, plain records, JSON schema, lookup and filter semantics; keep in sync with `deps.rs` + `deps/render.rs` |
| Validation baseline (Measure Killer) | tests/fixtures/adventure-works-baseline.txt | Header documents the format and the accepted deltas |
