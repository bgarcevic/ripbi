# ripbi
Static analysis, dependency graph visualization, and model pruning for Power BI PBIX and Fabric TMDL.

## Usage

```sh
# Scan a PBIP project: reports every object no report reaches.
ripbi scan "samples/AdventureWorks Sales.pbip"

# Scan a semantic model against one or more reports.
ripbi scan models/Sales.SemanticModel --report reports/Sales.Report

# CI: no output, exit code only (0 clean, 1 unused found, 2 error).
ripbi scan --quiet
```

See `ripbi scan --help` and [crates/ripbi-cli/docs/output.md](crates/ripbi-cli/docs/output.md)
for the full output contract (human, `--summary`, `--plain`, `--json`) and `ripbi.toml` support.
