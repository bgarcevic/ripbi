# Report AST

The normalized shape every report source format parses into — PBIR (`definition/`
folders) and PBIR-Legacy (`report.json` Layout). Power BI facts the type definitions
cannot state on their own; read this before changing the AST or adding an ingestion
format.

## What is modeled, and what is not

Modeled: report identity and its dataset link, pages (filters, drillthrough/tooltip
bindings, visuals), visuals (field wells, filters, sort-by, conditional formatting,
tooltip-page references), bookmarks (saved filters and active projections), and
report-level measures (`reportExtensions.json`).

Deliberately absent: page order and the active/landing page (`pages.json`), mobile
layouts (`mobile.json`), themes and resource packages, `semanticModelDiagramLayout.json`,
and every literal *value* a filter or slicer selection persists. None of them reference
model objects, so none can keep one alive. They describe the report; they never bind.
Adding them later is additive — but do not add them speculatively.

## Power BI semantics the types don't show

**One `ReportModel` per report, not one per project.** A semantic model is often shared
by several reports (thin reports, deployment pipelines). Reachability takes the union of
all reports' bindings, so the graph core does not care — but provenance does. A
[`BindingRef`] carries page/visual/bookmark/kind; *which report* is answered by the
`ReportModel` the binding was enumerated from, and that report's `name` completes the
"used by" explanation.

**A slicer is not a kind of binding.** A slicer is a visual with
`visual_type: "slicer"`; its field well is the binding. Saved slicer *selections* are
literal values (data), not references.

**Bindings are written-form, never resolved.** PBIR binds structured JSON entity trees
(`Column`/`Measure`/`HierarchyLevel`/`Aggregation`); legacy Layout binds written names.
Both normalize into `FieldTarget`, which keeps the column-vs-measure discrimination the
entity states outright. Resolution against `ModelIndex` is the graph layer's job; a
`Measure` target should resolve against *report* measures first — within its report, a
report measure shadows a model measure of the same name — then the model.

**Query aliases are the parser's problem.** PBIR filter condition trees introduce
aliases (`From: [{Name: "p", Entity: "Product"}]`, then `SourceRef: {Source: "p"}`).
By the time a filter reaches this AST, `Filter::references` holds alias-resolved
targets. Anything the parser cannot resolve lands in `FieldTarget::Written` — kept
rather than dropped, because a binding we cannot read is still a binding.

**Hidden is not dead.** Hidden pages and locked/hidden filters still apply; every flag
here is display-only, never liveness. Inactive projections bind too — they are one
toggle away from live.

**Bookmarks are roots.** Applying a bookmark re-applies its saved filters and
projections, so a field kept alive only by a bookmark is still alive. Bookmark
bindings enumerate with `bookmark` set, alongside the page and visual they captured.

**Report measures bridge both directions.** A report-level measure's DAX body
references model objects (so `ReportModel::dax_expressions` is an expression source on
top of `TabularDatabase::dax_expressions`), and visuals reference it by name (so it is
a reachability root of its own). Its graph identity is
`ObjectId::ReportMeasure` — deliberately not `ObjectId::Measure`, so a report measure
can never be conflated with a model measure of the same name.

**Tooltip pages are report-internal references.** A visual's `tooltip_page` keeps a
*page* reachable, not a model object, so it lives on the AST but never enumerates as a
binding. The page's own visuals bind fields like any other page's.

## Bindings hide in unobvious places

Mirroring the model side's rule for expressions: missing one binding site means the
objects it references get no roots and are reported unused — a false positive the scan
design forbids. `ReportModel::bindings` is the single enumeration; a new binding-bearing
field must be added there or it is silently invisible to reachability. Beyond visual
field wells, report-side roots also live in:

- report-, page-, and visual-level filters (`filterConfig`) — the declared field *and*
  the fields inside the condition tree
- drillthrough parameters (`pageBinding.parameters[].fieldExpr`)
- sort definitions (`sortDefinition.sort[].field`)
- conditional-formatting rule keys
- bookmarks' saved filters — at all three levels, mirroring a live report:
  report-level (`explorationState.filters`), per page section, and per visual —
  and their saved projections
- report-level measures, whose *bodies* consume model objects even though the measure
  itself is report-owned

## Provenance vocabulary

A binding's site answers "where does this come from", which later powers explanations
like `'Sales'[Amount]` ← visual `Card 1` on *Sales overview* (page 2) and per-report
slicing:

| Field | `None` means |
|---|---|
| `page` | report-level (report filter, or a report measure reference) |
| `visual` | page-level or report-level |
| `bookmark` | a live binding, not saved state |

The kind (`FieldWell { role }`, `Filter`, `Sort`, `Drillthrough`,
`ConditionalFormatting`) says what the binding *does*; the role string preserves the
well's name as written (`"Category"`, `"Y"`, `"Tooltips"`).

[`BindingRef`]: ../src/report.rs
