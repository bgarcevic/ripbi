# Source formats — detection, grammar, and drift policy

What each ingestion format's parser accepts, how it maps into the ASTs, and —
most importantly — the policy for everything it does *not* model. Read this
before changing anything under `src/ingest/`.

PBIR report ingestion (`.Report/` folders → `ReportModel`) lands with #5; this
file currently covers the semantic-model side.

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
| `table N` (+ `isHidden`) | `Table` (`detailRowsDefinition` → detail rows) |
| `column N` / `column N = dax` | `Column` (data / `ColumnKind::Calculated`) |
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

## Drift policy: two tiers of skipping

Ingestion entry points return `Ingested<T>` — the parsed value plus a
`Vec<SkipNotice>` (path, TMDL line number, `SkipKind`, detail). Notices are
warnings as data: core never prints, and the CLI decides presentation. They
are collected on every run, not only in debug builds, because a silent skip
can surface later as a false "unused" finding.

**Tier 1 — deliberately unmodeled, silent.** Keys on the curated list below
(and their whole subtrees) are skipped without a notice. Everything on it is
metadata that cannot consume a model object, so skipping it cannot cause a
false "unused" finding. The tests hold this honest in both directions:
`every_sample_parses_without_notices` fails if the list misses something the
samples carry; the golden fixture fails if a modeled key lands on the list.

**Tier 2 — unexpected drift, noticed.** An unknown object (root descriptor
this crate does not know, unknown file or directory under `definition/`), an
unknown property not on the list, a modeled value that fails to parse
(`MalformedValue`), or a reference that cannot be resolved
(`UnresolvedAlias`, reserved for PBIR aliases).

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

### Assumed spellings (no sample carries them)

KPI expressions accept both `target`/`status`/`trend` and the TOM names
`targetExpression`/`statusExpression`/`trendExpression`. `tablePermission`
accepts its filter as the `=` expression (inline or block) or as a
`filterExpression` child. Calculation-group selection expressions accept the
`noSelectionFormatString` and `noSelectionFormatStringDefinition` spellings
(likewise `multipleOrEmptySelection…`). `groupByColumn` (singular, repeated)
feeds `Column::group_by_columns`. If real files disagree, the drift policy
surfaces it as a notice rather than failing.

### Known gaps

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
