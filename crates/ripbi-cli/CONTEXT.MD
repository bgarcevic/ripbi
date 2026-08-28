# ripbi-cli — CONTEXT

The `ripbi` binary. Thin orchestration layer over `ripbi-core`:
argument parsing (clap), source-format auto-detection dispatch, human-readable and
CI-friendly output, and exit codes.

## Rules

- **No analysis logic here.** All ingestion, lexing, and graph work lives in
  `ripbi-core`; this crate only calls it and presents results.
- This is the only crate allowed to print to stdout/stderr or set exit codes.
- Exit codes: `0` clean, non-zero when dead objects are found (CI gating) or on error.
- `#![forbid(unsafe_code)]`.

## Routing

| Task | Read | Notes |
|---|---|---|
| UX conventions (help, output, flags, errors) | [../../docs/cli-ux-guidelines.md](../../docs/cli-ux-guidelines.md) | Follow for every CLI change |
| Output formats (text/JSON) | docs/output.md (planned) | Create when reporting lands |
