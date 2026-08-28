# AGENTS.md

`ripbi` is a high-performance static analysis tool, linter, and tree-shaker for Microsoft
Power BI semantic models and DAX, written in Rust. Power BI models accumulate bloat —
unused measures, orphaned columns, dead Power Query partitions — that Microsoft's TOM/XMLA
tooling cannot detect because it is report-agnostic. `ripbi` ingests both model schemas and
report visual bindings, then isolates dead code via graph reachability.

**Pipeline:** ingest (`.pbix`/`.pbit`, `.pbip`, PBIR, TMDL/`model.bim`) → normalize into a
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
| Format ingestion (.pbix/.pbip/PBIR/TMDL) | [crates/ripbi-core/](crates/ripbi-core/) | [ripbi-core/CONTEXT.MD](crates/ripbi-core/CONTEXT.MD) | Normalize all formats to the same AST |
| DAX lexing / reference discovery | [crates/ripbi-core/src/](crates/ripbi-core/src/) | [ripbi-core/CONTEXT.MD](crates/ripbi-core/CONTEXT.MD) | Zero-copy `&str` slices |
| Dependency graph / reachability | [crates/ripbi-core/src/](crates/ripbi-core/src/) | [ripbi-core/CONTEXT.MD](crates/ripbi-core/CONTEXT.MD) | `petgraph` DAG, BFS from report roots |
| CLI flags, output, exit codes | [crates/ripbi-cli/](crates/ripbi-cli/) | [ripbi-cli/CONTEXT.MD](crates/ripbi-cli/CONTEXT.MD) | All printing lives here, never in core |
| CLI UX design decisions | [docs/](docs/) | [cli-ux-guidelines.md](docs/cli-ux-guidelines.md) | Condensed from clig.dev |
| Workspace / dependencies | [Cargo.toml](Cargo.toml) | [Cargo.toml](Cargo.toml) | Two-crate workspace |
| Project intro / positioning | [README.md](README.md) | [README.md](README.md) | User-facing overview |

## Coding standards

1. **Strict library separation** — `ripbi-core` never prints to stdout/stderr and never
   calls `std::process::exit`. It returns typed `Result<T, ripbi_core::Error>`.
2. **Unified semantic graph** — every source format normalizes into the same
   `TabularDatabase` and `ReportModel` AST before graph construction.
3. **Resilient JSON traversal** — Power BI schemas drift across versions; never panic on
   unknown or missing fields.
4. **Zero-copy efficiency** — prefer `&str` slices during DAX lexing and layout filtering.
5. **No unsafe code** — `#![forbid(unsafe_code)]` in every crate.
