# Installation and quickstart

<!-- Mirrors the README's Install and Quickstart sections — keep them in sync. -->

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

<details>
<summary>Pinning a version, reviewing the scripts first, building from source</summary>

Pin a version:

```sh
curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh | RIPBI_VERSION=v0.2.1 sh
```

```powershell
$env:RIPBI_VERSION = 'v0.2.1'; irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 | iex
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

## Updating

Installed with one of the scripts above? `ripbi update` downloads the latest
release, verifies its sha256 checksum, and replaces `ripbi` and `rib` in place:

```sh
ripbi update           # update in place; exits 0 when done
ripbi update --check   # report only: exits 1 when a newer release exists
```

Exit codes: `0` updated or up to date, `1` update available (only from
`--check`), `2` error (network, checksum, unsupported platform).

Every command checks for a new release at most once a day and prints a dim
one-line notice when one is available. That check is a plain `GET` of the
public release metadata — no data is sent — and `RIPBI_NO_UPDATE_CHECK=1`
disables it. It is also skipped automatically when stderr is not a terminal
(pipes, CI logs), when `CI` is set, or under `-q`.

Cargo-installed copies are never self-replaced: `ripbi update` prints the
`cargo install ripbi --force` command instead. A source build found under a
`target/` directory prints the install-script and `cargo install --path`
alternatives.

On Windows a running executable cannot be deleted, so a completed update may
leave `ripbi.exe.old` (or `rib.exe.old`) beside the new binary. It is safe to
delete, and the next update removes it.

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

The [scan command](output.md) chapter documents every flag, the output
shapes, `ripbi.toml`, and the exit codes in full.
