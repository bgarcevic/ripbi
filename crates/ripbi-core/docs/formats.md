# Source formats — detection, grammar, and drift policy

What each ingestion format's parser accepts, how it maps into the ASTs, and —
most importantly — the policy for everything it does *not* model. Read this
before changing anything under `src/ingest/`.

Two formats are covered today: TMDL semantic models (`ingest::semantic_model`
→ `TabularDatabase`) and PBIR reports (`ingest::report` → `ReportModel`, its
own section below).

## Detection

`ingest::semantic_model(path)` accepts:

- a `.SemanticModel` item folder (its `definition/` subfolder is located
  automatically), or
- a `definition/` folder itself (any directory that directly contains a
  `model.tmdl`, or is named `definition`).

Anything else is `Error::UnsupportedFormat`. The item's display name is read
from `.platform` (`metadata.displayName`) beside `definition/` — TMDL itself
records no usable model name (`model.tmdl` names its root object `Model`). A
missing or unreadable `.platform` yields `None`; a name is provenance, never
liveness.

## TMDL grammar subset

Stage 1 (`src/ingest/tmdl.rs`) scans tab-indented lines into a generic node
tree; stage 2 maps known descriptors into `TabularDatabase`. Tab indentation
only — a leading-tab count is the depth; spaces after the tabs are content.

Object headers, mapped into the AST:

| Descriptor | Maps to |
|---|---|
| `table N` (+ `isHidden`) | `Table` (`defaultDetailRowsDefinition` → detail rows) |
| `column N` / `column N = dax` | `Column` (data / `ColumnKind::Calculated`; `sortByColumn`, nested `relatedColumnDetails` → group-by columns) |
| `measure N = dax` | `Measure` (+ `formatStringDefinition`, `detailRowsDefinition`, `kpi`) |
| `partition N = m\|calculated\|query\|<other>` | `PartitionSource::M`/`Calculated`/`Query`/`Other` |
| `relationship <guid>` | `Relationship` (`isActive` defaults true, like TOM) |
| `hierarchy N` + `level N`/`column:` | `Hierarchy` / `HierarchyLevel` |
| `role N` + `tablePermission T = filter` | `Role` / `TablePermission` |
| `calculationGroup` + `calculationItem N = dax` | `CalculationGroup` / `CalculationItem` |
| `expression N = m` | `SharedExpression` |
| `function N = dax` | `Function` |
| `model` / `database` | ordering/metadata only |

Property forms: `key: value` (scalar), `key = value` (raw), bare `key` (flag,
e.g. `isHidden`), `annotation N = v`, `ref table N` (fixes table order;
unreferenced table files append in file-name order). `cultures/` is never
read. Names are single-quoted when they contain spaces or punctuation
(`'Sales Order'`), with `''` as an escaped quote; unquoting happens in the
format layer. Column references are `Table.Column`, `'Quoted Table'.Column`,
`'Quoted Table'.'Quoted Column'`, or a bare (possibly quoted) `Column` for
same-table references such as `sortByColumn`.

**Multi-line expressions.** A `key =` line with nothing after it captures the
following deeper-indented block verbatim (blank lines inside preserved,
dedented by the block's own first-line indent). The block closes at the first
non-blank line above that indent — which is how a sibling property at
depth+1, such as a measure's `formatString`, closes the expression. When the
expression's first line sits on the header itself (`measure M = VAR …`
followed by lines *below* the property level, i.e. deeper than depth+1), the
value continues as `first line + "\n" + block` — the shape PBI Desktop
serializes for multi-line DAX (see `samples/…/Owners.tmdl`).

Only two things are `Error::Tmdl`, never notices: a line that tokenizes to
nothing (e.g. starting with `:` or `=`), and a flag-shaped node whose key is a
known descriptor that requires a name (a bare `table`). Everything else
unexpected is a skip notice; a run never fails because of drift.

## PBIR reports

PBIR is not one grammar but a folder of one-object-per-file JSON documents,
each carrying a `$schema` URL whose version drifts across Power BI releases
(preview format; real exports run ahead of the published schemas at
`microsoft/json-schemas`). There is therefore no fixed grammar: parsing is
per-file *key policies* — keys the AST models are parsed, keys deliberately
unmodeled are silent, anything else is `UnknownProperty` drift (see the
`Keys` tables in `src/ingest/pbir.rs`). Files are read in a fixed order
(`report.json` → `definition.pbir` → `reportExtensions.json` → pages in
folder-name order → bookmarks in file-name order) so notices are
deterministic.

**Detection.** `ingest::report(path)` accepts a `.Report` item folder (its
`definition/` is located automatically) or a `definition/` folder itself (any
directory directly containing a `report.json`). A report is parsed
*standalone* — the semantic model it references need not sit beside it,
because several reports can share one model. The reference is read from
`definition.pbir` beside `definition/` (`byPath.path` or
`byConnection.connectionString`; the schema demands exactly one); any absence
or drift yields `DatasetReference::Unresolved` — a pairing is never
fabricated. Display name from `.platform`, as on the model side.

**What each file contributes.**

| File | Maps to | Deliberately ignored |
|---|---|---|
| `report.json` (the anchor) | `filterConfig` → report filters | `themeCollection`, `settings`, `resourcePackages`, `slowDataSourceSettings`, `objects` (canvas formatting) |
| `definition.pbir` | `DatasetReference` | `version` |
| `reportExtensions.json` | `entities[].measures[]` → report measures (`name`, `expression`, `formatString`) | `dataType`, `hidden`, `dataCategory`, `displayFolder`, `measureTemplate`, `references`, … |
| `pages/pages.json` | *never read* — page order and the active page are display state | — |
| `pages/<dir>/page.json` | `name`, `displayName`, `visibility` (`HiddenInViewMode` → `is_hidden`), `filterConfig`, `pageBinding` (`type`, `parameters[].fieldExpr` → drillthrough) | `displayOption`, `height`, `width`, `objects`, `type`, `visualInteractions` |
| `pages/<dir>/visuals/<dir>/visual.json` | container `name`, `filterConfig`; `visual.visualType`, `query.queryState` (wells, plus `fieldParameters` as inactive projections), `query.sortDefinition` (sorts), `objects` (see below), `visualContainerObjects.visualTooltip`/`visualHeaderTooltip` `section` → tooltip page | `position`, `isHidden`, `parentGroupName`, `howCreated`, `visualGroup` (a group container carries no query and is skipped whole), `syncGroup`, `expansionStates`, `drillFilterOtherVisuals` |
| `pages/<dir>/visuals/<dir>/mobile.json` | *never read* — mobile layout binds nothing of its own | — |
| `bookmarks/*.bookmark.json` | `explorationState.filters` → bookmark report-level filters; `sections.<page>.filters` → section filters; `sections.<page>.visualContainers.<id>.filters` → per-visual saved filters; `singleVisual.projections`/`activeProjections` → saved wells | `options`, `explorationState.objects`/`version`/`activeSection`, `visualContainerGroups`, `singleVisual.display`/`orderBy`/`expansionStates`/… |
| `version.json` | *never read* | — |

**Persisted automatic filters.** A visual's own filter normally lives in the
container's `filterConfig`, but an *automatic* filter persists only after the
filter pane has been expanded in the report's authoring history — and then it
appears as a `filter` property inside the formatting `objects`
(`objects.general[].properties.filter`). Both shapes join the visual's
filters; the other `objects` properties are conditional formatting, whose
fields are collected structurally (a `FillRule` input, an icon rule's
comparison operands).

**Filters, aliases, and condition trees.** One filter-entry shape serves every
scope: the filtered field under `field` (bookmark states spell it
`expression`), and the condition under `filter` — a `FilterDefinition`
(`Version: 2`, `From`, `Where`). `From` maps query aliases to entities; a
`SourceRef.Source` resolves through it case-insensitively, a
`SourceRef.Entity` names the table directly, and a hierarchy on a date
variation sources its table through `PropertyVariationSource`. An alias that
matches no `From` entry yields `FieldTarget::Written` plus an
`UnresolvedAlias` notice — the alias is not a table name, so it is never
written in as one. Condition trees are walked *structurally*, not
schema-driven: known field containers (`Column`, `Measure`, `Aggregation`
with function codes 0–8, `Min`/`Max`/`Percentile`, `Hierarchy`/`HierarchyLevel`)
are extracted wherever they nest, `Literal` values are data and never
references, and a `Where` clause's `Target` arrays are references too. Known
non-drift shapes that yield no reference: `ScopedEval` wrappers (unwrapped
transparently), `RangePercent` bounds in formatting rules (they reuse the
`Min`/`Max` keys for gradient ends), and visual-calculation sources (below).

**Errors.** Only the anchor `report.json` can fail the run (`Error::Io` /
`Error::Json`) — it is what makes the folder a report. An unreadable page,
visual, or bookmark file, a malformed filter field, an unknown property: all
notices, never failures.

### PBIR known gaps

- **Visual calculations.** A field sourced from `Subquery`, `SelectRef`, or
  `TransformTableRef` names a visual-calculation local, not a model object, so
  it produces no binding and no notice; a subquery's own `Select` columns are
  walked with the subquery's aliases, so the model fields behind a
  calculation do bind. A `SelectRef` name (a calculation output referenced
  elsewhere in the same visual) cannot be resolved here, and a
  `NativeVisualCalculation` projection in a field well is not yet modeled.
- **Bookmark-saved display state.** `singleVisual.orderBy` (saved sort),
  saved formatting merges (`singleVisual.objects`, `explorationState.objects`
  — a conditional-formatting rule changed only inside a bookmark would be
  missed), and `highlight.selection` are deliberately unmodeled.
- **Tooltip pages** are read from `section` expr literals; any other spelling
  yields a `MalformedValue` notice rather than a silent loss.

## Drift policy: two tiers of skipping

Ingestion entry points return `Ingested<T>` — the parsed value plus a
`Vec<SkipNotice>` (path, TMDL line number or JSON pointer, `SkipKind`,
detail). Notices are warnings as data: core never prints, and the CLI decides
presentation. They are collected on every run, not only in debug builds,
because a silent skip can surface later as a false "unused" finding.

**Tier 1 — deliberately unmodeled, silent.** Keys on the curated lists (the
TMDL ignore list below; the PBIR `Keys` tables in `src/ingest/pbir.rs`) are
skipped without a notice. Everything on them is metadata that cannot consume
a model object, so skipping it cannot cause a false "unused" finding. The
tests hold this honest in both directions: the samples tests fail if a list
misses something the samples carry; the golden fixtures fail if a modeled key
lands on a list.

**Tier 2 — unexpected drift, noticed.** An unknown object (root descriptor
this crate does not know, unknown file or directory under `definition/`), an
unknown property not on a list, a modeled value that fails to parse
(`MalformedValue`), or a PBIR query alias that cannot be resolved
(`UnresolvedAlias`).

### The ignore list

Universal metadata: `lineageTag`, `changedProperty`, `description`,
`annotation`, `extendedProperty`.

Columns: `dataType`, `formatString` (static), `summarizeBy`, `sourceColumn`,
`dataCategory`, `isKey`, `isNameInferred`, `isDataTypeInferred`, `isUnique`,
`isDefaultLabel`, `isDefaultImage`, `isAvailableInMdx`, `keepUniqueRows`,
`relatedColumnDetails`, `tableDetailPosition`.

Measures/tables: `displayFolder`, `isPrivate`.

Model/database: `culture`, `sourceQueryCulture`,
`defaultPowerBIDataSourceVersion`, `discourageImplicitMeasures`,
`dataAccessOptions`, `compatibilityLevel`, `createOrReplace`,
`retainDataTillForceCalculate`.

Cultures (folder never read; keys listed for stray uses): `cultureInfo`,
`linguisticMetadata`, `contentType`.

Partitions: `mode`. Roles: `modelPermission`. KPIs: `statusGraphic`.
Hierarchies/levels: `ordinal`. Relationships: `crossFilteringBehavior`,
`fromCardinality`, `toCardinality`, `joinOnDateBehavior`, `hideArrows`,
`securityFilteringBehavior`, `reliability`.

### Verified spellings (validated against the AS engine)

No `samples/` model exercises these, so the spellings were verified by loading
probe models in the Analysis Services TMDL engine (via
[tomix-cli](https://github.com/bgarcevic/tomix-cli)'s `tx load`), and the
golden fixture is held to the same standard — it loads clean in the engine:

- **KPI**: `kpi` blocks carry `statusGraphic` plus `targetExpression`,
  `statusExpression`, `trendExpression`. The short forms (`target =`,
  `status =`, `trend =`) are *not* valid TMDL — the engine rejects them — so
  this parser notices them as drift rather than mapping them.
- **Detail rows**: measures use `detailRowsDefinition`; tables use
  `defaultDetailRowsDefinition` (the engine rejects the measure spelling on a
  table).
- **Calculation-group selection expressions**: `noSelectionExpression` and
  `multipleOrEmptySelectionExpression` are *objects* — the expression itself,
  with an optional nested `formatStringDefinition` child for its dynamic
  format string. Standalone format-string spellings are rejected by the engine.
- **`tablePermission`**: the filter is the `=` expression (inline or block);
  a `filterExpression` child property also loads. A permission with no filter
  is just `tablePermission <table>`. All three shapes verified.
- **`relatedColumnDetails`**: a nameless object under a column with one
  `groupByColumn: <column>` per grouped column (the shape in
  `samples/…/Toggle for breakdown.tmdl`); it feeds `Column::group_by_columns`.
- The engine also *resolves* relationship `fromColumn`/`toColumn` against the
  model's tables, and rejects `///` doc comments (descriptions) on
  `tablePermission`. This parser does not cross-validate references — missing
  targets are the graph layer's findings, not parse failures — but the golden
  fixture keeps self-consistent references to stay engine-loadable.

### TMDL known gaps

- **Date variations** (`variation`, `defaultHierarchy`, `isDefault`,
  `showAsVariationsOnly`) are deliberately unmodeled. A variation references a
  hierarchy *by name*; a hierarchy kept alive only by a variation could be
  mis-reported as unused. Tracked for a future AST extension; until then the
  keys are on the ignore list so healthy models do not drown in notices.
- **`Calendar` and `ColumnKind::CalculatedTableColumn`** have no sampled TMDL
  form; the descriptors are not mapped. If they appear, the drift policy
  notices them — which is the correct signal, not silence.
- A multi-line expression that continues at exactly the property level
  (depth+1) after a non-empty `=` value is indistinguishable from properties
  and reads as a sibling; TMDL serialization keeps expression bodies below
  the property level, so this has not been observed.
