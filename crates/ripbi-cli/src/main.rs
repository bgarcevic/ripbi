#![forbid(unsafe_code)]

//! The `ripbi` binary: parse, run one command, set the exit code. All logic
//! lives in the [`ripbi_cli`] library so tests can drive it without spawning
//! a process; the `rib` alias shares the same entry point.

fn main() -> std::process::ExitCode {
    ripbi_cli::entry::run()
}
