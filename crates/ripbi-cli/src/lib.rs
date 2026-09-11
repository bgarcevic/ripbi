//! ripbi's command-line interface: argument parsing, source-format dispatch,
//! presentation, and exit codes — the thin orchestration layer over
//! `ripbi-core`, and the only crate that prints or sets exit codes.

pub mod cli;
pub mod config;
pub mod discover;
pub mod entry;
pub mod error;
pub mod glob;
pub mod notify;
pub mod render;
pub mod scan;
pub mod style;
pub mod update;

#[cfg(test)]
pub(crate) mod test_support;

pub use cli::{Cli, Command};
pub use scan::Streams;
