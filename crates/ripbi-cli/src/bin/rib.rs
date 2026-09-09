#![forbid(unsafe_code)]

//! The `rib` binary: ripbi's short alias. Same parser, same commands, same
//! exit codes — only the name users type differs. It shares
//! [`ripbi_cli::entry::run`] with `src/main.rs`.

fn main() -> std::process::ExitCode {
    ripbi_cli::entry::run()
}
