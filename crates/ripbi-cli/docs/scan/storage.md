# Storage sizes

PBIX and `.abf` models carry the engine's storage catalog, so a scan of one also
says what the unused objects cost (issue #122). Only metadata is read: the size of
each data file the backup lists, attributed to the table, column, or relationship
it belongs to — dictionaries, column segments, attribute-hierarchy and relationship
indexes. PBIP, TMDL, `model.bim`, and PBIT inputs have no storage catalog of their
own; they get sizes from a storage source (below), and without one their output is
unchanged.

```text
Columns (35)
  'Opportunity'[Name]  (25.3 KB)
    ← nothing references it

✗ 37 unused objects   62 of 99 reachable from 41 roots
  Unused storage: ≈ 165.6 KB of 318.5 KB on disk (35 objects with size data)
```

- Tables, columns, and relationships carry a size; measures and the other kinds
  never do. A column's size is its dictionary, segments, and attribute hierarchy;
  a table's is every file of its own, its columns, and the relationships whose
  "from" side it is.
- The `Unused storage:` line under the verdict (both human modes) totals the reported findings,
  each file once: a column or relationship whose table is itself a finding is
  covered by the table's size. `of …` is the whole model's data. It prints only
  when some finding has a size.
- Sizes are in 1024-based units (`KB`, `MB`, `GB`), as VertiPaq Analyzer and
  DAX Studio show them. They measure the uncompressed files of the backup, which
  track the model's memory footprint more closely than the compressed `.pbix`.
- When some of an object's files are missing from the backup log, its size is a
  lower bound: a column counts only its dictionary, a table or relationship only
  the files found. The finding then reads `(≥ 1.2 MB)` and the total line says
  `at least … ; a lower bound`.

## Storage sources for PBIP and TMDL models

The model, graph, and findings always come from the scanned model; a storage source
only fills in sizes (issue #129). It is, in order of precedence:

1. `--stats-from <PATH>`: an `.abf` backup, a PBIX saved with its data, a PBIP's
   `cache.abf`, or a VertiPaq Analyzer `.vpax` (see below). `--stats-from none` attaches nothing.
2. `[scan].stats_from` in `ripbi.toml`, with the same values.
3. Auto-detection: when the model is a `.SemanticModel` folder (or its
   `definition/`, or the `model.bim` inside it) and
   `<Model>.SemanticModel/.pbi/cache.abf` exists — Power BI Desktop writes it when a
   PBIP is saved with data — it is used with no flag. `.pbi/` is gitignored by
   default, so this mostly helps local runs; CI passes `--stats-from` with an exported
   `.abf`, `.pbix`, or `.vpax`.

A PBIX or `.abf` model always keeps its own catalog: `--stats-from` with one is a usage
error, and the config setting is ignored.

Sizes attach by exact object identity (case-insensitive, like every other name). A
table, column, or relationship the source does not name — added or renamed since the
cache was saved — has no size; nothing is guessed across renames. `of …` on the total
line is the source's data, which may include objects the model no longer has.

```text
  ! stats from .pbi/cache.abf cover 212 of 240 tables and columns
  ! stats may be stale: .pbi/cache.abf is older than the model files
```

The coverage note prints when the source leaves some tables or columns without a size
(`--verbose` prints it for full coverage too); the staleness note when the
source was written before the newest file of the model (the `.pbi/` folder aside).
A third note flags a source with no rows in any table loaded by Power Query (or a
source query) — a model saved or exported before its first refresh, such as a PBIP
opened in Desktop and exported to `.vpax` straight away; its sizes are the engine's
empty minimums. Calculated tables are left out: the engine fills them without a refresh.
An explicit source that cannot be read, or has no storage catalog (a TMDL folder,
`model.bim`, a PBIT, a thin PBIX), fails the scan with exit `2`. An auto-detected
cache that cannot be read only prints `! cannot read stats from …; continuing
without sizes`. A source inside the model folder is named relative to it
(`.pbi/cache.abf`); `--verbose` gives full paths. Neither note is a skip notice, so `--strict` does not gate on them;
`--quiet` silences them. `--json` names the source in `summary.stats_source`.

### Model-only projects: `ripbi stub-report`

Desktop opens a `.pbip` only when it names a report, so a model-only project (a
central model whose thin reports live elsewhere) never gets a `cache.abf`.
`ripbi stub-report [PATH]` (issue #130) writes a stub that Desktop can open:

```text
ripbi stub-report models/Central.SemanticModel --wait
```

The stub is `Central.Report/` (a PBIR report with one empty page and no visuals,
marked with a `ripbi.stub` `report.json` annotation) plus `Central.pbip`. By default
both go into a fresh temp folder (`ripbi-stub-Central-…`), so nothing lands in the
repository. The report points back at the model with a relative `byPath`. PATH is a
`.SemanticModel` folder or its `definition/`; without it, the only `.SemanticModel` in
the current directory. No `.platform` is written; Desktop adds one on save.

In an interactive run on Windows, the `.pbip` then opens in Power BI Desktop through
its file association. It does not open under `--no-open`, `-q`, when stdin or stderr
is not a terminal, or when `CI` is set; the next steps say to open it by hand instead.
Refresh and save in Desktop: it writes `Central.SemanticModel/.pbi/cache.abf`, which
the auto-detection above reads. With `--wait`, ripbi keeps running until that file is
written after the stub was created (checked every second, done when its size stops
changing), then prints its size and the `ripbi scan` command to run. Ctrl-C stops
waiting.

| Flag | Meaning |
|---|---|
| `--out <DIR>` | Write the stub into DIR (created if missing) instead of a temp folder, e.g. `--out .`. DIR must be on the model's drive |
| `--name <NAME>` | File stem for the `.Report` folder and `.pbip`; defaults to the model's |
| `--no-open` | Do not open the `.pbip` in Desktop |
| `--wait` | Wait until Desktop saves `.pbi/cache.abf`, then print the next step |
| `-f`, `--force` | With `--out`: replace an existing ripbi stub and `.pbip`. A `.Report` that is not a ripbi stub is never replaced |
| `-q`, `--quiet` | Print nothing; never opens Desktop |
| `--no-color` | Never color output |

stdout is the absolute path of the `.pbip`, alone on one line. The next steps go to
stderr, plus a `.gitignore` tip under `--out`. Exit `0` when the stub is written; `2`
for a missing or ambiguous model, an invalid `--name`, an existing `--out` target
without `--force`, or a temp folder on a different drive from the model (the hint
suggests `--out`). A refusal writes nothing. A failed launch is only a note.

A `.vpax` (issue #108) is the VertiPaq Analyzer export DAX Studio, Tabular Editor,
and semantic-link-labs write from a live model. Only its `DaxModel.json` is read. Its
sizes are the engine's in-memory figures (`size_basis` `"engine"`), not file sizes,
so the same model reads somewhat differently than from an `.abf`: a column is its
dictionary, data, and attribute hierarchies; a table is its columns, user
hierarchies, and the relationships it is the many side of. The total line then reads `in memory` instead of `on disk`. An obfuscated `.vpax`
matches nothing. The staleness note compares the `.vpax` file's own date.
