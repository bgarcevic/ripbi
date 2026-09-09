//! M lexing and reference resolution.
//!
//! Tokenizes the Power Query (M) expressions carried by the AST — table
//! partitions and model-level shared expressions — and extracts the object
//! references they contain: enough to build the dependency graph, not a full
//! parse tree. See [`crate::m::lexer`] for the tokenizer and
//! [`crate::m::refs`] for the extraction rules.
//!
//! The design rule is the same **conservatism** as the DAX side: zero false
//! negatives. Anything the extractor cannot rule out is treated as a reference
//! — an unqualified `[Name]` or a harvested column string names **every**
//! column of that name in the model, because M string arguments carry no row
//! context a lexer could consult. Over-marking costs precision; under-marking
//! deletes live code. Resolution failures are data, never errors: the graph
//! layer decides what an unresolvable reference means.
//!
//! What a reference is *worth* is the graph layer's call, and it is not
//! uniform: Power Query reads the source and produces the columns the model
//! maps onto, so a column named in M is the column's **supply chain**, not a
//! consumer — unloading it cannot break refresh. A table or shared expression
//! named in M is different: deleting it deletes the query this expression
//! reads or joins, which breaks refresh. [`crate::graph`] encodes that split.
//!
//! ```
//! use ripbi_core::{Column, ModelIndex, Table, TabularDatabase, m};
//!
//! let db = TabularDatabase {
//!     tables: vec![Table {
//!         name: "Sales".to_string(),
//!         columns: vec![
//!             Column { name: "Amount".to_string(), ..Default::default() },
//!             Column { name: "Region".to_string(), ..Default::default() },
//!         ],
//!         ..Default::default()
//!     }],
//!     ..Default::default()
//! };
//! let index = ModelIndex::build(&db);
//!
//! // The expanded column name resolves to the model column.
//! let mut refs = m::references(r#"Table.ExpandTableColumn(Source, "Amount")"#);
//! let binding = m::bind(&db, &index, refs.remove(0));
//! assert_eq!(binding.targets().len(), 1);
//!
//! // An unknown name is data, not an error.
//! let refs = m::references(r#"Table.SelectColumns(Source, {"Gone"})"#);
//! assert!(m::bind(&db, &index, refs.into_iter().next().unwrap()).is_unresolved());
//! ```

pub mod lexer;
pub mod refs;

pub use lexer::{Token, TokenKind, tokenize};
pub use refs::{RawRef, references, unescape_name};

use crate::identity::{NameKey, ObjectId};
use crate::model::TabularDatabase;
use crate::model::index::{ModelIndex, Resolved};

/// What one [`RawRef`] bound to in the model — the graph-ready outcome of
/// resolution, as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding<'a> {
    /// The reference matched at least one model object. `targets` holds the
    /// graph-node identities of **every** candidate: an unqualified `[Name]`
    /// or a column string names every column of that name, because M has no
    /// row context the lexer could resolve the name against. Naming is not
    /// keeping alive — what a target is worth (liveness vs supply-chain
    /// context) is [`crate::graph`]'s decision.
    Bound {
        /// The reference as written.
        raw: RawRef<'a>,
        /// The objects the reference names.
        targets: Vec<ObjectId>,
    },
    /// The reference matched nothing — a stale expression, a typo, a renamed
    /// object, or a built-in's non-column argument. Never an error: the graph
    /// layer decides what an unresolvable reference means (typically: ignore).
    Unresolved {
        /// The reference as written, for diagnostics.
        raw: RawRef<'a>,
    },
}

impl Binding<'_> {
    /// The bound objects, empty for an unresolved reference.
    #[must_use]
    pub fn targets(&self) -> &[ObjectId] {
        match self {
            Binding::Bound { targets, .. } => targets,
            Binding::Unresolved { .. } => &[],
        }
    }

    /// True when the reference matched nothing.
    #[must_use]
    pub fn is_unresolved(&self) -> bool {
        matches!(self, Binding::Unresolved { .. })
    }
}

/// Resolves every reference in one M expression against the model.
///
/// The convenience the graph builder uses: [`references`] plus [`bind`] for
/// each, preserving source order.
///
/// ```
/// use ripbi_core::{Column, ModelIndex, Table, TabularDatabase, m};
///
/// let db = TabularDatabase {
///     tables: vec![Table {
///         name: "Sales".to_string(),
///         columns: vec![Column { name: "Amount".to_string(), ..Default::default() }],
///         ..Default::default()
///     }],
///     ..Default::default()
/// };
/// let index = ModelIndex::build(&db);
///
/// let bindings = m::bindings(&db, &index, r#"Table.SelectColumns(Source, {"Amount"})"#);
/// assert_eq!(bindings[0].targets().len(), 1);
/// assert!(bindings[1].is_unresolved(), "`Source` is a variable, not a table");
/// ```
#[must_use]
pub fn bindings<'a>(db: &TabularDatabase, index: &ModelIndex, text: &'a str) -> Vec<Binding<'a>> {
    references(text)
        .into_iter()
        .map(|raw| bind(db, index, raw))
        .collect()
}

/// Resolves one extracted reference against the model.
///
/// Resolution rules — deliberately narrower than the DAX side where M's
/// semantics differ:
///
/// - qualified `#"Table"[Name]` → that table **and** its column — reading a
///   field from `Table` requires the `Table` query to exist at refresh — but
///   **never** a measure fallback: an M expression cannot reference a measure;
/// - unqualified `[Name]` and a harvested column string → **every** column of
///   that name in the model: M string arguments carry no row context, so the
///   conservative direction is to name all candidates;
/// - a bare or quoted name → the table and/or shared expression of that name
///   (most bare words are `let` variables and resolve to nothing — data).
///
/// ```
/// use ripbi_core::{Column, Measure, ModelIndex, Table, TabularDatabase, m};
///
/// let db = TabularDatabase {
///     tables: vec![
///         Table {
///             name: "Sales".to_string(),
///             columns: vec![Column { name: "Antal".to_string(), ..Default::default() }],
///             measures: vec![Measure { name: "Antal".to_string(), expression: "0".to_string(), ..Default::default() }],
///             ..Default::default()
///         },
///         Table {
///             name: "Dato".to_string(),
///             columns: vec![Column { name: "Antal".to_string(), ..Default::default() }],
///             ..Default::default()
///         },
///     ],
///     ..Default::default()
/// };
/// let index = ModelIndex::build(&db);
///
/// // A qualified name names the table and its column — no measure fallback.
/// let raw = m::references(r#"#"Sales"[Antal]"#).remove(0);
/// assert_eq!(m::bind(&db, &index, raw).targets().len(), 2);
///
/// // An unqualified name keeps every candidate alive, on every table.
/// let raw = m::references("[Antal]").remove(0);
/// assert_eq!(m::bind(&db, &index, raw).targets().len(), 2);
/// ```
#[must_use]
pub fn bind<'a>(db: &TabularDatabase, index: &ModelIndex, raw: RawRef<'a>) -> Binding<'a> {
    let mut targets = Vec::new();
    match &raw {
        RawRef::Field {
            table: Some(table),
            name,
            ..
        } => {
            // Resolution is by logical name: `#"` wrappers and `""` escapes
            // are stripped first, or `#"It""s"[X]` could never find the
            // table `It"s`.
            let table = unescape_name(table);
            let name = unescape_name(name);
            // The qualifier names a query this expression reads from: the
            // table node travels with the column so the graph layer can keep
            // the table alive (deleting it deletes the query).
            if let Some(handle) = index.resolve_table(&table)
                && let Some(resolved) = db.table(handle)
            {
                targets.push(ObjectId::Table {
                    table: NameKey::new(resolved.name.as_str()),
                });
            }
            if let Some(handle) = index.resolve_column(&table, &name)
                && let Some(id) = db.object_id(Resolved::Column(handle))
            {
                targets.push(id);
            }
        }
        RawRef::Field {
            table: None, name, ..
        }
        | RawRef::ColumnString { name, .. } => {
            let name = unescape_name(name);
            for handle in index.resolve_columns(&name) {
                if let Some(id) = db.object_id(Resolved::Column(handle)) {
                    targets.push(id);
                }
            }
        }
        RawRef::Name { name, .. } => {
            let name = unescape_name(name);
            if let Some(handle) = index.resolve_table(&name)
                && let Some(table) = db.table(handle)
            {
                targets.push(ObjectId::Table {
                    table: NameKey::new(table.name.as_str()),
                });
            }
            if let Some(handle) = index.resolve_expression(&name)
                && let Some(expression) = db.shared_expression(handle)
            {
                targets.push(ObjectId::Expression {
                    name: NameKey::new(expression.name.as_str()),
                });
            }
        }
    }

    if targets.is_empty() {
        Binding::Unresolved { raw }
    } else {
        Binding::Bound { raw, targets }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, Measure, SharedExpression, Table};

    /// Fixture with hand-checked positions:
    ///
    /// ```text
    /// table 0  "Sales"  columns 0 "Amount" 1 "Antal"   measures 0 "Total Sales" 1 "Antal"
    /// table 1  "Dato"   columns 0 "Måned" 1 "Antal"
    /// expressions 0 "ServerName"
    /// ```
    ///
    /// "Antal" is deliberately both a measure and two tables' column: that is
    /// the ambiguity the zero-false-negative rule exists for.
    fn model() -> TabularDatabase {
        let column = |name: &str| Column {
            name: name.to_string(),
            ..Default::default()
        };
        let measure = |name: &str| Measure {
            name: name.to_string(),
            expression: "0".to_string(),
            ..Default::default()
        };
        TabularDatabase {
            tables: vec![
                Table {
                    name: "Sales".to_string(),
                    columns: vec![column("Amount"), column("Antal")],
                    measures: vec![measure("Total Sales"), measure("Antal")],
                    ..Default::default()
                },
                Table {
                    name: "Dato".to_string(),
                    columns: vec![column("Måned"), column("Antal")],
                    ..Default::default()
                },
            ],
            expressions: vec![SharedExpression {
                name: "ServerName".to_string(),
                expression: "\"localhost\"".to_string(),
            }],
            ..Default::default()
        }
    }

    /// Binds every reference in `expression` and flattens their targets.
    fn targets_of(expression: &str) -> Vec<ObjectId> {
        let db = model();
        let index = ModelIndex::build(&db);
        bindings(&db, &index, expression)
            .iter()
            .flat_map(|binding| binding.targets().iter().cloned())
            .collect()
    }

    #[test]
    fn a_qualified_reference_names_the_table_and_the_column() {
        // `Antal` is also a Sales measure. DAX's binder keeps the measure
        // alive on a stale qualifier; M cannot reference measures at all, so
        // the qualified form names exactly the table plus its column.
        let targets = targets_of(r#"#"Sales"[Antal]"#);
        assert_eq!(
            targets,
            [
                ObjectId::Table {
                    table: NameKey::new("Sales"),
                },
                ObjectId::Column {
                    table: NameKey::new("Sales"),
                    column: NameKey::new("Antal"),
                },
            ]
        );

        // The same name unqualified names both tables' columns — and still no
        // measure, and no table: `[Antal]` does not say which query it reads.
        let targets = targets_of("[Antal]");
        assert_eq!(targets.len(), 2);
        assert!(
            !targets
                .iter()
                .any(|t| matches!(t, ObjectId::Measure { .. } | ObjectId::Table { .. }))
        );
    }

    #[test]
    fn a_column_string_keeps_every_column_of_that_name_alive() {
        // M string arguments carry no row context, so every candidate counts.
        let targets = targets_of(r#"Table.SelectColumns(Source, {"Antal"})"#);
        assert_eq!(targets.len(), 2);

        let targets = targets_of(r#"Table.ExpandTableColumn(Source, "Amount")"#);
        assert_eq!(
            targets,
            [ObjectId::Column {
                table: NameKey::new("Sales"),
                column: NameKey::new("Amount"),
            }]
        );
    }

    #[test]
    fn a_name_resolves_to_the_table_or_shared_expression_of_that_name() {
        let targets = targets_of("let Source = Sales in Source");
        assert!(targets.contains(&ObjectId::Table {
            table: NameKey::new("Sales"),
        }));

        let targets = targets_of("Sql.Database(ServerName)");
        assert_eq!(
            targets,
            [ObjectId::Expression {
                name: NameKey::new("ServerName"),
            }]
        );
    }

    #[test]
    fn resolution_is_case_insensitive_end_to_end() {
        let targets = targets_of(r#"#"SALES"[amount]"#);
        assert_eq!(
            targets,
            [
                ObjectId::Table {
                    table: NameKey::new("Sales"),
                },
                ObjectId::Column {
                    table: NameKey::new("Sales"),
                    column: NameKey::new("Amount"),
                },
            ]
        );
    }

    #[test]
    fn an_escaped_qualifier_resolves_to_the_escaped_name() {
        let db = TabularDatabase {
            tables: vec![Table {
                name: "It\"s".to_string(),
                columns: vec![Column {
                    name: "X".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let index = ModelIndex::build(&db);
        let raw = references(r#"#"It""s"[X]"#).remove(0);
        assert_eq!(
            bind(&db, &index, raw).targets(),
            [
                ObjectId::Table {
                    table: NameKey::new("It\"s"),
                },
                ObjectId::Column {
                    table: NameKey::new("It\"s"),
                    column: NameKey::new("X"),
                },
            ]
        );
    }

    #[test]
    fn an_unknown_name_binds_to_nothing_and_stays_data() {
        let db = model();
        let index = ModelIndex::build(&db);
        let bindings_found = bindings(&db, &index, "let Source = Gone in Source[Gone]");
        assert_eq!(bindings_found.len(), 3);
        assert!(bindings_found.iter().all(Binding::is_unresolved));
        assert!(bindings_found.iter().all(|b| b.targets().is_empty()));
    }
}
