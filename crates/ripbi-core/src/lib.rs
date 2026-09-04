#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Core library for ripbi: Power BI ingestion, DAX lexing, and dependency-graph analysis.
//!
//! This crate never prints or exits; all fallible operations return [`Result`].
//!
//! Analysis starts from a [`TabularDatabase`] — the format-agnostic semantic model
//! every source format normalizes into — paired with a [`ModelIndex`] for resolving
//! the names that DAX and report bindings refer to objects by:
//!
//! ```
//! use ripbi_core::{Measure, ModelIndex, Table, TabularDatabase};
//!
//! let db = TabularDatabase {
//!     tables: vec![Table {
//!         name: "Sales".to_string(),
//!         measures: vec![Measure {
//!             name: "Total".to_string(),
//!             expression: "SUM(Sales[Amount])".to_string(),
//!             ..Default::default()
//!         }],
//!         ..Default::default()
//!     }],
//!     ..Default::default()
//! };
//!
//! // Names compare case-insensitively, as the Analysis Services engine does.
//! let index = ModelIndex::build(&db);
//! assert!(index.resolve_table("SALES").is_some());
//!
//! // Expressions are enumerated with their owner, ready for the DAX lexer.
//! let expressions = db.dax_expressions();
//! assert_eq!(expressions.len(), 1);
//! assert_eq!(expressions[0].text, "SUM(Sales[Amount])");
//! ```
//!
//! The report side is one [`ReportModel`] per report sharing the model: its
//! [`bindings`](ReportModel::bindings) are the reachability roots, and its
//! [`dax_expressions`](ReportModel::dax_expressions) add report-level measures on
//! top of the model's own expressions.

pub mod identity;
pub mod model;
pub mod report;

pub use identity::{FieldRef, NameKey, ObjectId};
pub use model::index::{
    ColumnHandle, ExpressionHandle, FunctionHandle, HierarchyHandle, MeasureHandle, ModelIndex,
    Resolved, TableHandle, UnqualifiedMatches,
};
pub use model::{
    CalculationGroup, CalculationItem, Calendar, Column, ColumnKind, DaxExpressionKind,
    DaxExpressionRef, ExpressionOwner, Function, Hierarchy, HierarchyLevel, Kpi, MExpressionRef,
    Measure, Partition, PartitionSource, Relationship, Role, SharedExpression, Table,
    TablePermission, TabularDatabase,
};
pub use report::{
    BindingKind, BindingRef, Bookmark, BookmarkSection, BookmarkVisual, DatasetReference,
    DrillthroughParameter, FieldTarget, FieldWell, Filter, Page, PageBinding, PageBindingKind,
    Projection, ReportMeasure, ReportModel, Visual,
};

use thiserror::Error;

/// Errors produced by ripbi-core.
#[derive(Debug, Error)]
pub enum Error {
    /// A file or directory could not be read.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A `.pbix`/`.pbit` archive could not be opened or is malformed.
    #[error("invalid archive: {0}")]
    Archive(#[from] zip::result::ZipError),
    /// A model or report JSON document could not be parsed.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The path is not a Power BI source this crate recognizes.
    #[error("unsupported or unrecognized source format: {0}")]
    UnsupportedFormat(String),
}

/// A result whose error is this crate's [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;
