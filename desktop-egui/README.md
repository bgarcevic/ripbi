# ripbi desktop (egui, experimental)

An exploration of a pure-Rust desktop UI for ripbi using [egui](https://github.com/emilk/egui)
(`eframe`). No Node.js, no WebView: one native binary.

## Run

```sh
cargo run --manifest-path desktop-egui/Cargo.toml
```

1. Pick a `.SemanticModel` / TMDL folder or a `.pbip` project (or drop it on the window).
2. Add `.pbip` projects, `.Report` folders, or search folders (or drop them).
3. Analyze, then browse unused objects, broken references, date tables, and coverage.
   Export JSON writes `{ result, diagnostics }`.

Or switch to **Tenant**: paste a link to a model or report (or name the
workspace and model, or leave both blank to pick from lists), choose a folder, and
Fetch & analyze. This is `ripbi fetch --scan`: it signs in (Azure CLI, a token, or a
service principal; see the CLI's `--auth`), shows the downstream forecast, asks before
downloading an incomplete report set, writes the project, and analyzes it.

The header's theme icon cycles through system, light and dark.
The choice, like the window size, is remembered across runs.

## Design

`src/analysis.rs` calls `ripbi_app::scan::run` on a worker thread: the same
pipeline `ripbi scan` runs, with a typed `ScanRequest` in and a typed
`ScanOutput` out — no CLI crate, no JSON round-trip. Its notes are collected
for the coverage tab, and Export JSON writes `ripbi_app::json` (schema v1,
exactly what `ripbi scan --json` prints) under `result`.
`src/fetching.rs` runs `ripbi_app::fetch::run` on a worker thread; the fetch's
questions (workspace and model pickers, the incomplete-set confirmation) arrive
as events and block the worker until the UI answers from a modal.
`src/main.rs` is the immediate-mode UI. The package is
standalone (its own `[workspace]`) so GUI dependencies stay out of CLI builds
and CI.

```sh
cargo clippy --manifest-path desktop-egui/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path desktop-egui/Cargo.toml
```
