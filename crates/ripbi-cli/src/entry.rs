//! Process entry shared by the `ripbi` and `rib` binaries: grab the real
//! stdio, run one command, set the exit code.

use std::io::{self, IsTerminal, Write};

use clap::Parser;

use crate::scan::{self, Streams};
use crate::{Cli, Command, notify, update};

/// Parse argv, run one command, produce the process exit code, and then give
/// the ambient update notifier its post-command turn (every command except
/// `update`; see `ambient_notice_follows`). The notifier never changes the
/// exit code.
pub fn run() -> std::process::ExitCode {
    let cli = Cli::parse();
    let quiet = match &cli.command {
        Command::Scan(args) => args.quiet,
        Command::Update(args) => args.quiet,
        Command::__UpdateCheck(_) => true,
    };
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let mut stdin = io::stdin().lock();
    let mut streams = Streams {
        out: &mut stdout,
        err: &mut stderr,
        input: &mut stdin,
        stdin_is_tty: io::stdin().is_terminal(),
        stdout_is_tty: io::stdout().is_terminal(),
        stderr_is_tty: io::stderr().is_terminal(),
    };
    let code = match &cli.command {
        Command::Scan(args) => scan::run(args, &mut streams),
        Command::Update(args) => update::run(args, &mut streams),
        Command::__UpdateCheck(_) => notify::run_check(),
    };
    if ambient_notice_follows(&cli.command) {
        notify::after_command(quiet, &mut streams);
    }
    let _ = io::stdout().flush();
    std::process::ExitCode::from(code as u8)
}

/// Whether the ambient update notice may follow this command. `update` is the
/// version command and is never followed: after a successful self-update the
/// running process still answers version questions with its old compile-time
/// constant, so the notice would claim the just-installed release is missing
/// (and `--check` already prints both versions). The hidden check child is the
/// notifier's own process and never notifies.
fn ambient_notice_follows(command: &Command) -> bool {
    !matches!(command, Command::Update(_) | Command::__UpdateCheck(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{UpdateArgs, UpdateCheckArgs};

    #[test]
    fn ambient_notice_follows_every_command_but_update() {
        assert!(ambient_notice_follows(&Command::Scan(Default::default())));
        assert!(!ambient_notice_follows(&Command::Update(
            UpdateArgs::default()
        )));
        assert!(!ambient_notice_follows(&Command::Update(UpdateArgs {
            check: true,
            ..UpdateArgs::default()
        })));
        assert!(!ambient_notice_follows(&Command::__UpdateCheck(
            UpdateCheckArgs::default()
        )));
    }
}
