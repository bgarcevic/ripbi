# `ripbi fetch`: download a model and every report built on it

Use `fetch` when the model you want to clean up lives in a Power BI / Fabric
workspace and its reports are spread across other workspaces. A scan is only as
good as its report set: against one of a model's 34 reports, ripbi flagged 1559
objects as unused; against all 34, 457. `fetch` finds every downstream report,
tells you which ones you can read before downloading anything, and writes a folder
that `ripbi scan` runs on with no flags:

```sh
az login
ripbi fetch "https://app.powerbi.com/groups/<workspace>/datasets/<model>/details"
cd Sales
ripbi scan
```

`ripbi fetch --scan` does the last two steps for you. `--dry-run` prints the
forecast and downloads nothing.

The rest of this page is the detailed reference for inputs, sign-in, the forecast,
the folder layout, and exit codes.

<!-- Maintainers: src/fetch.rs, src/fetch/ and crates/ripbi-service implement this contract. -->

## Inputs

```
ripbi fetch [LINK] [flags]
```

The model is resolved from, in order:

- `LINK`, a portal link to a semantic model
  (`…/groups/<ws>/datasets/<id>/details`, `…/semanticmodels/<id>`) or to a report
  (`…/groups/<ws>/reports/<id>/…`). A report link fetches the model that report is
  bound to. `My workspace` links (`/groups/me/`) are not supported.
- `--workspace NAME|ID` and `--model NAME|ID`. Names match case-insensitively; an id
  always works. `--model` without `--workspace` searches every workspace you can see
  and refuses, listing the candidates, when more than one model matches.
- On an interactive terminal, a numbered picker for whatever is missing: a workspace,
  then a model. `--no-input` (or a non-interactive stdin) turns the pickers into an
  error with a hint.

## Signing in

`fetch` never asks for a password and has no app registration of its own. Tokens
come from, in order (`--auth auto`, the default):

| Source | Set up | `--auth` |
|---|---|---|
| A token in the environment | `RIPBI_FABRIC_TOKEN` (Fabric API audience); optional `RIPBI_POWERBI_TOKEN` (Power BI API audience) | `env` |
| A service principal | `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` (optional `AZURE_AUTHORITY_HOST`) | `sp` |
| The Azure CLI | `az login`; ripbi runs `az account get-access-token` | `az` |

A service principal also needs the tenant setting *Service principals can use Fabric
APIs* and a workspace role (see below). Tokens are held in memory only: they never
reach a log line, an error message, or disk, and they are only ever sent to the
Fabric and Power BI API hosts. The first stderr line names the source and the tenant:
`· Signed in via Azure CLI (tenant …)`.

## The forecast

Before downloading, `fetch` lists every report bound to the model and predicts which
ones you can read, from your workspace roles. Exporting a definition needs
**Admin, Member, or Contributor**; a Viewer sees the report but cannot export it.

```
Sales · Data · 34 downstream reports (+34 app copies skipped)
  ✓ 17 downloadable   Marketing (9), HR (7), Finance (1)
  ✗ 17 not readable   Operations (17, viewer)
  · 2 other downstream items not scanned: 2 Dashboard
Scan will be incomplete: 17 reports can't be read.
```

- Reports in workspaces you cannot open are still listed — the relations API does not
  hide them — so the forecast names exactly what a scan would miss.
- **Org-app copies** are skipped: every report published in an app comes back twice,
  and the app copy can never be exported. Copies are told apart by the workspace's
  report listing, or, in a workspace you cannot list, by the org-app listings you can
  see; when neither works, a note says that a name appearing twice is likely a copy.
- Dashboards, paginated reports, other models, and reports bound to a model
  downstream of this one are counted in the `·` line and recorded in the manifest,
  but not downloaded.
- `--verify` cross-checks the list against every workspace's own report listing (the
  relations API is beta) and prints any difference as a `!` note.

If the model itself is not exportable, `fetch` stops with exit `2`. If some reports
are not, an interactive run asks `Continue with the readable reports? [y/N]`; a
non-interactive one refuses with exit `2` unless `--allow-incomplete` is given.

## Downloading

Every export starts at once, then all are polled together, so the service's ~20 s
per-export wait overlaps: 34 reports take about a minute, not ten. Throttled answers
are retried after their `Retry-After`. The roles forecast is checked against the real
answer: a report the forecast called readable that still answers `404` (item-level
permissions) is recorded as `no_access`, and the run exits `2` unless
`--allow-incomplete` (or the interactive confirmation) accepted a partial set.

## The folder

```
<out>/                                # -o DIR, else ./<model name>
  ripbi.toml                          # target + explicit report list
  .ripbi/source.json                  # provenance and completeness
  <Model>.SemanticModel/              # TMDL, definition.pbism, .platform
  reports/<Workspace>/<Report>.Report/
```

- Names are made safe for every OS (`/ \ : * ? " < > |` become `_`); two reports with
  the same name in one workspace get the first block of their id appended.
- TMDL file names keep the service's escaping (`Yes%2FNo.tmdl`).
- `StaticResources/` (images, themes, custom visual packages) is not downloaded: a
  scan never reads it. Open the report from the service, not from this folder.
- Part paths come from the service and are checked before writing: one that could
  leave its item folder fails that item.
- `ripbi.toml` starts with `# Written by \`ripbi fetch\``. A re-fetch rewrites it; a
  `ripbi.toml` without that line is kept, with a note.

The output directory must be empty, or an earlier fetch of the same model, in which
case its model folder and `reports/` are replaced and everything else is left alone.
Anything else is refused with exit `2`; `--force` writes into it anyway, replacing
only those same two folders.

## `.ripbi/source.json`

```json
{
  "schema_version": 1,
  "generator": "ripbi 0.10.0",
  "fetched_at": "2026-10-20T10:10:45Z",
  "tenant_id": "…",
  "model": { "id": "…", "name": "Sales", "workspace_id": "…", "workspace_name": "Data",
             "path": "Sales.SemanticModel" },
  "complete": false,
  "reports": [
    { "id": "…", "name": "Prices", "workspace_id": "…", "workspace_name": "Marketing",
      "status": "downloaded", "path": "reports/Marketing/Prices.Report" },
    { "id": "…", "name": "Rota", "workspace_id": "…", "workspace_name": "Operations",
      "status": "viewer" }
  ],
  "app_copies": [ { "id": "…", "name": "Prices", "type": "Report", "workspace_id": "…", "workspace_name": "Marketing" } ],
  "other_items": [ { "id": "…", "name": "Overview", "type": "Dashboard", "workspace_id": "…", "workspace_name": "Marketing" } ],
  "relations": [ { "item_id": "…", "depends_on": "…", "type": "Association" } ],
  "verification": null,
  "notes": []
}
```

`status` is `downloaded`, `viewer`, `no_access` (not a member, or the export answered
`404`), or `failed` (with `http_status` and `error` when known). `complete` is `true`
only when every report is `downloaded`. `verification` holds the `--verify` result:
`workspaces_checked`, `only_in_listing`, `only_in_downstream`, `failures`.

## Scanning a fetched folder

Run `ripbi scan` from inside the folder: `ripbi.toml` names the model and every
report, so nothing is discovered or paired by name. When `source.json` lists reports
that could not be fetched, the scan emits an `incomplete_report_set` skip notice
naming them — so `--strict` fails on an incomplete set, like any other skip notice
(see [JSON output](scan/json.md)).

## Streams

The forecast, notes, the download counter (on a terminal), and the summary go to
stderr. stdout carries one line: the output folder, as you would type it. `-q` prints
nothing. With `--scan`, the scan's own output follows on the same streams.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Fetched (or forecast, with `--dry-run`), including an incomplete set that `--allow-incomplete` or the prompt accepted |
| `2` | Usage, sign-in, network, or permission error; an incomplete set not accepted; a declined prompt |

With `--scan`, a successful fetch returns the scan's exit code instead.
