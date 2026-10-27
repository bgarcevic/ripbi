# AGENTS.md

`ripbi` is a high-performance static analysis tool, linter, and tree-shaker for Microsoft
Power BI semantic models and DAX, written in Rust. Power BI models accumulate bloat —
unused measures, orphaned columns, dead Power Query partitions — that Microsoft's TOM/XMLA
tooling cannot detect because it is report-agnostic. `ripbi` ingests both model schemas and
report visual bindings, then isolates dead code via graph reachability.

**Pipeline:** ingest (`.pbix`/`.pbit`, `.abf`, `.pbip`, PBIR, TMDL/`model.bim`) → normalize into a
unified `TabularDatabase` + `ReportModel` AST → tokenize DAX for references → build a DAG
with `petgraph` → BFS reachability from report roots → report unreachable objects.

Runs natively in terminals and headless CI/CD (GitHub Actions, Azure DevOps).

## Docs hierarchy

`AGENTS.md` (you are here) → per-crate `CONTEXT.MD` → specific process/skill docs.
Always route through the table below; each `CONTEXT.MD` routes further down.

## Routing

| Task | Go to | Read | Notes/skills |
|---|---|---|---|
| Semantic model AST / object identity | [crates/ripbi-core/](crates/ripbi-core/) | [ripbi-core/CONTEXT.MD](crates/ripbi-core/CONTEXT.MD) | Pure data; identity shared with the report AST |
| Format ingestion (.pbix/.abf/.pbip/PBIR/TMDL) | [crates/ripbi-core/](crates/ripbi-core/) | [ripbi-core/CONTEXT.MD](crates/ripbi-core/CONTEXT.MD) | Normalize all formats to the same AST |
| XPress9 decoding (ABF container) | [crates/ripbi-xpress9/](crates/ripbi-xpress9/) | [ripbi-xpress9/src/lib.rs](crates/ripbi-xpress9/src/lib.rs) | Vendored Microsoft C codec; the only crate with `unsafe` (FFI) |
| DAX lexing / reference discovery | [crates/ripbi-core/src/](crates/ripbi-core/src/) | [ripbi-core/CONTEXT.MD](crates/ripbi-core/CONTEXT.MD) | Zero-copy `&str` slices |
| Power Query (M) lexing / column & table references | [crates/ripbi-core/src/](crates/ripbi-core/src/) | [ripbi-core/CONTEXT.MD](crates/ripbi-core/CONTEXT.MD) | Same lexer shape as DAX; M names columns (supply chain, not consumers) and keeps merge-source tables/expressions alive (issue #39) |
| Dependency graph / reachability | [crates/ripbi-core/src/](crates/ripbi-core/src/) | [ripbi-core/CONTEXT.MD](crates/ripbi-core/CONTEXT.MD) | `petgraph` DAG, BFS from report roots |
| Front-end neutral app layer (scan/report/deps pipelines, discovery, config, typed results, the scan verdict, JSON/SARIF/Markdown/tx-rm writers) | [crates/ripbi-app/](crates/ripbi-app/) | [ripbi-app/CONTEXT.MD](crates/ripbi-app/CONTEXT.MD) | Never prints; front ends observe via `scan::Observer`. Start here for any new UI (GUI, web) |
| CLI flags, output, exit codes | [crates/ripbi-cli/](crates/ripbi-cli/) | [ripbi-cli/CONTEXT.MD](crates/ripbi-cli/CONTEXT.MD) | All printing lives here, never in core |
| Tenant access (`fetch`: sign-in, downstream reports, definitions) | [crates/ripbi-service/](crates/ripbi-service/) | [ripbi-service/CONTEXT.MD](crates/ripbi-service/CONTEXT.MD) | The only crate on the network; `ripbi-core` stays offline. Contract: [fetch.md](crates/ripbi-cli/docs/fetch.md) |
| CLI UX design decisions | [docs/](docs/) | [cli-ux-guidelines.md](docs/cli-ux-guidelines.md) | Condensed from clig.dev |
| Workspace / dependencies | [Cargo.toml](Cargo.toml) | [Cargo.toml](Cargo.toml) | Five-crate workspace |
| GitHub Action (`bgarcevic/ripbi-action`) | [ci/github-action/](ci/github-action/) | [ci/github-action/README.md](ci/github-action/README.md) | Composite action mirrored to its own repo on release; tested by `.github/workflows/action.yml` |
| Azure Pipelines template | [templates/](templates/) | [ripbi-scan.yml](templates/ripbi-scan.yml) header | Steps template; tested under `scripts/run_ado_template.py` by `.github/workflows/action.yml`, so keep to the expressions that emulator knows |
| Desktop UI (egui, experimental) | [desktop-egui/](desktop-egui/) | [desktop-egui/README.md](desktop-egui/README.md) | Standalone pure-Rust package; calls `ripbi-app` directly |
| Project intro / positioning | [README.md](README.md) | [README.md](README.md) | User-facing overview |
| Docs site (mdBook on Pages) | [book/](book/) | [CONTRIBUTING.md](CONTRIBUTING.md) | `book/src/` is generated — edit the canonical docs, never the projection |

## Coding standards

1. **Strict library separation** — `ripbi-core` and `ripbi-app` never print to stdout/stderr or
   call `std::process::exit`. They return typed `Result<T, ripbi_core::Error>` / `ripbi_app::ScanError`.
2. **Unified semantic graph** — every source format normalizes into the same
   `TabularDatabase` and `ReportModel` AST before graph construction.
3. **Resilient JSON traversal** — Power BI schemas drift across versions; never panic on
   unknown or missing fields.
4. **Zero-copy efficiency** — prefer `&str` slices during DAX lexing and layout filtering.
5. **No unsafe code** — `#![forbid(unsafe_code)]` in every crate, with one approved
   exception: `ripbi-xpress9` confines the FFI to Microsoft's XPress9 C decoder. Every
   `unsafe` block there carries a `SAFETY` note; nothing else may add `unsafe`.
