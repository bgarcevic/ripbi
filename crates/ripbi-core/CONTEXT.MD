# ripbi-core — CONTEXT

Pure library crate: format ingestion, DAX lexing, and dependency-graph analysis.
No I/O to stdout/stderr, no `std::process::exit` — everything returns
`Result<T, ripbi_core::Error>`. `#![forbid(unsafe_code)]`.

## Responsibilities

- **Semantic model** (`src/model/`, `src/identity.rs`): the normalized `TabularDatabase`
  AST and the case-insensitive object identity every other layer refers to objects by.
  Pure data — no parsing, no I/O.
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
| Public API / error type | [src/lib.rs](src/lib.rs) | `Error`, `ReportModel`, and the crate-root re-exports |
| Semantic model AST | [docs/semantic-model.md](docs/semantic-model.md) | What is modeled and what is not; where DAX hides. Read before changing the AST |
| Name resolution & identity | [docs/name-resolution.md](docs/name-resolution.md) | Case-insensitivity, model-global measures, why `[Name]` resolves to *all* candidates |
| Format specs & detection | docs/formats.md (planned) | Create when ingestion lands |
| DAX token grammar | docs/dax-lexing.md (planned) | Create when lexer lands |
