#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! The Power BI / Fabric REST client behind `ripbi fetch`: sign in, find a
//! semantic model's downstream reports, forecast which ones the caller may
//! read, and download item definitions.
//!
//! Same discipline as `ripbi-core`: this crate never prints or exits, and
//! returns typed [`Result`]s. Unlike `ripbi-core` it talks to the network —
//! which is why it is its own crate: `ripbi-core` stays offline.
//!
//! Every call goes through two seams: [`http::Transport`] (the real one is
//! `ureq`) and [`http::Clock`]. Bearer tokens are only ever sent to the
//! configured endpoints and Power BI / Fabric hosts.

pub mod api;
pub mod auth;
pub mod downstream;
pub mod error;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod http;
pub mod link;
pub mod service;

pub use error::{Error, Result};
pub use service::Service;

#[cfg(test)]
mod tests;
