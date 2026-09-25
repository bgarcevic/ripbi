# ripbi

[![CI](https://github.com/bgarcevic/ripbi/actions/workflows/ci.yml/badge.svg)](https://github.com/bgarcevic/ripbi/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/ripbi.svg)](https://crates.io/crates/ripbi)

Static analysis, linting, and tree-shaking for Power BI semantic models and DAX.

Docs: [bgarcevic.github.io/ripbi](https://bgarcevic.github.io/ripbi/).

I created ripbi because I liked Measure Killer, but I was looking for an
agent-friendly, free, fast tool to scan semantic models and the connected
reports to identify potentially unused semantic model objects. I tested it on a
shared semantic model with 14 connected reports, and it's 99% faster than
Measure Killer, reducing processing time from almost 4 minutes to a couple of
seconds.

It works without opening the reports or semantic models, so it can run as part
of a CI pipeline. It's also cross-platform, with no dependency on Power BI
Desktop or similar.

It reads local PBIP projects, TMSL `model.bim` files, PBIT templates, PBIX
files (the embedded model is decoded from its compressed `DataModel`; only
metadata is read, never the data), and standalone `.abf` backups. A thin PBIX
report pairs with a separate model. I plan to implement guided
automated cleanup and tenant scanning. An experimental
[desktop UI](desktop/README.md) provides guided model/report selection and usage metrics.

## Install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 | iex
```

Both scripts verify the download against the release's sha256 checksums and
install the binary into `~/.local/bin` as both `ripbi` and its short alias
`rib` — the two names are the same tool, so `rib scan` works anywhere
`ripbi scan` does. Or `cargo install ripbi`.

**Desktop (experimental, Windows x64):** starting with the next release,
the same [release page](https://github.com/bgarcevic/ripbi/releases) will also
include a `ripbi-desktop-<version>-x86_64-pc-windows-msvc-setup.exe` installer.
It shares the CLI version and checksums. Installers are unsigned and may trigger
Windows trust warnings. See [desktop setup and updates](desktop/README.md#releases-experimental).

## Quickstart

Clone the repository to get the sample projects, then scan one:

```sh
git clone https://github.com/bgarcevic/ripbi.git
cd ripbi
ripbi scan "samples/AdventureWorks Sales.pbip"
ripbi scan "samples/AdventureWorks Sales.pbit"
ripbi scan "samples/Revenue Opportunities.pbix"
ripbi scan --model model.bim --report report.pbix
```

Output from the committed AdventureWorks sample (trimmed):

```text
130 objects, 74 reachable from 51 roots, 56 unused

Measures (11)
  'Sales'[Average Sales per Order]
    ← nothing references it
Columns (28)
  'Customer'[City]
    ← only used by hierarchy 'Customer'[Geography] — hierarchy level (also unused)
  …
```

The summary line counts the model's objects, how many the reports reach, and
how many are unused. Each finding says why it is dead: nothing references it,
or its only consumer is itself unused. Broken report bindings surface too
(issue #60): a visual whose field no longer resolves in the model — the
renamed column, the broken measure — is reported with its page, visual, and
reason. `--broken` scopes a run to breakage alone so CI can gate on it
separately from unused findings.

`ripbi scan` discovers the project itself: pass a `.pbip` file, a project
folder, a `.SemanticModel`, or a `.Report`, or nothing to scan the current
directory. A `.Report` pairs with its model by stem sibling (`X.Report` beside
`X.SemanticModel`), sole model sibling, its `definition.pbir` path — or, for a
thin report bound by connection, the dataset name, when exactly one sibling
model's stem or display name carries it. A model-less `.pbip` project pairs
with the model its report names, the same way.

To scan one named model against every report bound to it, pass `--model`
(a `.SemanticModel` folder or the project's `.pbip`); each `--report` folder is
searched recursively for reports bound to that model:

```sh
ripbi scan --model "samples/AdventureWorks Sales.SemanticModel" --report samples/
```

The same search works without the flag when the path already names a semantic
model — `ripbi scan "samples/AdventureWorks Sales.SemanticModel" --report samples/`
walks plain `--report` folders exactly like `--model` mode. With `--report`
alone, the model is derived from the named reports' pairing and exactly those
reports are scanned:

```sh
ripbi scan --report "samples/AdventureWorks Sales.Report"
```

Exit codes:

| Code | Meaning |
|------|---------|
| `0`  | Nothing unused |
| `1`  | Unused objects found |
| `2`  | Error (bad path, ingestion failure, …) |

```sh
ripbi scan "samples/AdventureWorks Sales.pbip" -q   # no output; exit code only
```

For version pinning, script review, source builds, and updates, see the
[installation guide](https://bgarcevic.github.io/ripbi/installation.html).

## Seeing what ripbi sees

When a scan result looks wrong — "why is this measure live?" — `ripbi report`
prints the ingested report as data: pages → visuals → fields, with each
visual's type, the fields it binds (and through which well, sort, or
formatting rule), its filters, and any bindings that resolve to nothing in the
model:

```sh
ripbi report "samples/AdventureWorks Sales.pbip"
```

```text
AdventureWorks Sales — 1 page, 17 visuals
  Pages
    └── Overview (ReportSection) — 17 visuals
        ├── 11a03bbd46fd39147235 — donutChart
        │     Category   column 'Product'[Category]
        │     Y          measure 'Sales'[Cost] (inactive)
        │     sort       column 'Product'[Category]
        └── 3a1aeaede6fc79fe5066 — pivotTable
              Rows       hierarchy 'Product'[Products] level 'Category'
              …
```

`--json` (schema_version 1) and `--plain` (one tab-separated record per line)
carry the same inventory for scripts. The command is informational: it always
exits `0` on a produced inventory and `2` only on a real error, so it never
disturbs `scan`'s exit-code contract. The full contract is documented in the
user guide's [report chapter](https://bgarcevic.github.io/ripbi/report.html).

## Why is something alive? What goes with it?

`ripbi deps` explores one object from both sides: what it relies on
(Dependencies) and what relies on it (Impact) — model objects and report
bindings, each with its provenance. It is the deletion-planning view of the
same graph `scan` uses for findings: the `used_by` annotation shows one hop,
`deps --impact` shows the whole chain at once.

```sh
ripbi deps "'Sales'[Sales]" --model "samples/AdventureWorks Sales.SemanticModel"
ripbi deps "'Sales'[Sales]" --model "samples/AdventureWorks Sales.SemanticModel" --impact --depth 1
ripbi deps --model "samples/AdventureWorks Sales.SemanticModel" --table Sales
```

The command is read-only and informational: it exits `0` on any produced view
(empty impact included) and `2` only on errors. `--graph` draws the slice as a
topology diagram; `--plain` and `--json` never truncate. The full contract is
documented in the user guide's
[deps chapter](https://bgarcevic.github.io/ripbi/deps.html).

## Output

The full output contract (human, `--summary`, `--plain`, `--json`, and
`ripbi.toml` configuration) is documented in the user guide's
[scan chapter](https://bgarcevic.github.io/ripbi/output.html).

## Contributing

Dev setup, workflow, and CI checks are in [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
The `samples/` are Microsoft's own sample projects (MIT) and are not covered by
ripbi's license.
