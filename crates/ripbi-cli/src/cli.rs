//! The clap argument definitions — the interface users type and scripts pin,
//! per `docs/cli-ux-guidelines.md`. Parsing only; behavior lives in
//! [`crate::scan`], [`crate::deps`], and [`crate::report`].

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

    /// Print what ripbi sees in a report: pages → visuals → fields.
    #[command(after_help = REPORT_EXAMPLES)]
    Report(ReportArgs),

    /// Show what an object depends on, and what would be affected if it changed.
    #[command(after_help = DEPS_EXAMPLES)]
    Deps(DepsArgs),

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
  ripbi scan                       # discover a project in the current directory
  ripbi scan --json > findings.json
  ripbi scan --summary             # counts only, when the list would flood the terminal
  ripbi scan --measures --columns  # only these unused object types
  ripbi scan -q                    # exit code only: 0 clean, 1 unused found, 2 error

Type flags union; with none of them, everything is reported.";

/// Trailing examples for both `-h` and `--help` (clap falls back).
const UPDATE_EXAMPLES: &str = "\
Examples:
  ripbi update             # update to the latest release in place
  ripbi update --check     # report only: exit 1 when a newer release exists
  ripbi update --check -q  # exit code only: 1 = update available, 0 = current
  RIPBI_NO_UPDATE_CHECK=1 ripbi scan …  # silence the daily update notice";

/// Trailing examples for both `-h` and `--help` (clap falls back).
const REPORT_EXAMPLES: &str = "\
Examples:
  ripbi report \"samples/AdventureWorks Sales.pbip\"
  ripbi report reports/Sales.Report  # any report item pairs with its model
  ripbi report                       # discover a project here
  ripbi report --json > inventory.json
  ripbi report --plain | cut -f1     # record types: report page visual …
  ripbi report --visuals             # one roll-up row per visual
  ripbi report --used                # every model object the report keeps alive
  ripbi report --fields --match \"'Sales'[Total]\"
  ripbi report -q                    # exit code only: 0 read, 2 error";

/// Trailing examples for both `-h` and `--help` (clap falls back).
const DEPS_EXAMPLES: &str = "\
Examples:
  # Explore both dependencies and impact
  ripbi deps \"'Sales'[Total Sales]\"

  # What does this measure rely on?
  ripbi deps \"'Sales'[Total Sales]\" --dependencies

  # What could be affected if this measure changes?
  ripbi deps \"'Sales'[Total Sales]\" --impact

  # Direct impact only
  ripbi deps \"'Sales'[Total Sales]\" --impact --depth 1

  # Explore a table
  ripbi deps --table Sales

  # Impact within one report
  ripbi deps \"'Sales'[Total Sales]\" --impact --in-report Executive

  # Machine-readable output
  ripbi deps \"'Sales'[Total Sales]\" --plain
  ripbi deps \"'Sales'[Total Sales]\" --json";

/// `ripbi deps` arguments.
#[derive(Args, Debug, Default)]
pub struct DepsArgs {
    /// The model object to explore: 'Table'[Name], [Name], or a bare table name.
    pub object: Option<String>,

    /// Show what the object relies on, upstream.
    #[arg(long)]
    pub dependencies: bool,

    /// Show what relies on the object, downstream.
    #[arg(long)]
    pub impact: bool,

    /// Traverse at most N edges from the object; unset or 'all' traverses to
    /// the leaves.
    #[arg(long, value_name = "N|all")]
    pub depth: Option<String>,

    /// Explore every member of one table, instead of a single object.
    #[arg(long, value_name = "NAME", conflicts_with = "object")]
    pub table: Option<String>,

    /// Explore every object of one type, instead of a single object.
    #[arg(long = "type", value_name = "TYPE", conflicts_with = "object")]
    pub types: Vec<String>,

    /// A .SemanticModel folder to explore (or its definition/, or a project).
    #[arg(long, value_name = "PATH")]
    pub model: Option<PathBuf>,

    /// Only show downstream usages by this kind of consumer; 'visual' means
    /// report bindings.
    #[arg(long, value_name = "TYPE")]
    pub consumer: Option<String>,

    /// Only show report bindings belonging to this report.
    #[arg(long, value_name = "NAME")]
    pub in_report: Option<String>,

    /// Only show report bindings on this page.
    #[arg(long, value_name = "NAME")]
    pub on_page: Option<String>,

    /// A report to include as input; repeatable.
    #[arg(long = "report", value_name = "PATH")]
    pub reports: Vec<PathBuf>,

    /// Machine-readable JSON (schema: docs/deps.md).
    #[arg(long, conflicts_with = "plain")]
    pub json: bool,

    /// One typed record per line, for grep/awk.
    #[arg(long)]
    pub plain: bool,

    /// Print nothing; the exit code is the only output.
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// List every pairing note and skipped item on stderr.
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Never color output (also honors NO_COLOR, TERM=dumb).
    #[arg(long)]
    pub no_color: bool,

    /// Never prompt; fail where a picker would appear.
    #[arg(long)]
    pub no_input: bool,
}

/// `ripbi update` arguments.
#[derive(Args, Debug, Default)]
pub struct UpdateArgs {
    /// Report only: exit 1 when a newer release exists.
    #[arg(long)]
    pub check: bool,

    /// Print nothing; the exit code is the only output.
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Never color output (also honors NO_COLOR, TERM=dumb).
    #[arg(long)]
    pub no_color: bool,
}

/// `ripbi __update-check` arguments: none. The hidden child only refreshes the
/// cached update state for the ambient daily notification.
#[derive(Args, Debug, Default)]
pub struct UpdateCheckArgs {}

/// `ripbi report` arguments.
#[derive(Args, Debug, Default)]
pub struct ReportArgs {
    /// A .pbip file, project folder, .SemanticModel, or .Report.
    pub path: Option<PathBuf>,

    /// Machine-readable JSON (schema: docs/report.md).
    #[arg(long, conflicts_with = "plain")]
    pub json: bool,

    /// One typed record per line, for grep/awk.
    #[arg(long)]
    pub plain: bool,

    /// List pages (all pages, or those --page selects).
    #[arg(long, conflicts_with_all = ["json", "plain"])]
    pub pages: bool,

    /// List visuals: page, type, field and unresolved counts.
    #[arg(long, conflicts_with_all = ["json", "plain"])]
    pub visuals: bool,

    /// List every binding: one row per field reference.
    #[arg(long, conflicts_with_all = ["json", "plain"])]
    pub fields: bool,

    /// List every model object the report keeps alive.
    #[arg(long, conflicts_with_all = ["json", "plain"])]
    pub used: bool,

    /// Only pages matching a name or display-name glob.
    #[arg(long, value_name = "GLOB")]
    pub page: Vec<String>,

    /// Only visuals matching a name or type glob.
    #[arg(long, value_name = "GLOB")]
    pub visual: Vec<String>,

    /// Only rows matching a site glob, e.g. sort.
    #[arg(long, value_name = "GLOB")]
    pub site: Vec<String>,

    /// Only rows matching a kind glob, e.g. measure.
    #[arg(long, value_name = "GLOB")]
    pub kind: Vec<String>,

    /// Only references matching a target glob; repeatable.
    #[arg(long = "match", value_name = "GLOB")]
    pub matches: Vec<String>,

    /// Only broken visual bindings (unresolved references).
    #[arg(long)]
    pub broken: bool,

    /// Print nothing; the exit code is the only output.
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Never color output (also honors NO_COLOR, TERM=dumb).
    #[arg(long)]
    pub no_color: bool,

    /// Never prompt; fail where a picker would appear.
    #[arg(long)]
    pub no_input: bool,
}

/// `ripbi scan` arguments.
#[derive(Args, Debug, Default)]
pub struct ScanArgs {
    /// A .pbip file, project folder, .SemanticModel, or .Report.
    pub path: Option<PathBuf>,

    /// Analyze one named semantic model; disables discovery.
    #[arg(long, value_name = "PATH", conflicts_with = "path")]
    pub model: Option<PathBuf>,

    /// Extra report root; repeatable; replaces ripbi.toml `reports`.
    #[arg(long = "report", value_name = "PATH")]
    pub reports: Vec<PathBuf>,

    /// Machine-readable JSON (schema: docs/output.md).
    #[arg(long, conflicts_with = "plain")]
    pub json: bool,

    /// One `<type>\t<id>` record per line, for grep/awk.
    #[arg(long)]
    pub plain: bool,

    /// Counts only, when the list would flood the terminal.
    #[arg(short = 's', long, conflicts_with_all = ["json", "plain"])]
    pub summary: bool,

    /// Print nothing; the exit code is the only output.
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Full pairing audit trail on stderr (default: capped lines).
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Any parser skip notice becomes an error (exit 2).
    #[arg(long)]
    pub strict: bool,

    /// Skip a model with no connected reports instead of refusing (exit 2):
    /// a notice on stderr, exit code 0, no scan output. Lets a pipeline point
    /// the scan at every model and let each run decide whether it has
    /// anything to scan against.
    #[arg(long)]
    pub allow_no_reports: bool,

    /// Never color output (also honors NO_COLOR, TERM=dumb).
    #[arg(long)]
    pub no_color: bool,

    /// Never prompt; fail where a picker would appear.
    #[arg(long)]
    pub no_input: bool,

    /// Only report unused measures.
    #[arg(long)]
    pub measures: bool,

    /// Only report unused columns.
    #[arg(long)]
    pub columns: bool,

    /// Only report unused hierarchies.
    #[arg(long)]
    pub hierarchies: bool,

    /// Only report unused tables; also keeps the Auto date/time section.
    #[arg(long)]
    pub tables: bool,

    /// Only report unused partitions.
    #[arg(long)]
    pub partitions: bool,

    /// Only report unused relationships.
    #[arg(long)]
    pub relationships: bool,

    /// Only report unused calculation items.
    #[arg(long)]
    pub calc_items: bool,

    /// Only report unused expressions.
    #[arg(long)]
    pub expressions: bool,

    /// Only report unused functions.
    #[arg(long)]
    pub functions: bool,

    /// Only report unused report measures.
    #[arg(long)]
    pub report_measures: bool,

    /// Only broken visual bindings; the only mode where they gate.
    #[arg(long)]
    pub broken: bool,

    /// Also print the "Power Query also names it" annotations.
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
            (self.broken, "broken_visual"),
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
            (
                ScanArgs {
                    broken: true,
                    ..Default::default()
                },
                "broken",
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
