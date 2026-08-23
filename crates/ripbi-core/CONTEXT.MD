# ripbi-core — CONTEXT

Pure library crate: format ingestion, DAX lexing, and dependency-graph analysis.
No I/O to stdout/stderr, no `std::process::exit` — everything returns
`Result<T, ripbi_core::Error>`. `#![forbid(unsafe_code)]`.

## Responsibilities

- **Ingestion** (`src/ingest/`): detect and parse the four source formats —
  legacy `.pbix`/`.pbit` (unzip in-memory, `DataModelSchema` + `Report/Layout`),
  `.pbip` folders (`.Report/` PBIR + `.SemanticModel/` TMDL), standalone TMDL
  folders, and TMSL `model.bim`. All normalize into `TabularDatabase` + `ReportModel`.
- **DAX lexing** (`src/dax/`): tokenize expressions to discover upstream/downstream
  references. Zero-copy `&str` where possible.
- **Graph** (`src/graph/`): build a DAG with `petgraph`, BFS reachability from report
  roots (visuals, filters, slicers) to isolate dead objects.

## Rules

- Handle Power BI schema drift gracefully — unknown JSON fields must never panic.
- Ingestion output must be format-agnostic: downstream code never branches on source format.

## Routing

| Task | Read | Notes |
|---|---|---|
| Public API / error type | [src/lib.rs](src/lib.rs) | `Error`, `ReportModel`, plus re-exports of the AST and index types |
| Tabular AST | [src/model/mod.rs](src/model/mod.rs) | `TabularDatabase` and friends; `dax_expressions` / `m_expressions` are the only enumeration of expression sites |
| Object identity / case folding | [src/identity.rs](src/identity.rs) | `NameKey`, `FieldRef`, `ObjectId`; `fold_name` is the one case-folding chokepoint. Shared with the future report AST |
| Name lookup index | [src/model/index.rs](src/model/index.rs) | `ModelIndex` + handles; unqualified `[Name]` returns *all* candidates (zero false positives) |
| Format specs & detection | docs/formats.md (planned) | Create when ingestion lands |
| DAX token grammar | docs/dax-lexing.md (planned) | Create when lexer lands |
