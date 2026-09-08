# ripbi

[![CI](https://github.com/bgarcevic/ripbi/actions/workflows/ci.yml/badge.svg)](https://github.com/bgarcevic/ripbi/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/ripbi.svg)](https://crates.io/crates/ripbi)

Static analysis, linting, and tree-shaking for Power BI semantic models and DAX.

ripbi finds the measures, columns, and tables a model can live without. It reads
a PBIP project (TMDL semantic model plus PBIR reports), checks what the reports
actually use, and lists everything else. Supported today: TMDL and PBIR.
`.pbix` and `.pbit` files are not supported yet.

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
install the binary into `~/.local/bin`. Or `cargo install ripbi`.

<details>
<summary>Pinning a version, reviewing the scripts first, building from source</summary>

Pin a version:

```sh
curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh | RIPBI_VERSION=v0.1.0 sh
```

```powershell
$env:RIPBI_VERSION = 'v0.1.0'; irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 | iex
```

Both scripts are plain shell and PowerShell. Download them first if you would
rather read before running:

```sh
curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh -o install.sh
sh install.sh
```

```powershell
irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 -OutFile install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1  # the flag is only needed if your policy blocks script files
```

From a clone of this repository:

```sh
cargo install --path crates/ripbi-cli
```

</details>

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

The summary line counts the model's objects, how many the reports reach, and
how many are unused. Each finding says why it is dead: nothing references it,
or its only consumer is itself unused.

`ripbi scan` discovers the project itself: pass a `.pbip` file, a project
folder, a `.SemanticModel`, or a `.Report`, or nothing to scan the current
directory.

Exit codes:

| Code | Meaning |
|------|---------|
| `0`  | Nothing unused |
| `1`  | Unused objects found |
| `2`  | Error (bad path, ingestion failure, …) |

```sh
ripbi scan -q   # no output; exit code only
```

## Output

The full output contract (human, `--summary`, `--plain`, `--json`, and
`ripbi.toml` configuration) is documented in
[crates/ripbi-cli/docs/output.md](https://github.com/bgarcevic/ripbi/blob/main/crates/ripbi-cli/docs/output.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
The `samples/` are Microsoft's own sample projects (MIT) and are not covered by
ripbi's license.
