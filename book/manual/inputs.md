# Supported inputs

ripbi reads Power BI files from disk. It never connects to a workspace, opens
Power BI Desktop, or reads row data.

| Input | Model | Reports |
|---|---|---|
| PBIP project (`.pbip`, or its folder) | the TMDL `.SemanticModel` | the PBIR `.Report` folders |
| `.SemanticModel` folder, or its `definition/` | TMDL | paired from siblings or `--report` |
| `.Report` folder | paired through its `definition.pbir` | PBIR, including the phone layout |
| `model.bim` (TMSL) | yes | none: pass `--report` |
| `.pbit` template | yes | yes |
| `.pbix` file | yes, decoded from the compressed `DataModel` (metadata only) | yes |
| thin `.pbix` (live connection) | none: pass `--model`, or keep one model beside it | yes |
| `.abf` backup | yes | none: pass `--report` |

PBIX and `.abf` models also carry storage sizes, so a scan of one says what the
unused objects cost. A PBIP or TMDL model can borrow sizes from a backup, a PBIX,
or a VertiPaq Analyzer `.vpax`: see [Storage sizes](scan/storage.md).

## How reports find their model

A report pairs with a model by the path its `definition.pbir` names, then by the
PBIP naming convention (`X.Report` beside `X.SemanticModel`), then by the dataset
name of a live connection. When several models share a folder, `--model` picks one
and the scan walks the folder for the reports bound to it. The full rules are in
[inputs and pairing](scan/overview.md#inputs-and-pairing).

## When a file has something ripbi does not know

Power BI formats change between releases. ripbi never fails because of an unknown
property or object: it skips it and prints a *skip notice* on stderr, grouped under
one header. A skip that could hide a reference is reported, never silent, because
it could make a used object look unused. Pass `--strict` to turn any skip notice
into exit code `2`, which suits CI.

[Source formats and drift policy](formats.md) documents each parser, the TMDL and
PBIR shapes it reads, and the full list of properties skipped on purpose.
