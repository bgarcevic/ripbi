//! Process entry shared by the `ripbi` and `rib` binaries: grab the real
//! stdio, run one command, set the exit code.

use std::io::{self, IsTerminal, Write};

use clap::Parser;

use crate::scan::{self, Streams};
use crate::{Cli, Command, notify, update};

/// Parse argv, run one command, produce the process exit code, and then give
/// the ambient update notifier its post-command turn (every command, present
/// and future). The notifier never changes the exit code.
pub fn run() -> std::process::ExitCode {
    let cli = Cli::parse();
    let quiet = match &cli.command {
        Command::Scan(args) => args.quiet,
        Command::Update(args) => args.quiet,
        Command::__UpdateCheck(_) => true,
    };
    let hidden = matches!(&cli.command, Command::__UpdateCheck(_));
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let mut stdin = io::stdin().lock();
    let mut streams = Streams {
        out: &mut stdout,
        err: &mut stderr,
        input: &mut stdin,
        stdin_is_tty: io::stdin().is_terminal(),
        stderr_is_tty: io::stderr().is_terminal(),
    };
    let code = match &cli.command {
        Command::Scan(args) => scan::run(args, &mut streams),
        Command::Update(args) => update::run(args, &mut streams),
        Command::__UpdateCheck(_) => notify::run_check(),
    };
    if !hidden {
        notify::after_command(quiet, &mut streams);
    }
    let _ = io::stdout().flush();
    std::process::ExitCode::from(code as u8)
}
