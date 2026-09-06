//! DAX lexing and reference resolution.
//!
//! Tokenizes the DAX expressions carried by the AST (measures, calculated
//! columns and tables, RLS filters, calculation items, user-defined functions)
//! and extracts the object references they contain — enough to build the
//! dependency graph, not a full parse tree.
//!
//! The tokenizer is a Rust port of SQLBI Whiteboard's `DaxLexer`
//! (<https://github.com/sql-bi/SQLBI-Whiteboard>), © SQLBI, MIT licensed — thank
//! you for battle-testing the fiddly corners of DAX lexing (doubled-delimiter
//! escapes, dot-absorbing identifiers, the `Sales[E]` exponent trap). Reduced
//! here to what reference extraction needs: no case normalization, no comment
//! attachment, no formatter. Everything `DaxPrinter`-shaped upstream was
//! deliberately not ported, and definition-name extraction (`DefinedObjectName`,
//! `IsQuery`) is unnecessary because ingestion already knows each expression's
//! owner.
//!
//! The design rule is **conservative**: zero false positives. Anything the lexer
//! cannot prove is treated as a use — an unqualified `[Name]` keeps every
//! candidate alive, a bare identifier is a candidate table, and a call is a
//! candidate user-defined function. Over-marking costs precision; under-marking
//! deletes live code. Resolution failures are data, never errors: the graph
//! layer decides what an unresolvable reference means.
//!
//! ```
//! use ripbi_core::dax;
//!
//! // Lexing is purely syntactic: no model needed.
//! let refs = dax::references("SUM('Sales Header'[Net Price])");
//! assert_eq!(refs.len(), 2); // the SUM call candidate + the field reference
//!
//! // ...but materializing a FieldRef and resolving against a model is one call.
//! let field = refs[1].to_field_ref().expect("a field reference");
//! assert_eq!(field.to_string(), "'Sales Header'[Net Price]");
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
    /// can bind a measure and a home-table column at once, because a lexer
    /// cannot tell row context from filter context.
    Bound {
        /// The reference as written.
        raw: RawRef<'a>,
        /// The objects the reference keeps alive.
        targets: Vec<ObjectId>,
    },
    /// The reference matched nothing — a stale expression, a typo, a renamed
    /// object, or a built-in function. Never an error: the graph layer decides
    /// what an unresolvable reference means (typically: ignore).
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

/// Resolves one extracted reference against the model.
///
/// `home_table` is the row-context table of the expression the reference was
/// found in — [`DaxExpressionRef::home_table`](crate::DaxExpressionRef) carries
/// it. Resolution rules live in [`ModelIndex`]; this wrapper only turns its
/// answers into graph-ready data:
///
/// - qualified `'Table'[Name]` → the table's column, falling back to the
///   model-global measure (`resolve_qualified`);
/// - unqualified `[Name]` → the measure **and** the home-table column, both
///   kept alive (`resolve_unqualified`);
/// - bare table → the table (`resolve_table`);
/// - a call whose name is a user-defined function → that function
///   (`resolve_function`); built-ins resolve to nothing and stay unresolved.
///
/// ```
/// use ripbi_core::{Column, Measure, ModelIndex, Table, TabularDatabase, dax};
///
/// let db = TabularDatabase {
///     tables: vec![
///         Table {
///             name: "Sales".to_string(),
///             measures: vec![Measure {
///                 name: "Antal".to_string(),
///                 expression: "0".to_string(),
///                 ..Default::default()
///             }],
///             ..Default::default()
///         },
///         Table {
///             name: "Dato".to_string(),
///             columns: vec![Column {
///                 name: "Antal".to_string(),
///                 ..Default::default()
///             }],
///             ..Default::default()
///         },
///     ],
///     ..Default::default()
/// };
/// let index = ModelIndex::build(&db);
///
/// // `[Antal]` in a `Dato` row context is ambiguous: measure and column both.
/// let raw = dax::references("[Antal]").remove(0);
/// let binding = dax::bind(&db, &index, Some("Dato"), raw);
/// assert_eq!(binding.targets().len(), 2);
///
/// // An unknown name is data, not an error.
/// let raw = dax::references("[Ukendt]").remove(0);
/// assert!(dax::bind(&db, &index, Some("Dato"), raw).is_unresolved());
/// ```
#[must_use]
pub fn bind<'a>(
    db: &TabularDatabase,
    index: &ModelIndex,
    home_table: Option<&str>,
    raw: RawRef<'a>,
) -> Binding<'a> {
    let mut targets = Vec::new();
    match raw {
        RawRef::Field {
            table: Some(table),
            name,
            ..
        } => {
            if let Some(id) = index
                .resolve_qualified(table, name)
                .and_then(|r| db.object_id(r))
            {
                targets.push(id);
            }
        }
        RawRef::Field {
            table: None, name, ..
        } => {
            let matches = index.resolve_unqualified(name, home_table);
            for resolved in [
                matches.measure.map(Resolved::Measure),
                matches.column.map(Resolved::Column),
            ] {
                if let Some(id) = resolved.and_then(|r| db.object_id(r)) {
                    targets.push(id);
                }
            }
        }
        RawRef::Table { name, .. } => {
            if let Some(table) = index.resolve_table(name).and_then(|h| db.table(h)) {
                targets.push(ObjectId::Table {
                    table: NameKey::new(table.name.as_str()),
                });
            }
        }
        RawRef::Function { name, .. } => {
            if let Some(function) = index.resolve_function(name).and_then(|h| db.function(h)) {
                targets.push(ObjectId::Function {
                    name: NameKey::new(function.name.as_str()),
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
    use crate::identity::FieldRef;
    use crate::model::{Column, Function, Measure, Table};

    /// Fixture with hand-checked positions:
    ///
    /// ```text
    /// table 0  "Sales"  columns 0 "Amount" 1 "Beløb"   measures 0 "Total Sales" 1 "Antal"
    /// table 1  "Dato"   columns 0 "Måned" 1 "Antal"
    /// functions 0 "MyFunc"
    /// ```
    ///
    /// "Antal" is deliberately both a measure and a column: that is the
    /// ambiguity the zero-false-positive rule exists for.
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
                    columns: vec![column("Amount"), column("Beløb")],
                    measures: vec![measure("Total Sales"), measure("Antal")],
                    ..Default::default()
                },
                Table {
                    name: "Dato".to_string(),
                    columns: vec![column("Måned"), column("Antal")],
                    ..Default::default()
                },
            ],
            functions: vec![Function {
                name: "MyFunc".to_string(),
                expression: "1".to_string(),
                is_hidden: false,
            }],
            ..Default::default()
        }
    }

    /// Binds every reference in `expression` and returns their targets.
    fn bound<'a>(expression: &'a str, home_table: Option<&str>) -> Vec<Binding<'a>> {
        let db = model();
        let index = ModelIndex::build(&db);
        references(expression)
            .into_iter()
            .map(|raw| bind(&db, &index, home_table, raw))
            .collect()
    }

    #[test]
    fn a_qualified_reference_resolves_to_the_column_first() {
        let bindings = bound("'Sales'[Amount]", None);
        assert_eq!(bindings.len(), 1);
        assert_eq!(
            bindings[0].targets(),
            [ObjectId::Column {
                table: NameKey::new("Sales"),
                column: NameKey::new("Amount"),
            }]
        );
    }

    #[test]
    fn a_stale_qualifier_still_keeps_the_measure_alive() {
        // Measure names are model-global: a wrong or renamed table prefix must
        // not orphan them.
        let bindings = bound("'No Such Table'[TOTAL SALES]", None);
        assert_eq!(
            bindings[0].targets(),
            [ObjectId::Measure {
                table: NameKey::new("Sales"),
                measure: NameKey::new("Total Sales"),
            }]
        );
    }

    #[test]
    fn an_ambiguous_unqualified_reference_keeps_every_candidate_alive() {
        // "Antal" is a measure on Sales and a column on Dato; inside a Dato row
        // context both are live.
        let bindings = bound("[Antal]", Some("Dato"));
        assert_eq!(bindings[0].targets().len(), 2);
        assert!(bindings[0].targets().contains(&ObjectId::Measure {
            table: NameKey::new("Sales"),
            measure: NameKey::new("Antal"),
        }));
        assert!(bindings[0].targets().contains(&ObjectId::Column {
            table: NameKey::new("Dato"),
            column: NameKey::new("Antal"),
        }));

        // With no row context there is no column candidate to consider.
        let bindings = bound("[Antal]", None);
        assert_eq!(bindings[0].targets().len(), 1);
    }

    #[test]
    fn resolution_is_case_insensitive_end_to_end() {
        let bindings = bound("[total sales]", Some("dato"));
        assert_eq!(
            bindings[0].targets(),
            [ObjectId::Measure {
                table: NameKey::new("Sales"),
                measure: NameKey::new("Total Sales"),
            }]
        );
    }

    #[test]
    fn an_unknown_name_binds_to_nothing_and_stays_data() {
        let bindings = bound("[Ukendt] + 'X'[Y]", Some("Sales"));
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().all(Binding::is_unresolved));
        assert!(bindings.iter().all(|b| b.targets().is_empty()));
    }

    #[test]
    fn a_bare_table_use_resolves_to_the_table() {
        // Refs: two COUNTROWS calls (unresolved built-ins), 'Sales' (a table
        // use), and Missing (a table use that matches nothing).
        let bindings = bound("COUNTROWS('Sales') + COUNTROWS(Missing)", Some("Sales"));
        assert_eq!(bindings.len(), 4);
        assert!(
            bindings[0].is_unresolved(),
            "COUNTROWS is not a model function"
        );
        assert_eq!(
            bindings[1].targets(),
            [ObjectId::Table {
                table: NameKey::new("Sales"),
            }]
        );
        assert!(bindings[2].is_unresolved());
        assert!(bindings[3].is_unresolved());
    }

    #[test]
    fn a_user_defined_function_resolves_and_built_ins_do_not() {
        // Refs: MyFunc(, [Amount], SUM(, [Amount].
        let bindings = bound("MyFunc([Amount]) + SUM([Amount])", Some("Sales"));
        assert_eq!(bindings.len(), 4);
        assert_eq!(
            bindings[0].targets(),
            [ObjectId::Function {
                name: NameKey::new("MyFunc"),
            }]
        );
        assert_eq!(
            bindings[1].targets(),
            [ObjectId::Column {
                table: NameKey::new("Sales"),
                column: NameKey::new("Amount"),
            }]
        );
        assert!(
            bindings[2].is_unresolved(),
            "built-in SUM matches no model function"
        );
        assert!(!bindings[3].is_unresolved());
    }

    #[test]
    fn materialized_field_refs_display_valid_dax() {
        let db = model();
        let index = ModelIndex::build(&db);
        let raw = references("'It''s'[X]").remove(0);
        let field = raw.to_field_ref().expect("a field reference");
        assert_eq!(
            field,
            FieldRef {
                table: Some(NameKey::new("It's")),
                name: NameKey::new("X"),
            }
        );
        // Display re-applies the escaping the lexer stripped.
        assert_eq!(field.to_string(), "'It''s'[X]");
        // Unresolvable here — the fixture has no such table — but well-formed.
        assert!(bind(&db, &index, None, raw).is_unresolved());
    }

    #[test]
    fn the_tricky_sample_binds_only_its_real_reference() {
        // Ported from the SQLBI smoke tests: variables and the definition name
        // resolve to nothing in a model without such tables.
        let source = concat!(
            "Tricky :=\n",
            "-- a real comment\n",
            "VAR Year = 2024\n",
            "VAR Note = \"-- not a comment\"\n",
            "RETURN Year & Note & Sales[Amount]\n",
        );
        let bindings = bound(source, Some("Sales"));
        assert_eq!(
            bindings.len(),
            9,
            "every bare word is a conservative candidate"
        );
        assert_eq!(
            bindings.iter().filter(|b| !b.is_unresolved()).count(),
            1,
            "only Sales[Amount] resolves"
        );
        assert_eq!(bindings.last().expect("field ref last").targets().len(), 1);
    }
}
