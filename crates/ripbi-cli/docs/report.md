# `ripbi report`: inspect report bindings

Use `report` when you want to check what ripbi read from a report: its pages,
visuals, filters, and model fields. This is useful when a scan finding looks
surprising. These examples run from the root of a clone of this repository:

```sh
ripbi report "samples/AdventureWorks Sales.pbip"
ripbi report "samples/AdventureWorks Sales.pbip" --visuals
ripbi report "samples/AdventureWorks Sales.pbip" --used
```

The default tree shows each visual's bindings. `--visuals` gives one row per
visual; `--used` lists the model objects the report keeps alive. The command
is informational: a produced inventory exits `0`, even if it lists unresolved
bindings. Add `--broken` to the report command to focus on those bindings.

The rest of this page is the detailed reference for inputs, filters, output
formats, and exit codes.

<!-- Maintainers: src/report.rs and src/report/render.rs implement this contract. -->

## Inputs and pairing

```
ripbi report [PATH] [flags]
```

`PATH` resolves exactly as `scan` resolves it: a `.pbip` file, a project folder, a
`.SemanticModel` item folder, or a `.Report` item folder (which pairs with its model by
stem sibling, sole model sibling, its `definition.pbir` path, or — for a `byConnection`
reference — the dataset name, when exactly one sibling model's stem or display name
carries it; a `.pbip` with no model of its own pairs through its report the same way). `--model` names the model directly (a `.SemanticModel` folder, its
`definition/` folder, a folder holding `model.tmdl`, or the project's `.pbip`), with
`--report` adding report roots and plain folders searched recursively as in `scan`.
`--report` alone (no PATH, no `--model`) derives the model from the named reports'
pairing and inventories exactly those reports. Without any of these — and without
`target`/`reports` in `ripbi.toml` — the command discovers projects in the current
directory with the same picker rules. The `ripbi.toml` `target` and `reports` keys work
here exactly as in `scan`.

When no model pairs at all — the shared-dataset case, a thin report whose dataset
lives only in the workspace — the command refuses with exit `2` and a hint.
`--allow-no-model` opts into inventorying the report alone: pages, visuals, filters,
and written references, with no unresolved claims (nothing to resolve against) and no
`--used`. `--broken` is refused the same way. A mistyped path stays an error even
under the flag — it rescues a missing model, never a wrong path. The JSON `target`
reads `(no semantic model)` in that mode.

## Streams

| Content | Stream | Notes |
|---|---|---|
| Inventory tree, view tables, plain records, JSON | stdout | the machine-readable side |
| Reading line (`Reading … with N report(s)`) | stderr | one line |
| Pairing notes from a walk (`Note:` by-name matches, `Ignored … bound to other models`) | stderr | same collapsed one-line forms as `scan` |
| Model-less note (`No semantic model paired — listing … as written`) | stderr | under `--allow-no-model` |
| Next-command pointer (`N unresolved — see: ripbi report --broken`) | stderr | after the `--visuals` view, when any visual is broken |
| Empty-filter note (`no pages match …`) | stderr | when filters match nothing; the run still exits `0` |
| Skip notices (parser drift, stale saved state) | stderr | grouped under one header; suppressed in `--json` mode, where the JSON carries them |
| Errors + hints | stderr | `error: …` / `hint: …` |

`-q/--quiet` suppresses everything on both streams; the exit code is the only output.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | The inventory printed. Unresolved bindings are *listed*, never failed on — this command has no findings gate |
| `2` | Error: usage, bad PATH, unsupported archive, ingestion failure, a pair with no reports, ambiguous discovery off-TTY |

## Flags

| Flag | Effect |
|---|---|
| `--model <PATH>` | Inventory one named semantic model (`.SemanticModel`, its `definition/`, a folder holding `model.tmdl`, or the project's `.pbip`). Disables cwd discovery and the `ripbi.toml` `target`; plain `--report` folders become search folders. Conflicts with `PATH` |
| `--report <PATH>` | Report root; repeatable. Replaces `reports` from `ripbi.toml`. With no PATH and no `--model`, the reports' pairing derives the model and exactly these reports are inventoried |
| `--json` | JSON on stdout (schema below). Mutually exclusive with `--plain` and the views |
| `--plain` | One tab-separated record per line, each starting with its record type, for grep/awk. Mutually exclusive with `--json` and the views |
| `--pages` | List pages as a table (all pages, or those `--page` selects) |
| `--visuals` | List visuals: page, type, field and unresolved counts |
| `--fields` | List every binding: one row per field reference |
| `--used` | List every model object the report keeps alive |
| `--page <GLOB>` | Only pages matching a name or display-name glob |
| `--visual <GLOB>` | Only visuals matching a visual-name glob; repeatable |
| `--match <GLOB>` | Only references matching a target glob; repeatable |
| `--broken` | Only broken visual bindings (unresolved references) |
| `--allow-no-model` | Inventory the reports even when no semantic model pairs with them, instead of refusing (exit 2). Bindings are listed as written; `--used` and `--broken` are refused |
| `-q`, `--quiet` | No output; exit code only |
| `--no-color` | Never color (color is also off off-TTY, under `NO_COLOR`, or `TERM=dumb`) |
| `--no-input` | Never prompt; fail where a picker would appear |

## Views (`--pages`, `--visuals`, `--fields`, `--used`)

A view flag prints an aligned table — one row per page, visual, binding, or used model
object — instead of the tree. A view with no filter returns **all** objects of that
type; the filters below narrow it. Several view flags print their tables in sequence.
Views are for humans: `--json`/`--plain` (which always carry the full inventory) reject
them, and the tables are plain, colorless, and line-based.

```text
$ ripbi report --visuals "Broken.pbip"
page      visual  type  fields  unresolved
--------  ------  ----  ------  ----------
Overview  V1      card  1       0
Overview  V2      card  1       1
Overview  V3      card  1       1
Overview  V4      kpi   1       0
```

- `--pages` — `page`, `display`, `hidden`, `mobile`, `visuals`. `-` marks a page
  without a display name.
- `--visuals` — `page`, `visual`, `type` (the `visualType` as written), `fields`
  (binding rows), `unresolved` (count). When any visual carries unresolved bindings, a
  dim stderr line points at `--broken`.
- `--fields` — `page`, `visual`, `site`, `kind`, `target`, `note`: one row per field
  well, sort, conditional-formatting rule, and alt-text reference, one per filter
  target (`kind: filter`) and condition-tree reference (`kind: filter_ref`), and one
  per unresolved binding (`kind: unresolved`). The note carries `(inactive)`, the
  filter's name and type (`Filter5 (Categorical)`), or the unresolved reason. Report-
  and page-level rows use `-` for the segments above them.
- `--used` — `kind`, `id`: every model object reachability keeps alive, in scan's
  vocabulary (`measure 'Sales'[Total]`), sorted by kind then id — the complement of
  scan's unused findings. It always describes the whole report: `--page`/`--visual`/
  `--site`/`--broken` do not shrink reachability. `--kind` and `--match` filter its
  two columns — `--kind` here matches the object kinds (`table`, `column`, `measure`,
  `hierarchy`, `partition`, `relationship`, `calculation_item`, `expression`,
  `function`, `role`, `report_measure`), not the binding kinds of `--fields`.

## Filtering

Every column of the `--fields` table has a filter; the same filters narrow every
output mode.

| Flag | Matches | Notes |
|---|---|---|
| `--page <GLOB>` | page object name or display name | |
| `--visual <GLOB>` | visual object name or **type** | a visual's only author-visible identity is its type — PBIR assigns folder names as object names — so `--visual donut*` matches the donut chart and `--visual <hash>` pins one; both are visible in the `--visuals` table |
| `--site <GLOB>` | binding site: `field_well:<role>`, `sort`, `conditional_formatting`, `alt_text`, `filter`, `drillthrough` | |
| `--kind <GLOB>` | row kind: `column`, `measure`, `hierarchy_level`, `aggregation`, `written`, `filter`, `filter_ref`, `unresolved` | |
| `--match <GLOB>` | the written target — `--match "'Sales'[Sales]"` lists every visual and site that touches the field | |
| `--broken` | only the unresolved bindings | scan's selector; the precise form of `--kind unresolved` |

- Patterns are case-insensitive `*`/`?` globs; brackets and quotes are literal, so
  `--match "'Sales'[*]"` selects every `Sales` column and `--page Overview` matches
  exactly.
- Repeats of one flag union (`--kind measure --kind column`); different flags
  intersect (`--kind measure --site sort` = measures used as sort fields).
- `--page`/`--visual` select containers and keep their content; the row filters select
  rows — a visual with no surviving rows and a page with no surviving visuals are
  pruned, and a report left with nothing is dropped. A surviving filter keeps its
  whole block: the declared target and its condition-tree references together.
- Filters narrow **every** output mode — the tree, the views, `--plain`, and `--json` —
  and every count recomputes from what survives, so the numbers always describe the
  visible tree.
- `--used` plays by its own rules: `--kind`/`--match` filter its `kind`/`id` columns
  (see the view above), while `--page`/`--visual`/`--site`/`--broken` leave it alone —
  reachability always describes the whole report.
- Nothing matches: one stderr note (`no pages match Nope*`, `no visuals match zzz*`,
  `no bindings match the given filters`), still exit `0`.

## Human output (default)

A tree, one subtree per report — pages and visuals connected with box drawing,
their references as property rows aligned on a per-block label column:

```text
AdventureWorks Sales — 1 page, 17 visuals
  Pages
    └── Overview (ReportSection) — 17 visuals
        ├── 11a03bbd46fd39147235 — donutChart
        │     Category   column 'Product'[Category]
        │     Y          measure 'Sales'[Cost] (inactive)
        │     sort       column 'Product'[Category]
        │     formatting measure 'Sales'[Cost]
        └── 3a1aeaede6fc79fe5066 — pivotTable
              Rows       hierarchy 'Product'[Products] level 'Category'
              Values     measure 'Sales'[Products] (inactive)
              formatting measure 'Sales'[Products]
              filter     Filterf32699ca5c7851734a77 (Categorical) — 'Reseller'[Business Type]
              …
```

- The report header carries the report name, its page count (desktop pages; the phone
  layout has its own section), its visual count, and — when any exist — the unresolved
  binding count. Under filters, all three describe the surviving tree.
- Pages and visuals connect with `├──` for every child but the last, `└──` for the
  last, and a `│` stalk carrying down through open subtrees. A page line shows the
  display name, the object name when the two differ, `(hidden)` when the page is
  hidden, and the page's visual count. A visual line shows the object name — the full
  PBIR hash, never truncated, so what you see passes to `--visual` unchanged — and the
  `visualType` as written.
- Under each visual, references print as property rows: a label column padded to the
  longest label that visual carries, then the content. At page level the rows collect
  under named group nodes that connect into the page's child chain ahead of its
  visuals — `filters (n)` then `unresolved (n)`, each shown only when non-empty.
  Report-level rows keep their own block under the `Report filters` section.
  - A field row's label is the binding site as a person reads it: the field well's
    role (`Values`, `Category`, `Rows`, …), or `sort`, `formatting` (short for
    `conditional_formatting`), `alt text`. The content is the written reference, with
    a kind word only where it is the one disambiguation — `column` and `measure`,
    which a bare `'Table'[Name]` cannot distinguish; hierarchy levels, aggregations,
    and written expressions self-identify in their target. The complete `kind` and
    `site` vocabularies stay in the `--fields` view, `--plain`, and `--json`. Inactive
    well projections carry `(inactive)` — they still bind.
  - A filter row is `filter <name> (<type>, <display name>) — <target>`, at report,
    page, or visual level, followed by one `also references` line per additional
    field the condition tree touches, aligned under the target. A filter with neither
    name nor metadata prints only its target.
  - An unresolved row's label is the red `unresolved` marker; the content is the
    target and the reason in parentheses, plus `via <artifact>` when the binding lands
    on an artifact whose own DAX is broken. Visual-level rows sit under their visual;
    page-level rows (page filters, drillthrough parameters) under the page's
    `unresolved (n)` group; report-level rows under `Report filters`.
- Targets and visual names are always printed verbatim: what a row shows is what
  `--match` and `--visual` match.
- Report-level lines close each report: `N bookmarks` (a count — saved bookmark state
  is not itemized) and `Report measures: …` when `reportExtensions.json` defines any.

Sections appear only when non-empty: a report with no filters prints no `Report
filters` header, a report without a phone layout prints no `Mobile pages`.

## What "unresolved" means (and does not)

An unresolved binding is strictly a *written reference that names nothing in the model*
— the same verdict, vocabulary, and under-claim rules as `scan`'s broken visual
bindings (issue #60): `table_not_found`, `field_not_found`, `measure_not_found`,
`hierarchy_not_found`, `level_not_found`, or `bound_artifact_broken` (the binding
resolves, but to an artifact whose own expressions no longer resolve). The `--broken`
flag is scan's selector for exactly this set. It is not a runtime refresh failure;
Power BI's own "broken visual" count can differ. When the model ingest recorded
`unknown_object` skips, unresolved claims are suppressed entirely — a skipped table
never became a node, so a "field not found" verdict would be a guess. Bookmarks' saved
bindings are out of scope: they are counted, not itemized.

## `--plain`

One record per line, tab-separated, each starting with its record type. Segments a
record does not carry are `-`. Fields: `report\t<name>`,
`report-measure\t<report>\t<measure>`, `filter\t<report>\t<page>\t<visual>\t<name>\t<type>\t<target>`,
`filter-ref\t<report>\t<page>\t<visual>\t<reference>`, `page\t<report>\t<page>\t<desktop|mobile>`,
`visual\t<report>\t<page>\t<visual>\t<type>`,
`field\t<report>\t<page>\t<visual>\t<binding site>\t<kind>\t<target>`,
`unresolved\t<report>\t<page>\t<visual>\t<target>\t<reason>`
(real output from the golden test report, trimmed):

```text
report	Mini
report-measure	Mini	Budget %
filter	Mini	-	-	Filter1	Categorical	'Product'[Category]
page	Mini	P1	desktop
filter	Mini	P1	-	PageFilter	Categorical	'Date'[Calendar Year]
visual	Mini	P1	V1	slicer
field	Mini	P1	V1	field_well:Category	column	'Product'[Category]
filter	Mini	P1	V1	V1Filter	Advanced	'Reseller'[Business Type]
filter-ref	Mini	P1	V1	'Reseller'[Business Type]
unresolved	Mini	P1	V1	'Product'[Category]	table_not_found
```

Report-level filters use `-` for the page and visual segments. `cut -f1` yields the
record types; `grep '^unresolved'` yields the broken bindings alone.

## `--json`

Pretty-printed, `schema_version: 1`, one document per run. Additive evolution only —
existing fields keep their names and types, scripts may pin the version (real output
from the golden test report, trimmed; `target` reads `(no semantic model)` under
`--allow-no-model`):

```json
{
  "schema_version": 1,
  "target": "…/tmdl/golden/Mini.SemanticModel",
  "reports": [
    {
      "path": "…/pbir/golden/Mini.Report",
      "name": "Mini",
      "pages": [
        {
          "name": "P1",
          "display_name": "Overview",
          "hidden": false,
          "mobile": false,
          "filters": [
            {
              "name": "PageFilter",
              "display_name": null,
              "filter_type": "Categorical",
              "target": "'Date'[Calendar Year]",
              "references": []
            }
          ],
          "unresolved": [
            {
              "target": "'Date'[Calendar Year]",
              "reason": "table_not_found",
              "binding_site": "filter"
            }
          ],
          "visuals": [
            {
              "name": "V1",
              "type": "slicer",
              "fields": [
                {
                  "kind": "column",
                  "target": "'Product'[Category]",
                  "binding_site": "field_well:Category",
                  "active": true
                },
                {
                  "kind": "column",
                  "target": "'Product'[Category]",
                  "binding_site": "sort"
                }
              ],
              "filters": [
                {
                  "name": "V1Filter",
                  "display_name": "Business Type is not Regular Superstore",
                  "filter_type": "Advanced",
                  "target": "'Reseller'[Business Type]",
                  "references": ["'Reseller'[Business Type]"]
                }
              ],
              "unresolved": [
                {
                  "target": "'Product'[Category]",
                  "reason": "table_not_found",
                  "binding_site": "field_well:Category"
                }
              ]
            }
          ]
        }
      ],
      "unresolved": [
        {
          "target": "'Product'[Category]",
          "reason": "table_not_found",
          "binding_site": "filter"
        }
      ],
      "bookmarks": 2,
      "report_measures": ["Budget %"],
      "unresolved_count": 10
    }
  ],
  "skips": { "count": 1, "notices": ["…"] }
}
```

Field rules:

- `reports[]` follows ingestion order; `pages[]` desktop pages in source order, then
  phone-layout pages with `"mobile": true`.
- `fields[].kind` is the binding's own claim: `column`, `measure`, `hierarchy_level`,
  `aggregation`, or `written`. `binding_site` is `field_well:<role>`, `sort`,
  `conditional_formatting`, or `alt_text`. `active` appears on field-well projections
  only and is `false` for inactive ones — which still bind.
- `filters[]` appears at report, page, and visual level with the same shape. `name` is
  the filter's in-scope name (e.g. `Filter5`), `display_name` and `filter_type` the
  author-facing metadata as written (e.g. `Categorical`, `Advanced`), `target` the
  declared filtered field, and `references` the further fields the condition tree
  touches.
- `unresolved[]` appears at all three levels with the same shape: `target`, `reason`
  (the vocabulary above), `binding_site` (the same vocabulary as
  `fields[].binding_site`, plus `filter` and `drillthrough`), and `bound_artifact` —
  present exactly when `reason` is `bound_artifact_broken`. `unresolved_count` is the
  sum of every itemized row.
- `bookmarks` is a count. `report_measures` lists `reportExtensions.json` measure
  names.
- `skips` mirrors scan's: every parser notice, so a half-parsed report is visible in
  the machine-readable output.
