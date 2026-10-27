//! ripbi's application layer: everything between `ripbi-core`'s analysis and
//! a front end. It finds the model and its reports, reads `ripbi.toml`, runs
//! the scan pipeline, and returns typed results — never printing, never
//! exiting. The CLI renders these results as text, JSON, SARIF, and Markdown;
//! the desktop UIs render them as widgets.
//!
//! Start at [`scan::run`]; [`report::run`] and [`deps::run`] are the other
//! two commands, on the same [`scan::Observer`].

#![forbid(unsafe_code)]

pub mod compare;
pub mod config;
pub mod deps;
pub mod discover;
pub mod error;
pub mod glob;
pub mod json;
pub mod locate;
pub mod manifest;
pub mod markdown;
pub mod output;
pub mod report;
pub mod sarif;
pub mod scan;
pub mod tx_rm;
pub mod vso;

#[cfg(test)]
pub(crate) mod test_support;

pub use error::ScanError;
pub use scan::{Observer, Scan, ScanOutcome, ScanRequest, Verdict};
