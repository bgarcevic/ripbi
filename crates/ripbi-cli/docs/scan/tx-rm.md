# Removing with tomix

`--emit tx-rm` prints the findings as JSON Lines for
[tomix](https://github.com/bgarcevic/tomix-cli)'s batch removal, `tx rm --from`. `tx`
removes the whole list as **one change**: it resolves every line before anything changes,
judges the dependency guards against the whole set, and saves and validates once. Preview
first, then save:

```console
$ ripbi scan --emit tx-rm | tx rm --from - ./Model.SemanticModel
$ ripbi scan --emit tx-rm | tx rm --from - ./Model.SemanticModel --save
```

```text
# ripbi unused objects in Mini.SemanticModel
# Remove them as one change: tx rm --from <this-file> <model> --save
{"type":"column","table":"Sales","name":"Legacy","id":"'Sales'[Legacy]"}
{"type":"measure","table":"Sales","name":"Legacy Total","id":"'Sales'[Legacy Total]"}
```

| Field | Value |
|---|---|
| `type` | `table`, `column`, `measure`, `hierarchy`, `partition`, `calculation_item`, `relationship`, `role`, `expression`, or `function`. Always set: it keeps a column and a partition that share a name apart |
| `table` | The owning table, for columns, measures, hierarchies, partitions, and calculation items |
| `name` | The object's name, as written in the model. JSON carries any name, so nothing is quoted |
| `path` | Only for a relationship the model gives no name: its endpoints, `'A'[x] -> 'B'[y]`, which `tx` resolves itself |
| `id` | ripbi's display id. `tx` ignores it but echoes the whole line back as the target's `input`, so its JSON result traces each removal to its finding |

- **What it lists.** Exactly the findings the other modes list — `[scan].ignore`, `--type`,
  and `--compare-root` filter it the same way — plus dead auto date/time tables (verdict
  `dead`). An auto date/time table still `in_use` or `unused_by_reports` is advice, not a
  removal: switch the setting off in Desktop instead. Report measures and bookmarks live in
  the report, not the model: they are `# skipped` comment lines, which `tx` skips.
- **Order.** None needed. Lines follow ripbi's identity order; a dead table's members are
  listed beside it and `tx` reports them as `subsumed`.
- **Cascades.** When the set would also change objects that stay — a relationship to a live
  table, a sort-by setting, a perspective entry — `tx` lists them under `Would also change:`
  and refuses until you pass `--cascade`.
- **No findings.** The list is only comments, and `tx rm --from` exits `2` (`No objects to
  remove`). Gate the pipe on ripbi's exit code (`1` means there is something to remove).
- **Rescan after saving.** Removing objects can expose new findings — a key column whose
  only relationship is gone — so run the scan again.

The exit code is unchanged: `1` when there are findings, as in every mode.
