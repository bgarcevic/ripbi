# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Power Query labels escape apostrophes in names** (`#63`) — `named_in_power_query`
  labels hand-wrapped names in single quotes without doubling internal ones, so a
  table `O'Brien` rendered as `'O'Brien' partition` instead of the DAX-escaped
  `'O''Brien' partition` that ObjectId's own `Display` produces. Partition and
  shared-expression labels now go through `NameKey::quoted()`, the same escaping as
  every finding id.

## [0.2.0] - 2026-09-11

### Added

- **Model-centric scans** (`#32`) — `ripbi scan --model <path>` analyzes one named
  semantic model against every PBIR report bound to it. Plain `--report` folders become
  search folders walked recursively for report items; pairing is by `definition.pbir`
  `byPath`, then the PBIP stem convention, then `byConnection` `initial catalog` (name
  only, since service `semanticmodelid`s and local `logicalId`s are disjoint GUID
  namespaces). By-name matches are flagged with a `Note:` line; reports bound to other
  models are listed (`Ignored … bound to other models`) and never fail `--strict`, while
  dangling or malformed references join the skip notices that `--strict` and `--json`
  already carry. Zero connected reports refuse with per-category diagnostics instead of
  scanning model-only.
- **Worst-tables breakdown in `--summary`** (`#38`) — the summary mode now ends the
  per-type counts with a `Worst tables:` block: the (at most 10) tables carrying the
  most surviving findings, count descending then table name (case-insensitively, the
  model's identity order), cut from the same post-`[scan].ignore` findings as the
  counts. A dead relationship counts under its "from" table; report measures, shared
  expressions, and functions belong to no table and stay out of the block, and a
  footer announces how many further tables have findings. `--json` gains a `table`
  field on every finding — the quoted model table, `null` when it has none — so a
  consumer can group the same way without parsing ids.
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
- **Power Query (M) reference extraction** (`#39`) — table partitions and shared
  expressions are tokenized by a hand-written lexer (mirroring the DAX lexer) and
  their references bind against the model. A table or shared expression named in M
  (a merge source, a referenced parameter query) stays live, because deleting it
  breaks refresh; a column named in M is the column's supply chain, not a consumer,
  so it rides along on the finding as `named_in_power_query` instead of an edge.
  `named_by_m` attaches to Data columns only — an M step names a column it produces —
  so auto date/time columns matching Desktop's date-template query no longer carry
  the supply-chain note. Inactive relationships are now live only through a
  `USERELATIONSHIP` call site; an unactivated inactive relationship is a finding
  itself, with its key columns.
- **Auto date/time identity and in-scan verdict** (`#16`) — tables now carry the
  engine identity flags (`is_private`, `is_local_date_table`, `is_template_date_table`)
  from TOM `isPrivate`, the `__PBI_*DateTable` annotations, and the
  `LocalDateTable_`/`DateTableTemplate_` name prefixes. `scan` renders the three-state
  verdict (in use / unused by reports / dead) in its own section; unused-by-reports
  and dead gate the exit code, while in use stays informational. Date variations are
  modeled on `Column`, so a report's date-hierarchy binding written over a varied
  column resolves through the model onto the related `LocalDateTable_*` and used
  machinery is no longer false-flagged.
- **Short `rib` alias** (`#56`) — `rib` is the same tool as `ripbi` under a shorter
  name: both binaries share one entry point, clap derives the displayed name from
  `argv[0]`, the installers hard-link the alias next to the binary, and the release
  archives ship both names.
- **`ripbi update` and a daily update notice** (`#44`) — the new `update`
  subcommand resolves the latest GitHub release, verifies the archive's sha256
  against `sha256sums.txt`, and atomically replaces `ripbi` and `rib` for
  script-installed binaries. `--check` reports latest vs. current and exits 1
  when newer; errors exit 2. Cargo-managed installs and source builds print
  the matching update command instead of self-replacing. Every foreground
  command also checks (at most once a day, via a detached hidden child) and
  prints one dim stderr line when a newer release exists; the check sends no
  data and is disabled by `RIPBI_NO_UPDATE_CHECK`, non-TTY stderr, `CI`, or
  `-q`.
- **User guide on GitHub Pages** — an mdBook at
  [bgarcevic.github.io/ripbi](https://bgarcevic.github.io/ripbi/), assembled
  by a Pages workflow from the docs that live next to the code, with a link
  checker so a broken internal link fails CI.
- **CONTRIBUTING.md** (`#41`) — dev setup, the CI gates, and where things live, so
  first contributions land against the same definition of done.

### Fixed

- **Bookmark saved filters on deleted pages no longer bind** (`#48`) — Power BI
  leaves deleted pages' sections inside bookmarks forever, and a saved filter on a
  page nobody can navigate to kept its columns alive with no way to re-apply it. A
  bookmark section now binds only when its page exists (the case-folded union of
  `pages.json` `pageOrder` and the `pages/` folders; when the two disagree, that is
  itself a notice). A stale section is skipped whole with one `StaleState` notice
  naming the bookmark and section. On the Artificial Intelligence Sample this closes
  the bookmark-kept-alive delta against the external baseline: `unused_total`
  155 → 171.

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

[Unreleased]: https://github.com/bgarcevic/ripbi/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/bgarcevic/ripbi/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/bgarcevic/ripbi/releases/tag/v0.1.0
