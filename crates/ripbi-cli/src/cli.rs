//! The clap argument definitions — the interface users type and scripts pin,
//! per `docs/cli-ux-guidelines.md`. Parsing only; behavior lives in
//! [`crate::scan`], [`crate::deps`], and [`crate::report`].

use std::collections::HashSet;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

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
  ripbi scan --type measure --type column  # only these unused object types
  ripbi scan -q                            # exit code only: 0 clean, 1 unused found, 2 error

Selection flags union; with none of them, everything is reported.";

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
  ripbi report --model models/Sales.SemanticModel  # explicit inputs; --report adds reports
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
  ripbi deps \"'Sales'[Total Sales]\"  # both directions; discovers a project in the current directory
  ripbi deps --model models/Sales.SemanticModel  # explicit inputs; --report adds bindings
  ripbi deps                         # no object: compact overview — counts, never the whole graph
  ripbi deps \"'Sales'[Total Sales]\" --dependencies  # what the measure relies on
  ripbi deps \"'Sales'[Total Sales]\" --impact  # what could be affected by a change
  ripbi deps \"'Sales'[Total Sales]\" --impact --depth 1  # direct impact only
  ripbi deps --table Sales           # explore a whole table
  ripbi deps \"'Sales'[Total Sales]\" --impact --in-report Executive  # impact in one report
  ripbi deps \"'Sales'[Total Sales]\" --plain
  ripbi deps \"'Sales'[Total Sales]\" --json";

/// `ripbi deps` arguments.
#[derive(Args, Debug, Default)]
pub struct DepsArgs {
    /// The model object to explore — a reference like `'Table'[Name]`, never
    /// a filesystem path.
    pub object: Option<String>,

    /// Explore one named semantic model; disables discovery.
    #[arg(long, value_name = "PATH")]
    pub model: Option<PathBuf>,

    /// Extra report binding source; repeatable; replaces ripbi.toml
    /// `reports`.
    #[arg(long = "report", value_name = "PATH")]
    pub reports: Vec<PathBuf>,

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

    /// Explore a whole type (e.g. measure, column) instead of one object;
    /// repeatable or comma-separated.
    #[arg(
        long = "type",
        value_name = "TYPE",
        value_delimiter = ',',
        conflicts_with = "object"
    )]
    pub types: Vec<String>,

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

    /// Draw the slice as a topology diagram instead of a tree.
    #[arg(long, conflicts_with_all = ["plain", "json"])]
    pub graph: bool,

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
    /// A .pbip, .pbix, .pbit, .abf, model.bim, project folder, .SemanticModel, or .Report.
    pub path: Option<PathBuf>,

    /// Inventory one named semantic model; disables discovery.
    #[arg(long, value_name = "PATH", conflicts_with = "path")]
    pub model: Option<PathBuf>,

    /// Extra report root; repeatable; replaces ripbi.toml `reports`.
    #[arg(long = "report", value_name = "PATH")]
    pub reports: Vec<PathBuf>,

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

    /// Inventory reports even when no semantic model pairs with them.
    #[arg(long)]
    pub allow_no_model: bool,

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
    /// A .pbip, .pbix, .pbit, .abf, model.bim, project folder, .SemanticModel, or .Report.
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

    /// Skip a model with no connected reports instead of refusing (exit 2).
    #[arg(long)]
    pub allow_no_reports: bool,

    /// Never color output (also honors NO_COLOR, TERM=dumb).
    #[arg(long)]
    pub no_color: bool,

    /// Never prompt; fail where a picker would appear.
    #[arg(long)]
    pub no_input: bool,

    /// Only report these object types, e.g. measure, column; repeatable or
    /// comma-separated.
    #[arg(long = "type", value_name = "TYPE", value_delimiter = ',')]
    pub types: Vec<String>,

    /// Only broken artifacts and visual bindings; the only mode where they gate.
    #[arg(long)]
    pub broken: bool,

    /// Also print the "Power Query also names it" annotations.
    #[arg(long)]
    pub power_query: bool,

    /// Order unused findings by name or by storage size (PBIX and .abf only).
    #[arg(long, value_enum, value_name = "KEY", default_value_t)]
    pub sort: SortKey,
}

/// How `scan` orders its unused findings.
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    /// By object identity (the default).
    #[default]
    Name,
    /// Largest storage first; objects without size data last.
    Size,
}

impl ScanArgs {
    /// The breakage kinds selected by `--broken`, or `None` when it is
    /// absent. `scan` unions this with validated `--type` selections.
    #[must_use]
    pub fn selected_kinds(&self) -> Option<HashSet<&'static str>> {
        self.broken
            .then(|| HashSet::from(["broken_visual", "broken_artifact"]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    /// Help is an interface, not an essay: every flag's help and every
    /// subcommand's about is one short line, scannable in a terminal column.
    /// The full contract lives in `docs/*.md` — this gate keeps an
    /// over-eager doc comment (human or LLM) from shipping a paragraph into
    /// `--help`. Raise the offender's detail in the docs instead.
    #[test]
    fn help_text_stays_one_scannable_line() {
        const MAX_HELP: usize = 100;

        fn check_line(
            offenders: &mut Vec<String>,
            label: &str,
            text: Option<&clap::builder::StyledStr>,
        ) {
            if let Some(text) = text {
                let text = text.to_string();
                if text.contains('\n') || text.chars().count() > MAX_HELP {
                    offenders.push(format!("{label}: {text:?}"));
                }
            }
        }

        let mut offenders: Vec<String> = Vec::new();
        let cli = Cli::command();
        for command in std::iter::once(cli.clone()).chain(cli.get_subcommands().cloned()) {
            let name = command.get_name().to_string();
            check_line(&mut offenders, &name, command.get_about());
            for arg in command.get_arguments() {
                let id = arg.get_id().to_string();
                for help in [arg.get_help(), arg.get_long_help()].into_iter().flatten() {
                    let help = help.to_string();
                    if help.contains('\n') || help.chars().count() > MAX_HELP {
                        offenders.push(format!("{name} --{id}: {help:?}"));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "help text over {MAX_HELP} chars or multi-line — move the detail to docs/:\n{}",
            offenders.join("\n")
        );
    }

    /// Commands that share scan's input ladder (`--model`, `--report`) lead
    /// their help with those flags: declaration order is help order, and the
    /// inputs are the first thing a new user needs to find.
    #[test]
    fn shared_input_flags_lead_the_help() {
        let cli = Cli::command();
        for command in cli.get_subcommands() {
            let takes_model = command.get_arguments().any(|arg| arg.get_id() == "model");
            if !takes_model {
                continue;
            }
            let name = command.get_name();
            let options: Vec<String> = command
                .get_arguments()
                .filter(|arg| !arg.is_positional() && arg.get_id() != "help")
                .map(|arg| arg.get_id().to_string())
                .collect();
            // The ids are the struct field names; `--report`'s field is
            // `reports` because the flag is repeatable.
            assert_eq!(
                options.first().map(String::as_str),
                Some("model"),
                "{name} must declare --model first — the inputs lead the help"
            );
            assert_eq!(
                options.get(1).map(String::as_str),
                Some("reports"),
                "{name} must declare --report second"
            );
        }
    }

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

    /// `--broken` is the only boolean selection; object kinds use `--type`.
    #[test]
    fn broken_selects_both_breakage_kinds() {
        assert_eq!(ScanArgs::default().selected_kinds(), None);
        assert_eq!(
            ScanArgs {
                broken: true,
                ..Default::default()
            }
            .selected_kinds(),
            Some(HashSet::from(["broken_visual", "broken_artifact"]))
        );
    }

    #[test]
    fn bookmark_uses_the_existing_type_flag() {
        let cli = Cli::try_parse_from(["ripbi", "scan", "--type", "bookmark"])
            .expect("bookmark type parses");
        let Command::Scan(args) = cli.command else {
            panic!("expected scan");
        };
        assert_eq!(args.types, ["bookmark"]);
    }
}
