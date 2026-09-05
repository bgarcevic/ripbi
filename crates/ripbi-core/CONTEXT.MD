# ripbi-core — CONTEXT

Pure library crate: format ingestion, DAX lexing, and dependency-graph analysis.
No I/O to stdout/stderr, no `std::process::exit` — everything returns
`Result<T, ripbi_core::Error>`. `#![forbid(unsafe_code)]`.

## Responsibilities

- **Semantic model** (`src/model.rs`, `src/identity.rs`): the normalized `TabularDatabase`
  AST and the case-insensitive object identity every other layer refers to objects by.
  Pure data — no parsing, no I/O.
- **Report** (`src/report.rs`): the normalized `ReportModel` AST — pages, visuals,
  filters, bookmarks, and report-level measures. Its `bindings()` enumeration is the
  reachability-root surface; pure data, populated by the ingestion formats.
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
- Module files sit *beside* their directory (`src/model.rs` + `src/model/`), not inside it
  as `mod.rs`. `ingest`, `dax`, and `graph` follow the same shape — with four modules a
  crate full of `mod.rs` tabs is unnavigable.

## Routing

| Task | Read | Notes |
|---|---|---|
| Semantic model AST | [docs/semantic-model.md](docs/semantic-model.md) | What is modeled and what is not; where DAX hides. Read before changing the AST |
| Report AST & root bindings | [docs/report-model.md](docs/report-model.md) | What binds model objects and why; where roots hide. Read before changing the report AST |
| Name resolution & identity | [docs/name-resolution.md](docs/name-resolution.md) | Case-insensitivity, model-global measures, why `[Name]` resolves to *all* candidates |
| Format specs & detection | [docs/formats.md](docs/formats.md) | What each format's parser accepts, the TMDL grammar subset, and the drift/skip policy. Read before changing any ingest module |
| DAX token grammar | docs/dax-lexing.md (planned) | Create when lexer lands |
