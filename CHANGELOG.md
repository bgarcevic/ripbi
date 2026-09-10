# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Per-type scan filters** (`#31`) — `ripbi scan --measures`, `--columns`,
  `--tables`, `--hierarchies`, `--partitions`, `--relationships`, `--calc-items`,
  `--expressions`, `--functions`, and `--report-measures` report only unused objects
  of the passed types, in every output mode and in the exit code; the auto date/time
  section prints only when `--tables` is among them. `--json` gains
  `summary.unused_total`, the model-wide unused count, so consumers can tell a
  filtered-away finding from an absent one.
- **`--power-query` flag** (`#57`) — the "⭘ Power Query also names it" annotation on
  unused Data columns is now hidden by default and shown on request. `--json` always
  carries the underlying `named_in_power_query` field.
- **User guide on GitHub Pages** — an mdBook at
  [bgarcevic.github.io/ripbi](https://bgarcevic.github.io/ripbi/), assembled
  by a Pages workflow from the docs that live next to the code, with a link
  checker so a broken internal link fails CI.

## [0.1.0] - 2026-09-07

First release: the full static-analysis pipeline plus the `ripbi scan` command
that exposes it.

### Added

- **TMDL semantic-model ingestion** — `.SemanticModel` items (folder,
  `definition/`, or a `model.tmdl` directory) normalize into a unified
  `TabularDatabase`: tables, columns, measures, hierarchies, partitions,
  calculation groups, calendars, relationships, and RLS roles.
- **PBIR report ingestion** — `.Report` items normalize into a `ReportModel`:
  pages, visuals, and every field binding (values, filters, tooltips,
  conditional formatting) with report/page/visual provenance. A report finds
  its model through `datasetReference.byPath`.
- **Resilient schemas** — unknown or drifted properties and objects produce
  skip notices, never failures; `--strict` turns them into errors.
- **DAX reference resolution** — a zero-copy lexer extracts table, column,
  measure, and function references from expression strings, including
  `USERELATIONSHIP` and calculation-item logic.
- **Dependency graph and reachability** — a `petgraph` DAG with two-pass BFS
  (strong reachability for exact dead-object findings, weak reachability for
  relationship and key-column liveness). Report bindings and RLS roles seed
  the traversal; findings carry their dead-chain annotations
  (`← only used by 'X' (also unused)`).
- **`ripbi scan [PATH]`** — discovery of `.pbip` projects, `.SemanticModel`/
  `.Report` stem pairing, and connected-report resolution; human, `--plain`,
  and `--json` output; exit code 0 (clean) / 1 (unused found) / 2 (error);
  `-q/--quiet`, `--strict`, `--report <PATH>` (repeatable), `--no-color`,
  `--no-input`, and an interactive picker when several projects share a
  directory.
- **`ripbi.toml` project config** — `target`, `reports`, and `[scan].ignore`
  object-name globs (case-insensitive `*`/`?`); flags override the config.
- **Output contract** — the JSON schema, exit codes, and config reference are
  documented in `crates/ripbi-cli/docs/output.md`.
- **Validated findings** — an end-to-end test pins the Adventure Works sample
  scan against a committed 44-object baseline from an external unused-objects
  analysis: full agreement, zero false positives.
- **One-line installers** — `install.sh` (macOS/Linux) and `install.ps1`
  (Windows) resolve the latest GitHub release, verify the archive's sha256
  against `sha256sums.txt`, and install into `~/.local/bin`; both can be piped
  straight from the repository or downloaded, reviewed, and run from a file,
  with `RIPBI_VERSION` pinning a release.
- **crates.io publishing** — the release workflow publishes `ripbi` and
  `ripbi-core` on every tag (gated on the `CARGO_REGISTRY_TOKEN` secret), so
  `cargo install ripbi` works from 0.1.0 on; the binary crate is now named
  `ripbi` (library target unchanged).
- **README** — install instructions, a 30-second quickstart with real
  AdventureWorks output, the exit-code table, and CI/release/crates.io badges.

[Unreleased]: https://github.com/bgarcevic/ripbi/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/bgarcevic/ripbi/releases/tag/v0.1.0
