#![forbid(unsafe_code)]

//! Core library for ripbi: Power BI ingestion, DAX lexing, and dependency-graph analysis.
//!
//! This crate never prints or exits; all fallible operations return [`Result`].

use thiserror::Error;

/// Errors produced by ripbi-core.
#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid archive: {0}")]
    Archive(#[from] zip::result::ZipError),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported or unrecognized source format: {0}")]
    UnsupportedFormat(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Normalized semantic model, regardless of source format.
#[derive(Debug, Default)]
pub struct TabularDatabase {
    pub tables: Vec<Table>,
}

#[derive(Debug, Default)]
pub struct Table {
    pub name: String,
    pub columns: Vec<String>,
    pub measures: Vec<Measure>,
}

#[derive(Debug, Default)]
pub struct Measure {
    pub name: String,
    pub expression: String,
}

/// Normalized report model: visuals, filters, and slicer bindings.
#[derive(Debug, Default)]
pub struct ReportModel {
    pub bound_fields: Vec<String>,
}
