# Introduction

ripbi is static analysis, linting, and tree-shaking for Power BI semantic
models and DAX. Power BI models accumulate bloat — unused measures, orphaned
columns, dead Power Query partitions — that Microsoft's TOM/XMLA tooling
cannot detect because it is report-agnostic. ripbi ingests both model schemas
and report visual bindings, then isolates dead code via graph reachability.

It works without opening the reports or the semantic model, so it runs in a
terminal or headless CI/CD (GitHub Actions, Azure DevOps). It is
cross-platform, with no dependency on Power BI Desktop.

Today it works on local PBIP projects — a TMDL semantic model plus PBIR
reports; `.pbix` and `.pbit` files are not supported yet.

> This book is built from
> [`main`](https://github.com/bgarcevic/ripbi/commits/main) and may document
> changes that have not shipped in a release yet.

Start with [installation and quickstart](installation.md), then
[the scan command](output.md) for everything `ripbi scan` can do — flags,
output shapes, `ripbi.toml`, and exit codes. [What counts as
unused](graph.md) explains the liveness rules behind the findings.
