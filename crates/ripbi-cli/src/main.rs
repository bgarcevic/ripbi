#![forbid(unsafe_code)]

//! The `ripbi` binary: parse, run one command, set the exit code. All logic
//! lives in the [`ripbi_cli`] library so tests can drive it without spawning
//! a process.

use std::io::{self, IsTerminal, Write};

use clap::Parser;
use ripbi_cli::{Cli, Command, Streams};

fn main() -> std::process::ExitCode {
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
    let code = ripbi_cli::scan::run(&args, &mut streams);
    let _ = io::stdout().flush();
    std::process::ExitCode::from(code as u8)
}
