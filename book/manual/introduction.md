# Introduction

ripbi is static analysis, linting, and tree-shaking for Power BI semantic
models and DAX. Power BI models accumulate bloat — unused measures, orphaned
columns, dead Power Query partitions — that Microsoft's TOM/XMLA tooling
cannot detect because it is report-agnostic. ripbi ingests both model schemas
and report visual bindings, then isolates dead code via graph reachability.

It works without opening the reports or the semantic model, so it runs in a
terminal or headless CI/CD (GitHub Actions, Azure DevOps). It is
cross-platform, with no dependency on Power BI Desktop.

Today it works on local PBIP projects (a TMDL semantic model plus PBIR
reports), TMSL `model.bim` files, `.pbit` templates, `.pbix` files (including
the embedded model, whose metadata is decoded from the compressed `DataModel`),
and standalone `.abf` backups.

![ripbi scan --summary on the AdventureWorks sample: 130 objects, 74 reachable, 56 unused, with per-type counts and the worst tables](media/scan.svg)

> This book is built from
> [`main`](https://github.com/bgarcevic/ripbi/commits/main) and may document
> changes that have not shipped in a release yet.

Start with [installation and quickstart](installation.md). The rest of the
book is organized by what you want to do:

- **Guides** walk through tasks: [cleaning up a model](cleanup.md), gating pull
  requests on [only new findings](scan/compare.md), and
  [keeping objects on purpose](scan/keep.md).
- **Concepts** explain [what counts as unused](unused.md) and which
  [inputs](inputs.md) ripbi reads. Read the first before you remove anything.
- **Reference** documents each command in full:
  [`scan`](scan/overview.md) finds candidates for review,
  [`deps`](deps.md) follows an object's dependencies and impact,
  [`report`](report.md) shows what ripbi read from a report, and
  [`ripbi.toml`](config.md) holds project settings.
- **Internals** specify the graph, the parsers, and the lexers, for people
  auditing results or changing ripbi.
