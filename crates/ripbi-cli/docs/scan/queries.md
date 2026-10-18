# Logged queries (workspace monitoring)

An annotation declares a consumer; a query log observes one. With [workspace
monitoring](https://learn.microsoft.com/fabric/fundamentals/workspace-monitoring-overview)
enabled, Fabric records every query a semantic model serves — report visuals, Excel
pivots (MDX), DAX Studio, XMLA clients — in the `SemanticModelLogs` table of the
workspace's monitoring Eventhouse. Export the `QueryEnd` events and pass the file:

```kql
SemanticModelLogs
| where OperationName == "QueryEnd" and ItemName == "AdventureWorks Sales"
| where Timestamp > ago(30d)
| project Timestamp, OperationName, OperationDetailName, ItemId, ItemName,
          ApplicationName, ApplicationContext, ExecutingUser, EventText
```

```console
ripbi scan --queries-from exports/semantic-model-logs.csv
```

- **Every object a logged query names directly is a root**, like a report binding:
  it is live, and so is everything it uses. DAX queries resolve through the same
  binder as model DAX (query-local `DEFINE MEASURE`/`VAR` names resolve to nothing);
  MDX queries name `[Measures].[X]` measures and `[Table].[Column]` attribute or user
  hierarchies. A queried table keeps its columns alive — `EVALUATE 'Sales'`, an Excel
  drillthrough or `ADDCOLUMNS('Sales', …)` returns every one — but not its measures,
  which a table expression never returns.
- **Formats:** the CSV an Eventhouse or KQL queryset exports (a UTF-8 BOM, quoted
  multi-line fields), a JSON array of row objects (`az monitor log-analytics query`,
  pandas `to_json(orient="records")`), a Kusto REST v1/v2 response, or a Log Analytics
  query API response. Any of them may be gzip-compressed (`.csv.gz`) or the only file
  in a zip archive; the format is sniffed from content, not the extension.
  Columns are matched by name; only `EventText` is required. An unfiltered export
  works too: rows whose `OperationName` is not `QueryEnd` (storage-engine scans,
  discovers, metrics) are skipped silently. The Log Analytics `PowerBIDatasetsWorkspace`
  table's column names (`TimeGenerated`, `ArtifactName`) are read as well.
- **Size limits.** Kusto truncates a result at 500,000 rows or 64 MB by default (lift
  it with `set notruncation;` before the query) and marks it partial rather than
  failing — a truncated log silently undercounts. Filtering to `QueryEnd` in the KQL,
  as above, keeps even months of a busy model far below that.
- **One model per log.** An export can cover every model in the workspace. The rows
  whose `ItemName` (or `ItemId`) is this model are used — the model is the
  `.SemanticModel` folder's or `.pbix`/`.pbit`/`.abf` file's name, or `--queries-item
  NAME|ID` (`queries_item` in `ripbi.toml`) when the service name differs — and
  another model's queries are ignored, with a note: its `'Date'[Year]` says nothing
  about this model's. Rows that name no model are kept. A log that covers several
  models but not this one, or has no rows for an explicit `--queries-item`, counts
  none of its queries — the log is supplementary evidence, and a model nobody
  queried in the window is a real answer, not a failed run. A log of a single model
  under another name is used — most likely the same model, named differently
  locally. Either way the mismatch is a `stale_state` skip notice, so the run goes on
  but `--strict` fails it: in CI, a typo'd `--queries-item` or a renamed model never
  passes silently. Prefer the model's `ItemId` GUID as the key there (below) — it
  survives renames and folder names.
- **Notes:** one `·` note names the query count and the log's date window; a `!` note
  says how many queries other models' rows accounted for. The consumer hint under a
  verdict with findings changes to say that consumers outside the window stay invisible — a log is
  only as long as its retention, so a monthly report may not show up in a week of logs.
- **Output.** `--json` counts the queried objects in `summary.queried`;
  `ripbi deps --impact` lists them in a `Queried` section (see [deps.md](../deps.md)).
  User names in the log never reach any output — only distinct counts.
- An unreadable file, or one with no `EventText` column, is exit `2`. A `QueryEnd`
  row with no text is a skip notice.

## Pulling the log in CI

The Eventhouse's REST endpoint returns a Kusto v1 response, which `--queries-from`
reads as is. A service principal with read access to the workspace's monitoring KQL
database (the tenant must allow service principals to use Fabric APIs) is enough;
the query URI and database name are on the KQL database's page in Fabric. Pin the
model by its item id rather than its name — the GUID after `/semanticmodels/` (or
`/datasets/`) in the model's Fabric URL — and gate with `--strict`, so a log that
stops matching fails the build instead of quietly counting nothing:

```toml
# ripbi.toml
queries_from = "query-log.json"
queries_item = "00000000-0000-0000-0000-000000000000"  # the model's ItemId
```

```bash
az login --service-principal -u "$CLIENT_ID" -p "$CLIENT_SECRET" --tenant "$TENANT_ID"
TOKEN=$(az account get-access-token --resource "$KUSTO_URI" --query accessToken -o tsv)
KQL="SemanticModelLogs
  | where Timestamp > ago(30d) and OperationName == 'QueryEnd' and ItemId == '$MODEL_ID'
  | project Timestamp, OperationName, OperationDetailName, ItemId, ItemName,
            ApplicationName, ApplicationContext, ExecutingUser, EventText"
jq -n --arg db "$KUSTO_DB" --arg csl "$KQL" '{db: $db, csl: $csl}' |
  curl -sSf -X POST "$KUSTO_URI/v1/rest/query" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    -d @- -o query-log.json
ripbi scan --strict   # queries_from and queries_item come from ripbi.toml
```

With Log Analytics instead, `az monitor log-analytics query --workspace <guid>
--analytics-query "PowerBIDatasetsWorkspace | where OperationName == 'QueryEnd' …"`
(project `ArtifactId` beside `ArtifactName` to match by id)
writes row objects ripbi reads the same way. Drop `ExecutingUser` from the projection
to keep user names out of the file entirely, at the cost of the distinct-user count.
