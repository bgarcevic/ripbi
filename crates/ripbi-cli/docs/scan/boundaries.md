# Validation and known boundaries

## Validation

`scan`'s findings on three committed samples are pinned against external unused-objects
analyses —
[`tests/fixtures/adventure-works-baseline.txt`](../../tests/fixtures/adventure-works-baseline.txt),
[`tests/fixtures/regional-sales-baseline.txt`](../../tests/fixtures/regional-sales-baseline.txt),
and
[`tests/fixtures/artificial-intelligence-baseline.txt`](../../tests/fixtures/artificial-intelligence-baseline.txt):
every object a baseline marks dead is a finding with the same chain shape, and no live
object is ever flagged. The accepted deltas are documented in each fixture header: the
dead `Time Intelligence` field-parameter cluster on Adventure Works; fully-dead
relationship-only tables the exports don't list as rows (a table referenced by nothing
but a relationship is unused by the documented containment rule); and, on the
Artificial Intelligence sample, the auto date/time machinery of the five date columns
whose hierarchies no visual binds — reported only through the `auto_date_time` section
(its members are covered by the per-table verdicts, and the baseline's machinery rows
are asserted *absent* from the generic findings). The one bound table — the report's
date hierarchy over `'Opportunity Calendar'[Date]`, resolved through the model's
variation declaration — is fully live, its columns are gone from the findings, and its
`in use` verdict (with the other five tables' verdicts) is pinned by the same test
through the `auto_date_time` section. One conservatism policy
the external analyses do not share, visible in that baseline: bookmark saved filters
count as bindings (re-applying a bookmark re-binds its fields) — but only for
sections whose page still exists. Power BI leaves deleted pages' sections inside
bookmarks forever, so those sections do not bind (a `stale_state` notice,
surfaced by `--strict`), and a bookmark with only such sections and no
report-level filters is an ordinary `bookmark` finding. Its saved fields carry
`stale bookmark` dead-chain edges: the columns their filters were the last
consumers of surface as ordinary findings; on the Artificial Intelligence
sample that closes the last bookmark-kept-alive delta with the export, at the
cost of a documented cascade (the fully-dead `'Cases'` and `'Case Calendar'`
tables and everything chained under them). Inactive relationships
are the opposite correction: they are live only when a live `USERELATIONSHIP` reference
activates them, so an unactivated one is a finding itself, with its key columns chained
under it — `only used by relationship … (also unused)`.

The phone layout is the same conservatism on the report side (issue #49): a report can
ship a `definition.mobile/` tree beside `definition/`, and its visuals render on
phones, so its bindings enumerate as roots exactly like the desktop tree's — a field
referenced only there never surfaces as a finding, because deleting it would break the
report for phone users. Its provenance reads `mobile layout …`, and the summary's root
count includes it. Page visibility stays display-only in the phone layout, as on
desktop; report-level state (report measures, bookmarks) remains desktop-only.

A fourth, uncommitted validation ran against a large production model (≈3.8k graph
objects, 14 reports, ≈2.5k columns/measures measured externally): 99.3% of the
externally-dead objects were findings with identical chain shape, and — the direction
that matters — of the objects `scan` flags that the external analysis calls live,
**none** had a live consumer. Every one was a member of a chain where every consumer
was itself unused: auto date/time clusters no report binds, and active relationships
between otherwise-dead tables. The one other historical divergence — columns referenced
only inside Power Query — closed in both directions with the M lexing of issue #39:
the pipeline is M → tables/columns → DAX → reports, so an M mention of a column is its
supply chain, not a consumer, and those columns surface as findings again, each
carrying `named_in_power_query` so the script-side steps can be cleaned up alongside.
What M *does* keep alive is what deletion would break: shared expressions and
merge-source tables named by other queries' M. The incremental refresh policy
extends the same rule to refresh time (issue #53): its change-detection
expression and source expression are evaluated by the engine at every policy
refresh, so the measures and shared expressions they name — a measure-based
"detect data changes" polling expression, or the RangeStart/RangeEnd
parameters named only by the policy's source in the Desktop "Full DataView"
shape — stay live while the policy's partition is, with `change detection
expression` as the provenance; the policy's scalar vocabulary (periods,
granularities) names nothing and stays silent. Two external-analysis blind spots
surfaced the same run: a measure bound only by a
drillthrough filter on a hidden page (counted live here; the external tool skipped
hidden pages), and report-level measures the external tool judges by view telemetry,
which static analysis deliberately ignores.

## Known boundaries

- `--type` covers the model object kinds, including `role`; `--broken` selects
  broken visual bindings separately (issue #60).
  A broken binding's kind (`broken_visual`) also never appears in the `unused` array
  or the generic groups — it has no `ObjectId` of its own.
- TMDL and TMSL (`model.bim`) semantic models, PBIR folders, PBIT templates,
  PBIX files, and `.abf` backups are ingested. A PBIT or PBIX provides its own
  model and report (a PBIX's model is decoded from its compressed `DataModel`,
  metadata only). A thin PBIX report with no embedded model needs a separate
  model (`--model` or one unambiguous sibling: `.SemanticModel`, `model.bim`,
  `.abf`, or a model-bearing PBIT/PBIX). An `.abf` is model-only, like
  `model.bim`.
- Analysis covers only the ingested reports. External consumers — thin reports, Excel
  (Analyze in Excel), XMLA reads, other datasets' DAX — are invisible; a scan with
  findings says so in a hint under its verdict (`[scan].external_consumers = "none"`
  in [`ripbi.toml`](../config.md) drops the hint for a model nothing else reads). Excel
  workbooks you have can be scanned directly: `--report book.xlsx` reads their
  PivotTables, slicers, CUBE formulas, and DAX query tables (issue #172); workbooks
  you don't pass stay invisible. Mark what they read with a `ripbi_keep` annotation
  ([Keeping objects on purpose](keep.md)), or add what they
  actually queried from a workspace-monitoring log with `--queries-from`
  ([Logged queries](queries.md)).
- A model-only scan is refused: with no report bindings (and no RLS roles) everything
  is formally unused, which is never the answer the user wants. Pass `--report`. When
  search folders are walked, the same refusal lists how many report items were bound to
  other models and how many had unresolved dataset references.
- Folder-walking outside `--model` needs a PATH (or `ripbi.toml` `target`) that names a
  semantic model itself. Any other target — a `.pbip`, project folder, `.Report`, or a
  cwd-discovered project — rejects a plain `--report` folder with a hint naming the
  mode switch (`--report` accepts report items only).
- `byConnection` pairs by dataset *name* only (case-insensitive `initial catalog` against
  the model's `.platform` display name or item stem). Service `semanticmodelid` GUIDs and
  local `.platform` `logicalId` GUIDs are disjoint namespaces, so no static GUID match
  exists; report items that name a different dataset are excluded, and ones whose shape
  cannot be matched are unresolved notices.
- A multi-model search folder includes only the reports bound to the `--model` target;
  reports bound to other models are listed and skipped.
