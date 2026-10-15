# SARIF, Markdown, and Azure DevOps

## SARIF

`--sarif` writes a [SARIF 2.1.0](https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html)
log to stdout: one run, tool `ripbi`, and one result per reported finding. Code scanning
turns it into pull request annotations and alerts that can be dismissed. Skip notices stay
on stderr, as in the text modes, and the exit code is unchanged.

### Rules

Every log lists the full rule catalogue in this order, so `ruleIndex` is stable. Rule ids
are a contract: they are never renamed.

| Rule id | Finding | Level |
|---|---|---|
| `RIPBI-UNUSED-MEASURE` | unused `measure` | warning |
| `RIPBI-UNUSED-COLUMN` | unused `column` | warning |
| `RIPBI-UNUSED-HIERARCHY` | unused `hierarchy` | warning |
| `RIPBI-UNUSED-TABLE` | unused `table` | warning |
| `RIPBI-UNUSED-PARTITION` | unused `partition` | warning |
| `RIPBI-UNUSED-RELATIONSHIP` | unused `relationship` | warning |
| `RIPBI-UNUSED-ROLE` | unused `role` (roles are roots, so this rule does not fire today) | warning |
| `RIPBI-UNUSED-CALCULATION-ITEM` | unused `calculation_item` | warning |
| `RIPBI-UNUSED-EXPRESSION` | unused shared `expression` | warning |
| `RIPBI-UNUSED-FUNCTION` | unused `function` | warning |
| `RIPBI-UNUSED-REPORT-MEASURE` | unused `report_measure` | warning |
| `RIPBI-STALE-BOOKMARK` | stale `bookmark` | warning |
| `RIPBI-BROKEN-VISUAL` | broken visual binding | error |
| `RIPBI-BROKEN-ARTIFACT` | DAX artifact with unresolved references | error |
| `RIPBI-AUTO-DATE-TIME` | auto date/time table, `unused_by_reports` or `dead` (an `in_use` row is advice and emits no result) | warning |

The level is presentation only. The selection flags still decide what gates the exit
code: without `--broken`, breakage is reported but does not gate.

### Locations

- **PBIP and TMDL:** the `.tmdl` file that declares the object, with `region.startLine`
  on its declaration line. An object with no recorded declaration points at `model.tmdl`.
- **`.pbix`, `.pbit`, `.abf`, `model.bim`:** the model file, with no region.
- **Report-side findings** (broken visual bindings, report measures, bookmarks, and
  report-level broken artifacts): the report's `definition/report.json`, or the report
  archive itself.

Every result also carries a `logicalLocations` entry: the finding's display id as
`fullyQualifiedName` and its kind as `kind`. A path under the working directory is
written relative to it with `uriBaseId: "%SRCROOT%"`, so run the scan from the repository
root. A path outside it becomes an absolute `file:///` URI.

### Fingerprints

`partialFingerprints["ripbiFinding/v1"]` is the SHA-256 of the finding's
`--compare-root` fingerprint: its kind and display id, case-folded, plus a broken
binding's reason and site or an auto date/time verdict. It never includes a path, so
code scanning tracks one alert across runs, branches, and moved files.

### Under `--compare-root`

Findings that already existed in the other checkout are kept rather than dropped as in
the other modes. They carry
`suppressions: [{"kind": "external", "justification": "already in <DIR>"}]` (`<NAME>` under `--compare-label`), which is what
code scanning expects: the pull request annotates only new findings, and existing alerts
stay tracked. Only unsuppressed findings gate the exit code. The `Fixed since` list has no
SARIF form; code scanning closes an alert itself when its fingerprint stops appearing.

### Uploading

GitHub Actions (needs the `security-events: write` permission):

```yaml
- run: rib scan --compare-root ../base --sarif-file ripbi.sarif
- uses: github/codeql-action/upload-sarif@v4
  if: always()
  with:
    sarif_file: ripbi.sarif
    category: ripbi
```

`--sarif-file` keeps the readable report in the job log; `--sarif > ripbi.sarif`
writes the same log. `if: always()` uploads it even when the scan step fails the job
with exit `1`.

Azure DevOps: publish `ripbi.sarif` as a build artifact named `CodeAnalysisLogs`. The
[SARIF SAST Scans Tab](https://marketplace.visualstudio.com/items?itemName=sariftools.scans)
extension shows it on the build summary. For warnings and errors without an extension,
see [Azure DevOps](#azure-devops).

## Several outputs from one scan

A CI job usually wants several outputs at once: SARIF for annotations, JSON for counts,
Markdown for a job summary, and a readable log. The stdout modes are exclusive, so the
file flags write the others from the same scan, without running it again (twice
again, under `--compare-root`):

```sh
rib scan --compare-root ../base   --sarif-file ripbi.sarif --json-file ripbi.json --markdown-file ripbi.md
```

- Each file is byte-identical to its stdout mode: `--sarif-file` to `--sarif`
  (existing findings kept as suppressed results), `--json-file` to `--json`.
  `--markdown-file` has no stdout mode.
- They combine with any stdout mode, `-q` included, and with each other.
- A relative `PATH` resolves against the working directory. The file is replaced if it
  exists; its folder must exist. A file that cannot be written is exit `2`.
- The files are written once the scan has its findings, before stdout. A run that
  fails earlier with exit `2` (a bad PATH, an ingestion error) writes none of them, so
  a pipeline should not read a file left over from an earlier run.

## Markdown

`--markdown-file` writes a GitHub-flavored Markdown summary of the scan, sized for a
GitHub job summary (`$GITHUB_STEP_SUMMARY`), an Azure DevOps build summary
(`##vso[task.uploadsummary]`), or a pull request comment:

````markdown
### ripbi scan: `Mini.SemanticModel`

**1 new finding** · 0 fixed · 2 already in `../base`

| Finding | Count |
|---|--:|
| Unused measures | 1 |

#### New findings (1)

| Type | Object |
|---|---|
| measure | `'Sales'[Draft KPI]` |

> Remove the new unused objects, or if something outside these reports reads one (an Excel pivot, another workspace's report), mark it in the model: `annotation ripbi_keep = <reason>`

#### Worst tables

| Table | Unused |
|---|--:|
| `'Sales'` | 1 |
````

- **Headline.** `**N findings**`, the `summary.findings` count. Under `--compare-root`:
  `**N new findings** · F fixed · E already in <DIR>`, with `--compare-label`'s name
  in place of `DIR` when given.
- **Counts.** One row per finding type with findings, in the human output's order,
  then auto date/time tables, broken visual bindings, and broken artifacts.
- **Findings.** One row each, the same findings the other modes report: type, object,
  and a `Detail` column when any row has one (a storage size, a broken binding's reason
  and site, an artifact's unresolved references, an auto date/time verdict). The list
  stops after 50 rows with a count of the rest; `No findings.` (`No new findings.`
  under `--compare-root`) when there are none. Under `--compare-root`, new unused
  objects end with a quoted hint: remove them, or keep one with a `ripbi_keep`
  annotation (see [Keeping objects on purpose](keep.md)).
- **Fixed since `<DIR>`.** Under `--compare-root`, the other checkout's findings this
  scan no longer detects, also capped at 50 rows, with the parse-damage caveat when it
  applies.
- **Worst tables.** The tables with the most unused findings, as `--summary` ranks them.
- **Notes.** `[scan].ignore` suppressions and parser skips, as counts. The skips
  themselves are on stderr.

Object names are code spans, with `|` escaped so a name cannot break the table. The
layout may change between releases; parse `--json`, not the Markdown.

## Azure DevOps

`--azure-devops` writes one
[`task.logissue`](https://learn.microsoft.com/azure/devops/pipelines/scripts/logging-commands#logissue-log-an-error-or-warning)
logging command per finding on stdout. The agent turns each into a warning or error on the
run's summary page and the step's log, with no extension or published artifact:

```text
##vso[task.logissue type=warning;sourcepath=Mini.SemanticModel/definition/tables/Sales.tmdl;linenumber=10;code=RIPBI-UNUSED-MEASURE;]Unused measure 'Sales'[Legacy Total]: no report reaches it.
```

Each command carries the same fields as the SARIF result for that finding:

| Property | Value |
|---|---|
| `type` | The rule's SARIF level: `warning`, or `error` for broken visuals and broken artifacts |
| `sourcepath` | The SARIF location's file, relative to the working directory with `/` separators; an absolute path when the file lies outside it. Omitted when no file is known |
| `linenumber` | The TMDL declaration line; omitted for single-file formats and report-level findings |
| `code` | The SARIF rule id (see [Rules](#rules)) |
| message | The SARIF result message |

`%`, carriage returns, and line feeds in a value are escaped as the agent expects
(`%AZP25`, `%0D`, `%0A`); property values also escape `;` and `]`. In-use auto
date/time tables are advice, not findings, and log nothing, as in SARIF. Under
`--compare-root`, findings that already existed in the other checkout are dropped, as in
every mode but SARIF. The exit code is unchanged: a finding still fails the step with
`1`, so the logged issues and the gate agree. Run the scan from the repository root so
`sourcepath` is repository-relative.
