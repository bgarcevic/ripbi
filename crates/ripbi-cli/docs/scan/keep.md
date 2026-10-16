# Keeping objects on purpose

Some objects are unused *by the reports ripbi can see* on purpose: a measure only an
Excel pivot or a thin report in another workspace reads, or a column kept for an
upcoming report. Mark them in the model itself with a `ripbi_keep` annotation
(issue #151). Its value is the reason:

```tmdl
measure 'Excel Margin %' = DIVIDE([Margin], [Revenue])
    annotation ripbi_keep = Used by the Finance Excel pivot (FIN-231)
```

- **A kept object is a reachability root**, like a report binding: it is live, and so
  is everything it references — `[Margin]`, `[Revenue]`, and the columns behind them,
  which the Excel pivot needs too. None of them is a finding, and none gates.
- **Any object can carry it:** a table, column, measure, hierarchy, calculation item,
  relationship, shared expression, or function. A kept table keeps its members. A
  kept relationship counts as activated, so its key columns stay live even when it
  is inactive; like every relationship, it does not keep its tables alive.
- **Every model format reads it:** TMDL, `model.bim`, and the model inside a PBIX,
  PBIT, or `.abf`. The name is case-insensitive. Add it in Power BI Desktop's
  TMDL view, Tabular Editor, or the TMDL file; these tools and deployment pipelines
  carry annotations along, and a rename carries the annotation with the object.
  ripbi never writes it.
- **Write the reason.** An empty value (`annotation ripbi_keep =`) still keeps the
  object, but the next reader will not know why.
- **Output.** Kept objects are not listed by `scan`. `--json` counts them in
  `summary.kept`. `ripbi deps --impact` shows the annotation's reason in a `Kept`
  section, so "why is this alive?" answers with the reason (see
  [deps.md](../deps.md)).

Prefer the annotation over a `[scan].ignore` pattern for an intentionally unused
object: the reason sits beside the object, it survives renames, and it keeps the
object's inputs alive where an ignore pattern would leave them reported as dead.
`[scan].ignore` remains the tool for findings you want silenced without a model
edit, such as whole families of generated objects.
