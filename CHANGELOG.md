# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **No stale "update available" notice after a successful `ripbi update`** —
  the ambient daily notifier ran after `update` too, comparing the cached
  latest release against the *running* process's compile-time version, which a
  self-update cannot change: the command's last words were "Updated ripbi
  0.2.2 → 0.3.0" followed by "ripbi 0.3.0 is available (you have 0.2.2) — run
  'ripbi update'". The notice never follows `update` now (`--check` already
  prints both versions); it resumes with the next command, launched from the
  new binary.

## [0.3.0] - 2026-09-17

### Added

- **Calculated tables vouch for the names their expression makes visible** —
  a calculated table's columns exist only in its partition expression's
  output (a `DATATABLE`'s headers, an `ADDCOLUMNS`'s string-named columns,
  the columns of a wrapped `FILTER`/`VALUES` table), so a visual or measure
  reading one of them resolved to nothing and flagged as broken. A qualified
  miss on a calculated table now resolves when the name is lexically visible
  in that expression, and still flags when it is visible nowhere — so
  renaming a `DATATABLE` header out from under its bindings is real breakage
  again, the case a blanket "calculated ⇒ resolved" rule would have gone
  silent on. On a second production workspace the broken-visual findings
  fell from 24 to the 7 genuinely stale ones.
- **The artifact-breakage pass resolves query-time references** — three real
  models turned every rule into a false `broken` claim, so the pass now
  resolves them: a report measure referencing a sibling report measure (the
  graph's ordinary report-measure edge — the pass simply wasn't mirroring
  it), `@`-prefixed extension columns (the SQLBI `ADDCOLUMNS(…, "@Krav", …)`
  pattern read back as `[@Krav]`), and the column names query time
  introduces in the same expression — string-literal extension names
  (`SELECTCOLUMNS`/`GROUPBY`/`ROW`) and the table constructor's fixed
  `Value`/`Value1…N` defaults (new `dax::quoted_names` beside
  `dax::references`). Verification needs DAX scope analysis the lexer
  deliberately does not do, so each rule is under-claim; on a 26-report
  production workspace the broken-visual findings went from 30 to the one
  genuinely stale filter.
- **The pairing announce reads one line per fact in every mode; `--verbose`
  expands it** — the by-name pairing notes and the ignored-reports line of
  model-centric scans (and the report-name list in the scanning line) were
  the last uncollapsed walls on a workspace whose reports all bind
  `byConnection`: 26 thin reports printed 26 near-identical `Note:` lines
  before the actual findings. Issue #65's collapse — one line per
  `initial catalog`, count, up to three names, `… and N more` — is now every
  mode's shape, the ignored list is capped the same way, and a new
  `-v/--verbose` flag restores the full per-report audit trail (every name
  in the scanning line, one note per report, the complete ignored list).
  The pairing *facts* never disappear: a wrong pairing means wrong
  findings, so the counts and the weak-pairing warning always show.
- **Standard Fabric export properties parse without drift notices** —
  `database.tmdl`'s `id`, `compatibilityMode`, and `language`,
  `report.json`'s `publicCustomVisuals`, and `page.json`'s
  `pageBinding.referenceScope` join the known-key tables as ignored
  metadata: real exports carry them on every item and they name no model
  object. The trigger was `scan`'s clean-ingest bar for breakage findings
  (`#60`): on models whose only skips were these, every broken-visual
  finding was suppressed as if the parser had drifted past the objects the
  bindings name.
- **Broken visual bindings are findings; `--broken` gates them** (`#60`) —
  `scan` no longer stays silent when a report is broken. Every report
  binding's resolution is now classified as it becomes a root, and the misses
  surface as `broken_visual` findings naming the written field, the page,
  the visual, and the reason: `table_not_found`, `field_not_found`,
  `measure_not_found`, `hierarchy_not_found`, or `level_not_found` — the
  static form of the error state the service would render. A binding that
  resolves onto an artifact whose own DAX binds a reference to nothing
  inherits the breakage with the artifact named
  (`bound_artifact_broken`) while staying a root; the artifact's own
  bound-or-unbound finding kind remains issue #84's scope. The precision bar
  mirrors the unused findings' conservatism rule inverted — a false "broken"
  is itself a breakage claim, so KPI-suffixed variants (`… Goal`,
  `… Status`, `… Trend`, `… Value`) resolve when the base measure exists,
  field parameters and auto date/time hierarchies resolve through their
  machinery (#52, #47), variation-flavored hierarchies the machinery cannot
  resolve stay silent, stale qualifiers resolve to their model-global
  measures, `Written` references never flag, and — the clean-ingest gate —
  unknown-*object* skips from the model ingest suppress the findings entirely
  with the count explained on the summary line (`--strict` surfaces the
  skips behind it); property-level drift cannot hide a name — the object was
  parsed with it — so it never suppresses. Liveness is untouched: the
  qualifying-table fallback still roots what
  it always rooted, and the unused set is byte-identical. Breakage is
  advisory by default — reported in every output mode (a dedicated human
  section, `broken_visual:<reason>` records in `--plain`, a top-level
  `broken` array plus `summary.broken`/`broken_total` in `--json`) but never
  changing the exit code — and `--broken` joins the type-flag family to
  scope the run to breakage and gate on it, so an unused-only gate never
  fails on a broken visual and vice versa (#84's constraint). The
  broken-visual PBIP fixture (a healthy card, a card on a dropped column, a
  card on a broken measure, a KPI-style card that must resolve) pins the
  contract end to end.
- **Field-parameter role bindings parse and bind; fixture locks for field
  parameters and auto date/time** (`#52`) — a visual's `query.queryFieldParametersByRole`
  — the role-keyed map some exports hang off the query instead of the per-role
  `fieldParameters` arrays — is now a known shape: each role's entries' `expr`
  fields join that role's well as inactive projections, which bind like any
  other field, so a parameter table bound only through that key keeps its
  columns (and, via sort-by/group-by, the hidden auxiliary columns) alive
  instead of surfacing as drift plus false positives. Two PBIP fixtures lock
  the no-false-positive guarantees: a field-parameters project whose toggle
  tables are bound through both mechanisms (with the parameter expression's
  source columns kept alive by the calculated partition's `NAMEOF()` calls),
  and an auto date/time project where a date-hierarchy visual over a varied
  date column keeps the `LocalDateTable_*` machinery at an `in_use` verdict
  with none of the generated columns flagged — while a plain date-column
  binding shows the machinery reporting through its `dead` verdict instead.
  Validation showed the per-role `fieldParameters` path already bound (the
  fixtures lock it); only `queryFieldParametersByRole` was missed.
- **Dynamic M parameter bindings keep the bound column live** (`#50`) — a model's
  dynamic M query parameters now record which column they are bound from: the
  parameter expression's `parameterValuesColumn` property (the authoritative half of
  the binding; the column side is an anonymous `ParameterMetadata` marker). A consumed
  parameter keeps its bound column alive with the `dynamic M parameter binding`
  provenance, so a slicer-fed parameter's column no longer surfaces as unused just
  because no DAX or report field names it. The chain is report → consuming partition →
  parameter (Power Query references) → bound column; an unconsumed parameter stays
  dead and takes its bound column with it, and a binding naming a column the model no
  longer has keeps nothing alive. Field parameters (`"kind": 2`) and what-if
  parameters (`"version": 0`) remain unmodeled — their columns were never at risk.
  DirectQuery-column `sourceProviderType` and the model's `valueFilterBehavior` are
  now recognized as Tier-1 metadata instead of drift notices.
- **Incremental refresh change-detection expressions confer liveness** (`#53`) — a
  table's `refreshPolicy` was the one partition child the TMDL parser did not read,
  yet its expressions run at refresh time: `pollingExpression` (change detection) and
  `sourceExpression` (the `RangeStart`/`RangeEnd`-filtered source) name model objects —
  a measure-based "detect data changes" pick, the shared query a custom polling
  expression reads, the `RangeStart`/`RangeEnd` parameters named only there in the
  Desktop Full-DataView shape — and deleting any of them breaks refresh, the one
  direction the conservatism policy refuses to get wrong. The policy is now parsed and
  its two expression properties enumerated like any other expression:
  `pollingExpression` through both the DAX pipeline (provenance `change detection
  expression`) and the existing M bindings, `sourceExpression` through the M bindings
  alone. The policy's scalar vocabulary (periods, granularities) names no model object
  and stays silent; an unrecognized key inside the policy is an ordinary
  `UnknownProperty` notice.
- **Phone-layout bindings keep fields live** (`#49`) — a report's phone layout
  (`Report/definition.mobile/`) is now ingested beside the desktop tree: its pages
  parse with the same walker, their visuals bind the same model, and the bindings
  become reachability roots — so a field referenced only there, still rendered for
  phone users, no longer surfaces as an unused finding. Only the page/visual tree is
  read (no report anchor, report measures, or bookmarks); a missing or anchor-less
  layout is the common case and is silent. The bindings' provenance reads
  `mobile layout …` so an audit of a survivor names the surface that kept it alive,
  and the summary's root count includes them.

### Fixed

- **Injected streams decide the output palette** — `scan::run_in` probed the
  process's real stdout/stderr for terminal detection instead of the
  `Streams` it was given, so a test harness whose real stdout was a terminal
  got ANSI escapes inside otherwise plain output (and any embedder could hit
  the same). `Streams` now carries `stdout_is_tty` beside `stderr_is_tty`,
  both palettes derive from the injected streams, and the binary passes the
  real terminals' state. Test runs are deterministic regardless of the
  terminal `cargo test` runs in.

### Changed

- **A plain `--report` folder pairs with a PATH that names a semantic model** (`#67`) —
  `ripbi scan models/X.SemanticModel --report references/` now walks the folder as a
  search root, with the same pairing tiers, notes, exclusions, and `--strict` behavior
  as `--model` mode, instead of rejecting it with the mode-switch hint. The `ripbi.toml`
  `target` counts as the same explicit model. Other targets keep the error: a `.pbip`,
  project folder, `.Report`, or a discovered project still accepts report items only,
  and anchor-less `.Report` folders stay malformed.
- **The auto date/time section is the machinery's only surface** (`#47`) — the
  engine-generated tables' unused members (the GUID-named columns, hierarchies, and
  partitions under `LocalDateTable_*`/`DateTableTemplate_*`) no longer appear as
  generic findings indistinguishable from genuine orphans. The section's one verdict
  per table — with the date column it serves and the dead chain — is the deliberate,
  actionable report, because the members are not separately actionable: removing the
  table removes them, and disabling auto date/time on the named column is the fix.
  The summary line accounts for the covered members (`(41 unused auto date/time
  members covered by their tables' verdicts)` on the Artificial Intelligence sample),
  and `--json` counts them in `summary.auto_date_time.member_findings`.
- **The `--summary` auto date/time line aggregates the machinery** (`#47`) — it now
  reads `Auto date/time: 6 hidden tables over 5 date columns (1 in use, 5 dead)`,
  counting the distinct date columns the local tables serve; the shared
  `DateTableTemplate_*` serves none, so the columns can be fewer than the tables, and
  a model whose machinery pairs with no column drops the clause. `--json` gains
  `summary.auto_date_time.hidden_tables` and `summary.auto_date_time.date_columns`
  alongside the verdict counts.
- **A documented opt-out for legacy auto date/time models** (`#47`) — `docs/output.md`
  now gives the `[scan].ignore` recipe (`ignore = ["LocalDateTable_*",
  "DateTableTemplate_*"]`) that silences the whole section, with the exit-code
  treatment (suppressed tables count as handled) and the trade-off: the recipe also
  hides the `in use` tables' replace-with-a-real-date-table advice.

## [0.2.2] - 2026-09-13

### Fixed

- **`ripbi update` no longer fails with access denied on Windows** — the install
  scripts create the `rib` alias as a hard link to `ripbi`, so once the running
  binary had been replaced, the alias was still a link to the mapped old image and
  the plain rename into `rib.exe` failed with `os error 5`, leaving `ripbi` updated
  while `rib` stayed behind (and the next check reported "up to date"). Locked
  targets now get the same move-aside-to-`<name>.old` treatment as the running
  binary, the running binary is replaced last so a failure cannot leave a
  half-installed pair, and stale `.old` backups are swept on the next update.

## [0.2.1] - 2026-09-13

### Changed

- **By-name pairing notes collapse in count-oriented modes** (`#65`) — in `--summary`,
  `--plain`, and `--json`, the `Note:` lines for reports matched only by a `byConnection`
  `initial catalog` now print once per catalog with the count and up to three names
  (`… and N more`), instead of one line per report. The default human mode keeps the
  per-report list, and `-q` still suppresses everything. The notes stay informational:
  never `--strict`-fatal and never in the JSON `skips` array.

### Fixed

- **`--model` search walks surface malformed `.Report` folders** (`#66`) — a `*.Report`
  folder found under a search folder without a `report.json` / `definition/report.json`
  anchor was pruned silently, so its bindings could not keep objects alive and they
  surfaced as false "unused" findings. The walk now records it as a
  `malformed_report_item` skip notice, which stderr, `--json`, and `--strict` all see —
  matching the explicit `--report` error path.
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

[Unreleased]: https://github.com/bgarcevic/ripbi/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/bgarcevic/ripbi/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/bgarcevic/ripbi/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/bgarcevic/ripbi/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/bgarcevic/ripbi/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/bgarcevic/ripbi/releases/tag/v0.1.0
