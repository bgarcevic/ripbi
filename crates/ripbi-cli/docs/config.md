# `ripbi.toml`

Found in the working directory or its nearest ancestor; relative paths resolve against
the file's own directory. Flags override the file; the file overrides discovery.
Unknown keys are errors, so a misspelled setting cannot silently change a scan.

```toml
target = "samples/AdventureWorks Sales.SemanticModel"  # used when no PATH is given
reports = ["samples/AdventureWorks Sales.Report"]      # extra roots when discovery finds none
queries_from = "exports/semantic-model-logs.csv"        # logged queries as consumers (scan and deps)
queries_item = "AdventureWorks Sales"                   # the log's model, when its name differs

[scan]
ignore = ["'*Time Intelligence'[*]", "*Legacy*"]       # object-name globs, never reported unused
stats_from = "exports/AdventureWorks Sales.abf"        # sizes for a PBIP/TMDL model, or "none"
external_consumers = "none"                            # nothing outside these reports reads the model

[apps]
"apps/sales-dashboard" = "models/Sales.SemanticModel"  # a Fabric app and the model(s) it reads
```

| Key | Flag it stands in for | Read by |
|---|---|---|
| `target` | `PATH` (`--model` for `deps`) | `scan`, `deps`, `report` |
| `reports` | `--report` | `scan`, `deps`, `report` |
| `queries_from` | `--queries-from` | `scan`, `deps` |
| `queries_item` | `--queries-item` | `scan`, `deps` |
| `[scan].ignore` | — | `scan` |
| `[scan].stats_from` | `--stats-from` | `scan` |
| `[scan].external_consumers` | — | `scan` |
| `[apps]` | — | `scan`, `deps`, `report` |

`target` and `reports` feed the input ladder every command shares: see
[inputs and pairing](scan/overview.md#inputs-and-pairing). `--model` disables
`target`, and passing `--report` replaces `reports`.

`stats_from` is the config form of `--stats-from` (see [Storage sizes](scan/storage.md)):
a path resolved against the file's directory, or `none` to turn off
`.pbi/cache.abf` auto-detection. The flag replaces it. It is ignored when the
scanned model carries its own storage catalog (a PBIX or `.abf`).

`reports` may list Excel workbooks (`.xlsx`, `.xlsm`) beside report folders; their
pivots and CUBE formulas then count as consumers, as with `--report book.xlsx`.

`external_consumers = "none"` says nothing outside the scanned reports reads the
model — no thin reports in other workspaces, Analyze in Excel, or XMLA clients — and
drops the check-before-removing hint a scan with findings prints under its verdict.
It changes no finding and no exit code. `none` is the only value.

`[apps]` pairs Fabric app folders found under a search folder with the models they
read: each key is an app folder, each value a model path or a list of them, both
resolved against the file's directory. `fabric.yaml` names its models by service ID,
which a local model folder does not record, so without an entry an app pairs only by
a `semanticModels` key matching the model's name (see
[inputs and pairing](scan/overview.md#inputs-and-pairing)). An entry is final: the app
is included in scans of the listed models and ignored in every other.

`queries_from` is the config form of `--queries-from`, resolved against the file's
directory; it sits at the top level because `deps` reads it too. `queries_item` picks
the log's model by name or `ItemId` GUID. The flags replace both (see
[Logged queries](scan/queries.md)).

`ignore` patterns are case-insensitive globs where `*` matches any run of characters
and `?` exactly one; everything else (quotes and brackets included — they appear in
display ids) is literal. A pattern matches a finding when it matches the full display
id (`'Sales'[Draft Amount]`) or the bare object name; for a broken binding it matches
the written reference whole (`'Sales'[Color]` — a binding has no bare name of its own).
For a stale bookmark, both forms use its author-facing `displayName` when present,
falling back to its source object name.
Suppressed findings are excluded
from the output and the exit code, and counted in `summary.ignored`. The suppression
applies before type selection: an object matched by both is simply gone. The auto
date/time machinery can be silenced wholesale this way — see the recipe at the end of
[Human output](scan/human-output.md). For an object that is unused on purpose, prefer
a [`ripbi_keep` annotation](scan/keep.md). `deps` never applies `ignore`: a dependency
view shows what the graph says, ignored or not.
