# ripbi

[![CI](https://github.com/bgarcevic/ripbi/actions/workflows/ci.yml/badge.svg)](https://github.com/bgarcevic/ripbi/actions/workflows/ci.yml)
[![Release](https://github.com/bgarcevic/ripbi/actions/workflows/release.yml/badge.svg)](https://github.com/bgarcevic/ripbi/actions/workflows/release.yml)
[![crates.io](https://img.shields.io/crates/v/ripbi.svg)](https://crates.io/crates/ripbi)

Static analysis, linting, and tree-shaking for Power BI semantic models and DAX.

ripbi ingests TMDL semantic models and PBIR reports, tokenizes every DAX
expression, and walks the dependency graph from report bindings to find what no
report reaches: the dead measures, orphaned columns, and unreachable tables that
TOM/XMLA tooling cannot see because it is report-agnostic.

## Install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 | iex
```

Both scripts resolve the latest release, verify the archive's sha256 against
the published `sha256sums.txt`, and install into `~/.local/bin`. Pin a version
with `RIPBI_VERSION`:

```sh
curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh | RIPBI_VERSION=v0.1.0 sh
```

```powershell
$env:RIPBI_VERSION = 'v0.1.0'; irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 | iex
```

Prefer to read a script before you run it? Both are plain shell and PowerShell —
download them, review, then run from the file:

```sh
curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh -o install.sh
less install.sh
sh install.sh
```

```powershell
irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 -OutFile install.ps1
Get-Content .\install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1  # the flag is only needed if your execution policy blocks script files
```

With Cargo:

```sh
cargo install ripbi
```

Or build from a clone of this repository:

```sh
cargo install --path crates/ripbi-cli
```

## Quickstart

Clone the repository to get the sample projects, then scan one:

```sh
git clone https://github.com/bgarcevic/ripbi.git
cd ripbi
ripbi scan "samples/AdventureWorks Sales.pbip"
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

The summary line counts every object in the model, how many the reports reach,
and how many are unused. Findings are grouped by object type, and each carries
a chain annotation: either nothing references it, or the only thing that does
is itself unused — a dead chain you can delete whole.

`ripbi scan` discovers the project itself: pass a `.pbip` file, a project
folder, a `.SemanticModel`, or a `.Report`, or nothing at all to scan the
current directory.

Exit codes make it CI-ready:

| Code | Meaning |
|------|---------|
| `0`  | Nothing unused |
| `1`  | Unused objects found |
| `2`  | Error (bad path, ingestion failure, …) |

```sh
ripbi scan -q   # output suppressed; the exit code is the report
```

## Output

The full output contract — human, `--summary`, `--plain`, `--json`, and
`ripbi.toml` configuration — is documented in
[crates/ripbi-cli/docs/output.md](https://github.com/bgarcevic/ripbi/blob/main/crates/ripbi-cli/docs/output.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
The `samples/` are Microsoft's own sample projects (MIT) and are not covered by
ripbi's license.
