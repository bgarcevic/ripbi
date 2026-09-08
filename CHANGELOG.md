# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.1.0]: https://github.com/bgarcevic/ripbi/releases/tag/v0.1.0
