# ripbi-core — CONTEXT

Pure library crate: format ingestion, DAX/M lexing, and dependency-graph analysis.
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
- **DAX lexing** (`src/dax/`): tokenize DAX expressions to extract object references
  (fields, bare table uses, function calls) and resolve them against the model as data.
  Zero-copy `&str` tokens; a conservative port of SQLBI Whiteboard's lexer.
- **M lexing** (`src/m/`): tokenize Power Query (M) expressions — table partitions and
  shared expressions — to extract the columns, tables, and shared expressions they
  name. A column named in M is that column's supply chain, never a consumer (unloading
  it cannot break refresh); tables and shared expressions named in M do keep alive,
  because deleting them deletes a query another expression reads (issue #39). Same
  zero-copy, total-lexer, conservatism-first design as the DAX side.
- **Graph** (`src/graph/`): build a DAG with `petgraph`, BFS reachability from report
  roots (visuals, filters, slicers) to isolate dead objects.

## Rules

- Handle Power BI schema drift gracefully — unknown JSON fields must never panic.
- Ingestion output must be format-agnostic: downstream code never branches on source format.
- Module files sit *beside* their directory (`src/model.rs` + `src/model/`), not inside it
  as `mod.rs`. `ingest`, `dax`, `m`, and `graph` follow the same shape — with five modules a
  crate full of `mod.rs` tabs is unnavigable.

## Routing

| Task | Read | Notes |
|---|---|---|
| Semantic model AST | [docs/semantic-model.md](docs/semantic-model.md) | What is modeled and what is not; where DAX hides. Read before changing the AST |
| Report AST & root bindings | [docs/report-model.md](docs/report-model.md) | What binds model objects and why; where roots hide. Read before changing the report AST |
| Name resolution & identity | [docs/name-resolution.md](docs/name-resolution.md) | Case-insensitivity, model-global measures, why `[Name]` resolves to *all* candidates |
| Format specs & detection | [docs/formats.md](docs/formats.md) | What each format's parser accepts, the TMDL grammar subset, the PBIR key policies, and the drift/skip policy. Read before changing any ingest module |
| DAX token grammar | [docs/dax-lexing.md](docs/dax-lexing.md) | Token grammar, extraction rules, the conservatism rule, and the SQLBI provenance. Read before changing the lexer |
| M token grammar | [docs/m-lexing.md](docs/m-lexing.md) | The M lexer/extraction/binding rules, the column-string function whitelist, and what is deliberately not modeled. Read before changing the `m` module |
| Graph, reachability & liveness policy | [docs/graph.md](docs/graph.md) | The edge catalog, the two-pass relationship rule, and what "unused" means. Read before changing the graph module |
