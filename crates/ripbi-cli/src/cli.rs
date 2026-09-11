//! The clap argument definitions — the interface users type and scripts pin,
//! per `docs/cli-ux-guidelines.md`. Parsing only; behavior lives in
//! [`crate::scan`].

use std::collections::HashSet;
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

    /// Update ripbi to the latest GitHub release.
    #[command(after_help = UPDATE_EXAMPLES)]
    Update(UpdateArgs),

    /// Internal: refresh the cached update state. Not for direct use.
    #[command(name = "__update-check", hide = true)]
    __UpdateCheck(UpdateCheckArgs),
}

/// Trailing examples for both `-h` and `--help` (clap falls back).
const EXAMPLES: &str = "\
Examples:
  ripbi scan \"samples/AdventureWorks Sales.pbip\"
  ripbi scan models/Sales.SemanticModel --report reports/Sales.Report
  ripbi scan --model models/Sales.SemanticModel --report reports/
  ripbi scan                       discover a project in the current directory
  ripbi scan --json > findings.json
  ripbi scan --summary             counts only, when the list would flood the terminal
  ripbi scan --measures --columns  only these unused object types
  ripbi scan -q                    exit code only: 0 clean, 1 unused found, 2 error";

/// Trailing examples for both `-h` and `--help` (clap falls back).
const UPDATE_EXAMPLES: &str = "\
Examples:
  ripbi update                  update to the latest release in place
  ripbi update --check          report only: exit 1 when a newer release exists
  ripbi update --check -q       exit code only: 1 = update available, 0 = current
  RIPBI_NO_UPDATE_CHECK=1 ripbi scan …   silence the daily update notice";

/// `ripbi update` arguments.
#[derive(Args, Debug, Default)]
pub struct UpdateArgs {
    /// Report the latest release and whether it is newer; download and install
    /// nothing. Exit code 1 means a newer release exists.
    #[arg(long)]
    pub check: bool,

    /// Print nothing; the exit code is the only output.
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Never color output, even on a TTY (also honors NO_COLOR, TERM=dumb).
    #[arg(long)]
    pub no_color: bool,
}

/// `ripbi __update-check` arguments: none. The hidden child only refreshes the
/// cached update state for the ambient daily notification.
#[derive(Args, Debug, Default)]
pub struct UpdateCheckArgs {}

/// `ripbi scan` arguments.
#[derive(Args, Debug, Default)]
pub struct ScanArgs {
    /// Project to scan: a .pbip file, a project folder, a .SemanticModel, or a
    /// .Report. Defaults to `target` in ripbi.toml, then to discovery in the
    /// current directory.
    pub path: Option<PathBuf>,

    /// The semantic model to analyze: a .SemanticModel folder, its definition/,
    /// or any folder containing model.tmdl. Disables cwd discovery and the
    /// ripbi.toml `target`; --report values that are plain folders become search
    /// folders for reports bound to this model (default: the model's parent).
    #[arg(long, value_name = "PATH", conflicts_with = "path")]
    pub model: Option<PathBuf>,

    /// Extra report root to scan against; repeatable. Replaces `reports` from
    /// ripbi.toml. With --model, a plain folder is searched recursively for
    /// report items bound to the model.
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

    /// Only report unused measures. Combine with the other type flags to
    /// select several; with none of them, everything is reported.
    #[arg(long)]
    pub measures: bool,

    /// Only report unused columns. Combine with the other type flags to
    /// select several; with none of them, everything is reported.
    #[arg(long)]
    pub columns: bool,

    /// Only report unused hierarchies. Combine with the other type flags to
    /// select several; with none of them, everything is reported.
    #[arg(long)]
    pub hierarchies: bool,

    /// Only report unused tables. Also keeps the Auto date/time section,
    /// which other type flags hide.
    #[arg(long)]
    pub tables: bool,

    /// Only report unused partitions. Combine with the other type flags to
    /// select several; with none of them, everything is reported.
    #[arg(long)]
    pub partitions: bool,

    /// Only report unused relationships. Combine with the other type flags
    /// to select several; with none of them, everything is reported.
    #[arg(long)]
    pub relationships: bool,

    /// Only report unused calculation items. Combine with the other type
    /// flags to select several; with none of them, everything is reported.
    #[arg(long)]
    pub calc_items: bool,

    /// Only report unused expressions. Combine with the other type flags to
    /// select several; with none of them, everything is reported.
    #[arg(long)]
    pub expressions: bool,

    /// Only report unused functions. Combine with the other type flags to
    /// select several; with none of them, everything is reported.
    #[arg(long)]
    pub functions: bool,

    /// Only report unused report measures. Combine with the other type flags
    /// to select several; with none of them, everything is reported.
    #[arg(long)]
    pub report_measures: bool,

    /// Also print the "Power Query also names it" annotation on unused Data
    /// columns (human output). `--json` always carries the field.
    #[arg(long)]
    pub power_query: bool,
}

impl ScanArgs {
    /// The object kinds the type flags select, or `None` when no type flag
    /// was passed — the no-filter path keeps today's output exactly. The
    /// exit code, every output mode, and the `[scan].ignore` note all
    /// describe only what this filter lets through.
    #[must_use]
    pub fn selected_kinds(&self) -> Option<HashSet<&'static str>> {
        let picks = [
            (self.measures, "measure"),
            (self.columns, "column"),
            (self.hierarchies, "hierarchy"),
            (self.tables, "table"),
            (self.partitions, "partition"),
            (self.relationships, "relationship"),
            (self.calc_items, "calculation_item"),
            (self.expressions, "expression"),
            (self.functions, "function"),
            (self.report_measures, "report_measure"),
        ];
        let selected: HashSet<&'static str> = picks
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, kind)| kind)
            .collect();
        (!selected.is_empty()).then_some(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::GROUPS;
    use clap::{CommandFactory, Parser};

    /// The update flags parse, and the hidden notifier child parses too.
    #[test]
    fn update_flags_parse() {
        let cli = Cli::try_parse_from(["ripbi", "update", "--check", "-q", "--no-color"])
            .expect("update flags parse");
        let Command::Update(args) = cli.command else {
            panic!("expected the update subcommand");
        };
        assert!(args.check && args.quiet && args.no_color);

        let cli = Cli::try_parse_from(["ripbi", "__update-check"]).expect("hidden child parses");
        assert!(matches!(cli.command, Command::__UpdateCheck(_)));
    }

    /// The type flags are sugar over the render groups: one flag per group,
    /// selecting exactly its kind, in the groups' order. This is the
    /// lockstep guard — add the flag and the group together or it fails.
    #[test]
    fn type_flags_stay_in_lockstep_with_the_render_groups() {
        let one_flag = [
            (
                ScanArgs {
                    measures: true,
                    ..Default::default()
                },
                "measures",
            ),
            (
                ScanArgs {
                    columns: true,
                    ..Default::default()
                },
                "columns",
            ),
            (
                ScanArgs {
                    hierarchies: true,
                    ..Default::default()
                },
                "hierarchies",
            ),
            (
                ScanArgs {
                    tables: true,
                    ..Default::default()
                },
                "tables",
            ),
            (
                ScanArgs {
                    partitions: true,
                    ..Default::default()
                },
                "partitions",
            ),
            (
                ScanArgs {
                    relationships: true,
                    ..Default::default()
                },
                "relationships",
            ),
            (
                ScanArgs {
                    calc_items: true,
                    ..Default::default()
                },
                "calc-items",
            ),
            (
                ScanArgs {
                    expressions: true,
                    ..Default::default()
                },
                "expressions",
            ),
            (
                ScanArgs {
                    functions: true,
                    ..Default::default()
                },
                "functions",
            ),
            (
                ScanArgs {
                    report_measures: true,
                    ..Default::default()
                },
                "report-measures",
            ),
        ];
        assert_eq!(
            one_flag.len(),
            GROUPS.len(),
            "one type flag per render group"
        );
        for ((args, flag), (kind, _)) in one_flag.iter().zip(GROUPS.iter()) {
            let selected = args
                .selected_kinds()
                .unwrap_or_else(|| panic!("--{flag} must select its kind"));
            assert!(
                selected.len() == 1 && selected.contains(kind),
                "--{flag} must select exactly the {kind:?} group, got {selected:?}"
            );
        }

        assert!(
            ScanArgs::default().selected_kinds().is_none(),
            "no type flags, no filter"
        );

        let command = Cli::command();
        let scan = command.find_subcommand("scan").expect("scan subcommand");
        for (_args, flag) in &one_flag {
            assert!(
                scan.get_arguments()
                    .any(|argument| argument.get_long() == Some(*flag)),
                "scan is missing the --{flag} argument"
            );
        }
    }
}
