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
ripbi scan "samples/AdventureWorks Sales.pbip" -q   # no output; exit code only
```

The [scan command](output.md) chapter documents every flag, the output
shapes, `ripbi.toml`, and the exit codes in full.

## More installation options

<details>
<summary>Pin a version, review the scripts, or build from source</summary>

Pin a version:

```sh
curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh | RIPBI_VERSION=v0.4.1 sh
```

```powershell
$env:RIPBI_VERSION = 'v0.4.1'; irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 | iex
```

Download a script to review it before running:

```sh
curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh -o install.sh
sh install.sh
```

```powershell
irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 -OutFile install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

From a clone of this repository:

```sh
cargo install --path crates/ripbi-cli
```

</details>

## Updating

If you installed with one of the scripts above, `ripbi update` downloads the
latest release, verifies its sha256 checksum, and replaces `ripbi` and `rib`:

```sh
ripbi update           # update in place; exits 0 when done
ripbi update --check   # report only: exits 1 when a newer release exists
```

Exit codes: `0` updated or up to date, `1` update available (only from
`--check`), `2` error (network, checksum, unsupported platform).

The optional daily release check prints one notice when a new version is
available. Set `RIPBI_NO_UPDATE_CHECK=1` to disable it; it is already off in
CI, with redirected stderr, and under `-q`.

For Cargo installs, use `cargo install ripbi --force` instead. A source build
under `target/` also needs to be rebuilt or reinstalled. On Windows an update
may leave `ripbi.exe.old` or `rib.exe.old` beside the new binary; the next
update removes it.
