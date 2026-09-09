//! Process entry shared by the `ripbi` and `rib` binaries: grab the real
//! stdio, run one command, set the exit code.

use std::io::{self, IsTerminal, Write};

use clap::Parser;

use crate::scan::{self, Streams};
use crate::{Cli, Command};

/// Parse argv, run one command, and produce the process exit code. The
/// invoked name comes from `argv\[0\]`, so help output says `ripbi` or `rib`
/// depending on which binary the user typed.
pub fn run() -> std::process::ExitCode {
    let cli = Cli::parse();
    let Command::Scan(args) = cli.command;
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let mut stdin = io::stdin().lock();
    let mut streams = Streams {
        out: &mut stdout,
        err: &mut stderr,
        input: &mut stdin,
        stdin_is_tty: io::stdin().is_terminal(),
    };
    let code = scan::run(&args, &mut streams);
    let _ = io::stdout().flush();
    std::process::ExitCode::from(code as u8)
}
