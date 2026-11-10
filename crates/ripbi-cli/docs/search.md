# `ripbi search`: find text in DAX and Power Query

Use `search` to find every expression that mentions something: a function, a
column, a server name, a `TODO`. These examples run from the root of a clone of
this repository and use the committed AdventureWorks model:

```sh
ripbi search DIVIDE --model "samples/AdventureWorks Sales.SemanticModel"
ripbi search "'Sales'[Sales Amount]" --model "samples/AdventureWorks Sales.SemanticModel" --type measure
ripbi search --regex "Table\.\w+" --model "samples/AdventureWorks Sales.SemanticModel" --m
```

The first one prints:

```
'Sales'[Profit %]  measure
     1:1  DIVIDE([Profit], [Sales])

'Sales'[Average Sales per Order]  measure
     1:1  DIVIDE([Sales], [Orders])

'Sales'[Customers %]  measure
     1:1  DIVIDE([Customers], CALCULATE([Customers], ALLSELECTED()))

calculation item 'Time Intelligence'[YoY %]
    1:82  VAR __py = CALCULATE(SELECTEDMEASURE(), SAMEPERIODLASTYEAR('Date'[Date])) RETURN DIVIDE(SELECTEDMEASURE() - __py, __py)

4 matching lines in 4 objects
```

The search is line-based, like `grep`: each matching line is shown under the
object that owns the expression, as `line:column  text`. It reads the text as
written, so comments and string literals match too.

<!-- Maintainers: ripbi-app's search.rs and ripbi-cli's search.rs implement this contract. -->

## Command form

```
ripbi search PATTERN [flags]
```

`PATTERN` is required. The model is found the way `deps` finds it: `--model`,
else the `ripbi.toml` `target`, else the model `--report` paths bind, else
discovery in the current directory. A model with no reports is fine.

## What is searched

- **DAX**: measures (expression, dynamic format string, detail rows, KPI
  target, status, and trend), calculated columns, calculated tables, calculation
  items and their group's selection expressions, RLS filters, table detail rows,
  refresh-policy change detection, user-defined functions, and the report-level
  measures of every report read.
- **Power Query (M)**: table partitions and shared expressions.

Not searched: object names and descriptions, native-query partitions, visual
bindings (use [`ripbi report`](https://bgarcevic.github.io/ripbi/report.html)
for those), and inline DAX a visual writes itself.

## Flags

| Flag | Meaning |
|---|---|
| `--model <PATH>` | Search one named semantic model; disables discovery |
| `--report <PATH>` | Report whose report-level measures are searched too; repeatable; replaces `ripbi.toml` `reports` |
| `--regex` | Read `PATTERN` as a regular expression ([syntax](https://docs.rs/regex/latest/regex/#syntax)) |
| `--case-sensitive` | Match case exactly. The default ignores case, as DAX does |
| `--dax` | Search DAX only |
| `--m` | Search Power Query only |
| `--type <TYPE>` | Only objects of this type: `measure`, `column`, `table`, `partition`, `role`, `calculation_item`, `expression`, `function`, `report_measure`; repeatable or comma-separated |
| `--json` | Machine-readable JSON, below |
| `--plain` | One tab-separated record per matching line |
| `-q`, `--quiet` | Print nothing |
| `-v`, `--verbose` | List every pairing note and skipped item on stderr |
| `--no-color` | Never color output |
| `--no-input` | Never prompt; fail where a picker would appear |

Without `--regex` the pattern is plain text: `SUM(` needs no escaping. A match
never spans lines.

## Output

Results go to stdout; the pairing header, notes, and skip notices go to stderr.
An object the parser skipped was not searched, so skip notices print in every
mode.

### Human

One block per expression: the owner's id (and its type, where the id does not
already say it), then its matching lines.
The heading also names the property when it is not the main expression
(`format string`, `detail rows`, `rls filter`, …), `Power Query` for M, and the
report path of a report-level measure. A line with several matches is listed
once, at its first match. The closing line counts matching lines and objects,
or says `No matches for "PATTERN"`.

### `--plain`

```
<type>	<id>	<language>	<property>	<line>	<column>	<text>
```

`language` is `dax` or `m`. `line` is 1-based within the expression and
`column` is the 1-based character position of the first match. Tabs inside
`text` become spaces, so a record always has seven fields. No closing line.

```sh
ripbi search USERELATIONSHIP --plain | cut -f2 | sort -u   # the objects, once each
```

### `--json`

```json
{
  "schema_version": 1,
  "pattern": "DIVIDE",
  "target": "samples/AdventureWorks Sales.SemanticModel",
  "summary": { "hits": 4, "objects": 4 },
  "hits": [
    {
      "id": "'Sales'[Profit %]",
      "type": "measure",
      "report": null,
      "language": "dax",
      "property": "expression",
      "line": 1,
      "column": 1,
      "text": "DIVIDE([Profit], [Sales])"
    },
    …
  ]
}
```

`property` is one of `expression`, `format_string`, `detail_rows`,
`kpi_target`, `kpi_status`, `kpi_trend`, `change_detection`, `rls_filter`,
`no_selection`, `no_selection_format_string`, `multiple_or_empty_selection`,
or `multiple_or_empty_selection_format_string`. `report` is the report path of
a report-level measure, `null` otherwise. New fields may be added under schema
version 1; existing ones keep their meaning.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | The search ran, with or without matches |
| `2` | Usage error (empty pattern, invalid regular expression, unknown `--type`), discovery error, or ingestion error |

`search` is informational, like `deps` and `report`: finding nothing is a
result, not a failure. To gate a pipeline on a match, test the output:

```sh
test -z "$(ripbi search TODO --plain)"
```
