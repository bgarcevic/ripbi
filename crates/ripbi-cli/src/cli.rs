//! The clap argument definitions — the interface users type and scripts pin,
//! per `docs/cli-ux-guidelines.md`. Parsing only; behavior lives in
//! [`crate::scan`].

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Static analysis, linting, and tree-shaking for Power BI semantic models and DAX.
#[derive(Parser)]
#[command(name = "ripbi", version)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands.
#[derive(Subcommand)]
pub enum Command {
    /// Report the objects no report reaches: the dead measures, columns, and tables.
    #[command(after_help = EXAMPLES)]
    Scan(ScanArgs),
}

/// Trailing examples for both `-h` and `--help` (clap falls back).
const EXAMPLES: &str = "\
Examples:
  ripbi scan \"samples/AdventureWorks Sales.pbip\"
  ripbi scan models/Sales.SemanticModel --report reports/Sales.Report
  ripbi scan                       discover a project in the current directory
  ripbi scan --json > findings.json
  ripbi scan --summary             counts only, when the list would flood the terminal
  ripbi scan -q                    exit code only: 0 clean, 1 unused found, 2 error";

/// `ripbi scan` arguments.
#[derive(Args, Debug, Default)]
pub struct ScanArgs {
    /// Project to scan: a .pbip file, a project folder, a .SemanticModel, or a
    /// .Report. Defaults to `target` in ripbi.toml, then to discovery in the
    /// current directory.
    pub path: Option<PathBuf>,

    /// Extra report root to scan against; repeatable. Replaces `reports` from
    /// ripbi.toml.
    #[arg(long = "report", value_name = "PATH")]
    pub reports: Vec<PathBuf>,

    /// Write machine-readable JSON to stdout (schema:
    /// crates/ripbi-cli/docs/output.md).
    #[arg(long, conflicts_with = "plain")]
    pub json: bool,

    /// Write one record per line for grep/awk: `<type>\t<id>`.
    #[arg(long)]
    pub plain: bool,

    /// Print only the counts: the summary line and per-type totals, no
    /// findings list — for models whose finding list would flood the terminal.
    #[arg(short = 's', long, conflicts_with_all = ["json", "plain"])]
    pub summary: bool,

    /// Print nothing; the exit code is the only output.
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Treat any parser skip notice as an error (exit 2).
    #[arg(long)]
    pub strict: bool,

    /// Never color output, even on a TTY (also honors NO_COLOR, TERM=dumb).
    #[arg(long)]
    pub no_color: bool,

    /// Never prompt for a project; fail where a picker would appear.
    #[arg(long)]
    pub no_input: bool,
}
