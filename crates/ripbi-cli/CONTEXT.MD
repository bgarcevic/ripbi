# ripbi-cli — CONTEXT

The `ripbi` binary. A thin front end over `ripbi-app` (discovery, config, and
the scan, report, and deps pipelines, the scan verdict, and the JSON, SARIF,
`##vso`, Markdown, and tx-rm writers) and `ripbi-core`: argument parsing
(clap), human-readable output, and exit codes. `compare`, `config`,
`discover`, `error`, `glob`, `markdown`, `sarif`, `tx_rm`, and `vso` are
re-exported from `ripbi-app`, so `ripbi_cli::config` and friends keep working; see
[ripbi-app/CONTEXT.MD](../ripbi-app/CONTEXT.MD) for them.

## Rules

- **No analysis or orchestration logic here.** Ingestion, lexing, and graph work
  live in `ripbi-core`; target resolution and finding bucketing live in
  `ripbi-app`. This crate maps flags to requests and presents results.
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
  else it lists is informational and never gates. `fetch` is `0` or `2`: `2` for
  any error and for an incomplete report set nobody accepted (`--allow-incomplete`
  or the prompt); with `--scan`, a fetched folder returns the scan's code.
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
| `entry.rs` | process entry shared by the `ripbi` and `rib` binaries: real stdio → one command → exit code → the update notifier hook |
| `scan.rs` | `ripbi scan` over `ripbi_app::scan`: `request` maps `ScanArgs` to a `ScanRequest` (`--compare-root` adds the same request rebased onto the other checkout); `CliObserver` draws the pipeline's stages on the ticker, its notes as `·`/`!` lines, the pairing header (`pairing_announce`, capped to one line per fact; `--verbose` expands it), and the TTY picker; then the outcome renders in the chosen mode and `ScanOutcome::verdict` maps to the exit code. `CliObserver` is shared with `report` and `deps` (`unpaired` draws `report --allow-no-model`'s header). `write_skip_notices`, `Notes`, `consumer_hint`, and the side outputs (`--sarif-file`, `--json-file`, `--markdown-file`) stay here: they are presentation. Every byte on stdout and stderr matches the pre-split binary
| `report.rs` + `report/render.rs` | `ripbi report` (issue #33) over `ripbi_app::report`: `request` maps `ReportArgs` to a `ReportRequest`; the app resolves, ingests, attributes `broken_bindings`, applies the filters, and computes `--used` and the empty-filter note (`empty_note`) — see `ripbi-app/CONTEXT.MD`. What stays here is presentation: four list views (`--pages`/`--visuals`/`--fields`/`--used`) hand-roll aligned tables via one `write_table` helper; the six glob filters prune the inventory for every mode. Human tree (connected pages and visuals, `filters (n)`/`unresolved (n)` group nodes, per-block label columns), `--plain` typed records, `--json` `schema_version: 1` (the inventory types and writer are `ripbi_app::report`'s, re-exported; contract: `docs/report.md`) |
| `deps.rs` + `deps/` | `ripbi deps` (issue #20) over `ripbi_app::deps`: `request` maps `DepsArgs` to a `DepsRequest`; the app resolves, ingests, and projects the slices (`DepsOutput`) — see `ripbi-app/CONTEXT.MD`. What stays here is presentation. `deps/tree.rs` projects a slice into the human tree (`↩ already shown`, `↺ cycle`, a fixed expansion budget with an explicit footer); `deps/graph_view.rs` draws the `--graph` topology diagram (BFS layers, barycenter rows, deterministic, degrades past 24 nodes); `deps/render.rs` owns all three output modes — human, `--plain`, `--json` `schema_version: 1` (the writer is `ripbi_app::deps::json`, re-exported; contract: `docs/deps.md`), which never truncate. The Impact view's `Kept` section (`kept` records / JSON array) names the `ripbi_keep` annotations on the impact slice (`DependencyGraph::kept_by`, issue #151); the consumer and report filters drop it. The `Queried` section after it (`queried` records / JSON array) names the objects `--queries-from` logged queries reference (`DependencyGraph::queried_by`), with counts, never user names; the same filters drop it. Filters (`--consumer`/`--in-report`/`--on-page`) restrict results after full traversal, in the app |
| `fetch.rs` + `fetch/` | `ripbi fetch` over `ripbi_app::fetch` (contract: `docs/fetch.md`): maps `FetchArgs` to a `FetchRequest` (`interactive` = stdin TTY, no `--no-input`, no `-q`) and `--auth` to the app's `token_source`; its `CliObserver` prints the sign-in line and the forecast (`fetch/render.rs`), runs the `picker`s and the incomplete-set `confirm`, and starts the `progress` ticker at the first export. Then `fetch/render.rs`'s summary, the kept-`ripbi.toml` warning, the folder on stdout, and exit `2` when the outcome is `refused`. `--scan` runs `scan::run_in` from inside the folder. `run_with` injects the `Service` (tests use `ripbi_service::fake`) |
| `stub.rs` | `ripbi stub-report` (issue #130): resolves a `.SemanticModel` (PATH, its `definition/`, or the only one in the cwd), writes `ripbi_core::report_stub` into a fresh temp folder (or `--out`) with a relative `byPath` back to the model (canonical long paths; another drive is exit `2`), prints the `.pbip` path on stdout, opens it in Desktop when interactive on Windows (`should_open`; `--no-open`), and with `--wait` polls `.pbi/cache.abf` until a new save settles (`wait_for_save`). `--out` never overwrites: an existing `.Report`/`.pbip` is exit `2` unless `--force`, and `--force` replaces only a ripbi stub, never a real report. `scan` names stubs in a note and treats a model whose only reports are stubs like one with no reports (refusal, or the `--allow-no-reports` skip) |
| `update.rs` | `ripbi update`: GitHub release lookup (ureq), `sha256sums.txt` verification, tar.gz/zip extraction, channel detection, atomic replacement of `ripbi`/`rib` — the running binary last, and on Windows a locked target (the install scripts hard-link `rib` to `ripbi`) is moved aside to `<name>.old` instead of failing. `ReleaseClient` is the network seam; `run_in_with_home` injects the exe and cargo home |
| `picker.rs` | The one picker: `scan`/`report`/`deps` project discovery and `fetch` workspace → model. Stdin and stderr both TTYs: `dialoguer` fuzzy select drawn on the console (a bare `\r` through `Streams.err` first holds the `progress` ticker off; the echoed choice releases it). Otherwise a numbered list on stderr read with an unbuffered `read_line`, so two prompts share one stdin — the path tests drive. Also `confirm` (`[y/N]`) |
| `notify.rs` | the ambient daily check: XDG/LOCALAPPDATA state file, detached `__update-check` child, the `after_command` notice hook, and env/CI/TTY/`-q` suppression. `NotifyContext` is the test seam |
| `render.rs` | the three output modes: human, `--plain`, `--json` (the scan DTOs, `KINDS`, `kind_of`, and the JSON writer are `ripbi-app`'s `output.rs`/`json.rs`, re-exported here) — plus the Auto date/time section rendered in all of them, and the storage sizes of PBIX/`.abf` findings (issue #122: `(1.2 MB)` beside a finding, the `Unused storage:` total that `scan.rs` dedupes by owning table, the plain third field, the optional JSON fields — all absent without a storage catalog, so other inputs render byte-identically; `--sort size` reorders `findings` in `scan.rs`). `GROUPS` is the fixed kind order the type selection prints in; `KINDS` is the machine vocabulary `scan --type` and `deps --type`/`--consumer` validate against (locked to `kind_of` by test). A lone `--broken` uses `No broken reports or artifacts.` when both breakage sections are empty. `ScanOutput::kept` (`DependencyGraph::kept`, issue #151) is the JSON `summary.kept` count only — kept objects are roots, never findings, and the human modes list them nowhere. `Finding.table` (`ObjectId::owning_table`, set in `scan.rs` only for `--json`/`--summary`) feeds the JSON `table` field and `--summary`'s worst-tables breakdown |
| `style.rs` | The "pruning shears" ANSI palette, gated on TTY (or `CLICOLOR_FORCE`) / `NO_COLOR` / `TERM=dumb` / `--no-color`. Methods are roles, not colors: `warn` (rust — unused), `ok` (moss — clean), `alert` (brick — errors, breakage), `muted`/`dim` (warm greys — notes, annotations), `accent`, `bold`; `warn_note`/`info_note` draw `scan`'s tiered `  !`/`  ·` stderr notes (every model command reaches them through `scan::Notes` and opens with `scan::pairing_announce`). Truecolor when `COLORTERM`/`WT_SESSION`/`TERM_PROGRAM` says so, else xterm-256. Styling only ever adds color: uncolored text is the output contract. `help_styles()` maps the same inks onto clap's `--help`/usage errors (`cli.rs` `styles = …`; clap gates color itself) |
| `progress.rs` | The stage ticker `scan`, `report`, `deps`, and `fetch` (exports → write, with a `Progress::detail` counter) share on stderr (`◐ discover · model · reports · graph · findings  0.8s`); it clears when the run ends, and each command puts `Progress::elapsed` on its own closing line; each command passes its `Pipeline` (`SCAN`/`REPORT`/`DEPS`: stage names + closing verb). Runs only when stdout and stderr are TTYs, the stderr palette is on, and the mode is human without `-q` (`Progress::wanted`); appears after 120 ms. It starts before `resolve_target`, so the discovery walk is covered, and each command swaps its `Streams.err` for `Progress::writer` for the whole run: every write erases the line first, and a write that leaves a line open (the picker prompt) holds the ticker off. `entry.rs` keeps stderr unlocked so the ticker thread can draw |

## Routing

| Task | Read | Notes |
|---|---|---|
| UX conventions (help, output, flags, errors) | [../../docs/cli-ux-guidelines.md](../../docs/cli-ux-guidelines.md) | Follow for every CLI change |
| `scan` inputs, streams, exit codes | [docs/scan/overview.md](docs/scan/overview.md) | Entry page of the scan contract; its table routes to the other `docs/scan/` pages |
| `scan` flags | [docs/scan/flags.md](docs/scan/flags.md) | One row per flag; keep in sync with `cli.rs` |
| `scan` output modes | [docs/scan/human-output.md](docs/scan/human-output.md), [text-formats.md](docs/scan/text-formats.md), [json.md](docs/scan/json.md) | Human, `--summary`/`--plain`, and the JSON schema; keep in sync with `render.rs` |
| `scan` CI outputs | [docs/scan/ci-formats.md](docs/scan/ci-formats.md) | SARIF rules/locations/fingerprints, `--*-file`, Markdown, `##vso`; keep in sync with `ripbi-app`'s `sarif.rs`, `markdown.rs`, `vso.rs` |
| `scan` features | [compare.md](docs/scan/compare.md), [keep.md](docs/scan/keep.md), [queries.md](docs/scan/queries.md), [storage.md](docs/scan/storage.md), [tx-rm.md](docs/scan/tx-rm.md) | `--compare-root`, `ripbi_keep`, `--queries-from`, storage sizes and `stub-report`, `--emit tx-rm` |
| Validation and known boundaries | [docs/scan/boundaries.md](docs/scan/boundaries.md) | The baselines' accepted deltas; what scan cannot see |
| `ripbi.toml` | [docs/config.md](docs/config.md) | Every key, which commands read it; keep in sync with `ripbi-app`'s `config.rs` |
| The `report` command's output contract | [docs/report.md](docs/report.md) | Inventory shapes, plain records, JSON schema; keep in sync with `report/render.rs` and `ripbi-app`'s `report.rs` + `report/json.rs` |
| The `fetch` command: sign-in, forecast, folder layout, `source.json` | [docs/fetch.md](docs/fetch.md) | Keep in sync with `fetch.rs`, `fetch/` here and in `ripbi-app`; the network side is [ripbi-service](../ripbi-service/CONTEXT.MD) |
| The `deps` command's output contract | [docs/deps.md](docs/deps.md) | Tree/diagram/overview shapes, plain records, JSON schema, lookup and filter semantics; keep in sync with `deps/render.rs` and `ripbi-app`'s `deps.rs` + `deps/json.rs` |
| Validation baseline (Measure Killer) | tests/fixtures/adventure-works-baseline.txt | Header documents the format and the accepted deltas |
