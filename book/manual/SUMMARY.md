# Summary

[Introduction](introduction.md)

# Getting started

- [Installation and quickstart](installation.md)
- [CI in five minutes](ci.md)

# Guides

- [Cleaning up a model](cleanup.md)
- [Gating only new findings](scan/compare.md)
- [Keeping objects on purpose](scan/keep.md)
- [Counting Excel and other consumers](scan/queries.md)
- [Measuring what unused objects cost](scan/storage.md)
- [Removing findings with tomix](scan/tx-rm.md)

# Concepts

- [What counts as unused](unused.md)
- [Supported inputs](inputs.md)

# Reference

- [ripbi scan](scan/overview.md)
  - [Flags](scan/flags.md)
  - [Human output](scan/human-output.md)
  - [--summary and --plain](scan/text-formats.md)
  - [JSON output](scan/json.md)
  - [SARIF, Markdown, Azure DevOps](scan/ci-formats.md)
  - [Validation and known boundaries](scan/boundaries.md)
- [ripbi deps](deps.md)
- [ripbi report](report.md)
- [ripbi fetch](fetch.md)
- [ripbi.toml](config.md)

# Internals

- [Graph and liveness rules](graph.md)
- [Source formats and drift policy](formats.md)
- [Semantic model AST](semantic-model.md)
- [Report AST](report-model.md)
- [DAX lexing](dax-lexing.md)
- [Power Query (M) lexing](m-lexing.md)
- [Name resolution](name-resolution.md)

---

- [Changelog](changelog.md)
