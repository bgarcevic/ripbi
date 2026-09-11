# ripbi-cli — CONTEXT

The `ripbi` binary. Thin orchestration layer over `ripbi-core`:
argument parsing (clap), source-format auto-detection dispatch, human-readable and
CI-friendly output, and exit codes.

## Rules

- **No analysis logic here.** All ingestion, lexing, and graph work lives in
  `ripbi-core`; this crate only calls it and presents results.
- This is the only crate allowed to print to stdout/stderr or set exit codes.
- Exit codes: `0` clean, `1` unused objects found or auto date/time machinery no
  report binds (CI gating), `2` error (usage, discovery, ingestion, or `--strict`
  skip notices). `update` reuses the same 0/1/2: `1` only from `--check` when a
  newer release exists; a completed self-update is `0`.
- Results to stdout; messages, notices, and errors to stderr. `-q` suppresses both.
- `#![forbid(unsafe_code)]`.

## Layout

`src/lib.rs` wires the modules; `main.rs` only parses and maps the returned code to
`ExitCode`. The lib target exists so integration tests drive `scan` and `update`
without spawning a process (`Streams` injects stdout/stderr/stdin and the
stdin/stderr-are-TTY bits; `ReleaseClient` fakes the network).
The short alias `rib` (`src/bin/rib.rs`) shares `entry::run` with `main.rs`; clap
derives the displayed name from argv[0], so help says whichever name was typed.

| Module | Role |
|---|---|
| `cli.rs` | clap definitions — the user-facing interface. The type flags' `selected_kinds` is the one filter; its lockstep test pins the flag set to `render::GROUPS` |
| `config.rs` | `ripbi.toml` discovery (cwd upward), parsing, path resolution |
| `discover.rs` | PATH classification, PBIP stem pairing, cwd candidate discovery; `--model` target resolution and bound-report folder walking (byPath → stem → `byConnection` name) |
| `entry.rs` | process entry shared by the `ripbi` and `rib` binaries: real stdio → one command → exit code → the update notifier hook |
| `scan.rs` | orchestration: resolve → ingest (core) → graph (core) → ignore filter → type filter → render. Auto date/time verdict rows are assembled here (a dead table's own finding moves under its verdict; the section prints only when no type filter runs without `--tables`) |
| `update.rs` | `ripbi update`: GitHub release lookup (ureq), `sha256sums.txt` verification, tar.gz/zip extraction, channel detection, atomic replacement of `ripbi`/`rib`. `ReleaseClient` is the network seam; `run_in_with_home` injects the exe and cargo home |
| `notify.rs` | the ambient daily check: XDG/LOCALAPPDATA state file, detached `__update-check` child, the `after_command` notice hook, and env/CI/TTY/`-q` suppression. `NotifyContext` is the test seam |
| `render.rs` | the three output modes: human, `--plain`, `--json` — plus the Auto date/time section rendered in all of them. `GROUPS` is the fixed kind order the type flags select from. `Finding.table` (`ObjectId::owning_table`, set in `scan.rs` only for `--json`/`--summary`) feeds the JSON `table` field and `--summary`'s worst-tables breakdown |
| `style.rs` | ANSI palette gated on TTY / `NO_COLOR` / `TERM=dumb` / `--no-color` |
| `glob.rs` | case-insensitive `*`/`?` matcher for `[scan].ignore` |

## Routing

| Task | Read | Notes |
|---|---|---|
| UX conventions (help, output, flags, errors) | [../../docs/cli-ux-guidelines.md](../../docs/cli-ux-guidelines.md) | Follow for every CLI change |
| Output formats (text/plain/JSON), exit codes, config | [docs/output.md](docs/output.md) | The full user-facing contract; keep in sync with `render.rs` |
| Validation baseline (Measure Killer) | tests/fixtures/adventure-works-baseline.txt | Header documents the format and the accepted deltas |
