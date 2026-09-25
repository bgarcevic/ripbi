# ripbi desktop

A local Tauri 2 interface for ripbi: select a semantic model, add reports or
recursive search folders, then explore model usage and cleanup candidates.

## Run

Install Node.js 24.15+ (or 22.22.2+) and stable Rust, plus the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/)
(on Windows: C++ Build Tools and WebView2).

From this directory:

```sh
npm ci
npm run tauri dev
```

`npm run dev` opens only the web preview; native file selection and scanning
require the Tauri app. On Windows, `npm run tauri -- build -- --locked` produces
a release executable and NSIS installer. Use `--no-bundle` for an executable
without an installer when developing on other platforms.

## Releases (experimental)

Desktop shares the CLI's version and `v*` release tags. Tagged releases build
a Windows x64 MSVC installer alongside the CLI archives and include both in
`sha256sums.txt`. Download the `ripbi-desktop-<version>-x86_64-pc-windows-msvc-setup.exe`
asset from the [release page](https://github.com/bgarcevic/ripbi/releases).

The installer runs for the current user and downloads WebView2 if it is missing.
That first installation needs internet access. Installers are currently unsigned,
so Windows may display a trust warning. Install newer releases manually;
`ripbi update` updates the CLI only. Automatic desktop updates and code signing
are not configured yet.

The reusable `.github/workflows/desktop.yml` runs frontend tests, Rust checks,
and an NSIS packaging build on Windows for every PR. PR artifacts are debug
builds for testing; release-tag artifacts are optimized release builds.

Version metadata is synchronized from the root `Cargo.toml` workspace version.
See [release preparation](../CONTRIBUTING.md#release-preparation) before tagging.

## Workflow and metrics

1. Select a `.SemanticModel` / TMDL folder or a `.pbip` project.
2. Add one or more `.pbip` projects, `.Report` folders, or search folders.
   Repeat the folder picker to add multiple folders. Discovery recursively
   matches reports to the model using ripbi's existing pairing rules.
3. Review graph object counts, reachable percentage, all unused objects,
   connected reports, cleanup candidates by kind, broken references, and
   auto date/time verdicts. Search unused objects and expand their dependencies.
   Coverage & notices includes matched report paths and the full discovery log.
   Export JSON uses a native save dialog and includes results and diagnostics.

Reachability is **not** a storage or performance estimate. The unused total
includes hidden date machinery; the candidate list excludes grouped machinery
and ignored findings. Other consumers outside the selected reports can still
use these objects. Parser notices and ignored findings remain visible.

The desktop adapter calls `ripbi_cli::scan::run_in` in a background worker,
captures its schema-v1 JSON and diagnostics, and preserves discovery,
conservative liveness, skip, and breakage semantics. Project `ripbi.toml` is
loaded from the model directory upward, including ignore patterns. Explicit
report selections override configured report sources. No subprocess or separate
CLI installation is needed; model files are never modified. PBIX/PBIT and
`model.bim` are not currently supported by CLI discovery or this UI.

## Development

The desktop Rust package is excluded from the root Cargo workspace, keeping
native webview dependencies out of CLI-only builds and CI. It uses the existing
CLI crate as a path dependency. Its Cargo lockfile is tracked separately.

```sh
npm run build
npm test
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

Frontend tests cover ordered selection, cancellation, duplicate sources, errors,
result metrics, filtering, escaping, and export. The Rust adapter test runs the
actual discovery and analysis pipeline against the AdventureWorks sample.
