//! ripbi's command-line interface: argument parsing, source-format dispatch,
//! presentation, and exit codes — the thin orchestration layer over
//! `ripbi-core`, and the only crate that prints or sets exit codes.

pub mod cli;
pub use ripbi_app::{compare, config, discover, error, glob};

pub mod deps;
pub mod entry;
pub mod fetch;
pub mod markdown;
pub mod notify;
pub mod picker;
pub mod progress;
pub mod render;
pub mod report;
pub mod sarif;
pub mod scan;
pub mod stub;
pub mod style;
pub mod tx_rm;
pub mod update;
pub mod vso;

pub use cli::{Cli, Command};
pub use scan::Streams;
