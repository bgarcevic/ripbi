//! Case-insensitive name lookup over a [`TabularDatabase`].
//!
//! DAX and PBIR bindings reference model objects by name; the graph layer needs a
//! stable node key. This module bridges the two: [`ModelIndex`] is built once after
//! ingestion and turns a written name into a positional handle
//! ([`TableHandle`], [`ColumnHandle`], [`MeasureHandle`], [`HierarchyHandle`],
//! [`ExpressionHandle`]), which the accessors on [`TabularDatabase`] turn back into
//! borrowed AST nodes and [`ObjectId`](crate::identity::ObjectId)s.
//!
//! Two rules govern every resolution here:
//!
//! - **Zero false positives.** When a reference is genuinely ambiguous — an
//!   unqualified `[Name]` that is both a measure and a column of the home table —
//!   [`resolve_unqualified`](ModelIndex::resolve_unqualified) returns *all*
//!   candidates. Marking too much used is safe; marking too little deletes live code.
//! - **Never fail on drift.** Duplicate names are invalid in a real model but do
//!   occur in hand-edited files; the first occurrence wins and the build never panics.
//!   An unresolvable name is data (`None`), not an error.
//!
//! All keys and all lookup inputs pass through [`crate::identity::fold_name`], the
//! single case-folding chokepoint.

use std::collections::HashMap;

use crate::identity::fold_name;
use crate::model::TabularDatabase;

/// Positional index of a table in [`TabularDatabase::tables`].
///
/// Only meaningful against the database the index was built from, which must not
/// be mutated afterwards. Every accessor treats a stale handle as a miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TableHandle(
    /// Index into [`TabularDatabase::tables`].
    pub usize,
);

/// Positional index of a column: its table, then its position in
/// [`Table::columns`](crate::model::Table::columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColumnHandle {
    /// Index into [`TabularDatabase::tables`].
    pub table: usize,
    /// Index into that table's `columns`.
    pub column: usize,
}

/// Positional index of a measure: its home table, then its position in
/// [`Table::measures`](crate::model::Table::measures).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeasureHandle {
    /// Index into [`TabularDatabase::tables`].
    pub table: usize,
    /// Index into that table's `measures`.
    pub measure: usize,
}

/// Positional index of a hierarchy: its table, then its position in
/// [`Table::hierarchies`](crate::model::Table::hierarchies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HierarchyHandle {
    /// Index into [`TabularDatabase::tables`].
    pub table: usize,
    /// Index into that table's `hierarchies`.
    pub hierarchy: usize,
}

/// Positional index of a model-level shared M expression in
/// [`TabularDatabase::expressions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExpressionHandle(
    /// Index into [`TabularDatabase::expressions`].
    pub usize,
);

/// What a field reference resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resolved {
    /// The reference names a column.
    Column(ColumnHandle),
    /// The reference names a measure.
    Measure(MeasureHandle),
}

/// Every candidate an unqualified `[Name]` reference could bind to.
///
/// The graph layer must add an edge to **every** candidate. In DAX row context
/// `[Name]` binds to the home table's column; outside row context it binds to the
/// measure of that name. A lexer cannot tell the two apart without a full parse and
/// semantic analysis, so both objects must stay alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnqualifiedMatches {
    /// The model-global measure of that name. Measure names are unique across the
    /// whole model (the engine enforces it), so no home table is needed.
    pub measure: Option<MeasureHandle>,
    /// The home table's column of that name, when a home table was supplied.
    pub column: Option<ColumnHandle>,
}

impl UnqualifiedMatches {
    /// The single best answer for callers that cannot carry ambiguity: the measure
    /// if there is one, otherwise the column.
    ///
    /// The graph layer must **not** use this — it would drop a live candidate. It
    /// exists for diagnostics and for callers that only need something to display.
    pub fn primary(&self) -> Option<Resolved> {
        match (self.measure, self.column) {
            (Some(measure), _) => Some(Resolved::Measure(measure)),
            (None, Some(column)) => Some(Resolved::Column(column)),
            (None, None) => None,
        }
    }

    /// True when the name matched nothing. An unresolved reference is data — a
    /// stale expression, a typo, a table removed by hand — not an error.
    pub fn is_empty(&self) -> bool {
        self.measure.is_none() && self.column.is_none()
    }
}

/// Case-insensitive lookup index over a [`TabularDatabase`].
///
/// Build it once, after ingestion, with [`ModelIndex::build`]. Building never fails:
/// duplicate names are invalid in a valid model but tolerated here, with the **first**
/// occurrence kept and later ones ignored.
#[derive(Debug, Clone)]
pub struct ModelIndex {
    /// Folded table name → index into `db.tables`.
    tables: HashMap<String, usize>,
    /// (folded table name, folded column name) → column handle.
    columns: HashMap<(String, String), ColumnHandle>,
    /// Folded measure name → measure handle. Global: measure names are unique
    /// across the whole model, not just within their home table.
    measures: HashMap<String, MeasureHandle>,
    /// (folded table name, folded hierarchy name) → hierarchy handle.
    hierarchies: HashMap<(String, String), HierarchyHandle>,
    /// Folded shared-expression name → expression handle.
    expressions: HashMap<String, ExpressionHandle>,
}

impl ModelIndex {
    /// Indexes every table, column, measure, hierarchy, and shared expression.
    ///
    /// Runs in one pass over the model and allocates one folded `String` per name.
    /// On a duplicate folded name the first occurrence is kept.
    pub fn build(db: &TabularDatabase) -> Self {
        let mut tables: HashMap<String, usize> = HashMap::new();
        let mut columns: HashMap<(String, String), ColumnHandle> = HashMap::new();
        let mut measures: HashMap<String, MeasureHandle> = HashMap::new();
        let mut hierarchies: HashMap<(String, String), HierarchyHandle> = HashMap::new();
        let mut expressions: HashMap<String, ExpressionHandle> = HashMap::new();

        for (table_idx, table) in db.tables.iter().enumerate() {
            let folded_table = fold_name(&table.name);
            // `or_insert` — not `insert` — is what makes the first occurrence win.
            tables.entry(folded_table.clone()).or_insert(table_idx);

            for (column_idx, column) in table.columns.iter().enumerate() {
                columns
                    .entry((folded_table.clone(), fold_name(&column.name)))
                    .or_insert(ColumnHandle {
                        table: table_idx,
                        column: column_idx,
                    });
            }

            for (measure_idx, measure) in table.measures.iter().enumerate() {
                measures
                    .entry(fold_name(&measure.name))
                    .or_insert(MeasureHandle {
                        table: table_idx,
                        measure: measure_idx,
                    });
            }

            for (hierarchy_idx, hierarchy) in table.hierarchies.iter().enumerate() {
                hierarchies
                    .entry((folded_table.clone(), fold_name(&hierarchy.name)))
                    .or_insert(HierarchyHandle {
                        table: table_idx,
                        hierarchy: hierarchy_idx,
                    });
            }
        }

        for (expression_idx, expression) in db.expressions.iter().enumerate() {
            expressions
                .entry(fold_name(&expression.name))
                .or_insert(ExpressionHandle(expression_idx));
        }

        Self {
            tables,
            columns,
            measures,
            hierarchies,
            expressions,
        }
    }

    /// Looks up a table by name, case-insensitively.
    pub fn resolve_table(&self, name: &str) -> Option<TableHandle> {
        self.tables.get(&fold_name(name)).copied().map(TableHandle)
    }

    /// Resolves a qualified reference, `Table[Name]`.
    ///
    /// The named table's columns are tried first. Falling through to a measure is
    /// deliberate and conservative: measure names are model-global, so a qualified
    /// reference carrying a wrong or stale table prefix — `'Dato'[Total Sales]` for a
    /// measure that lives on `Sales`, or a prefix naming a table that no longer
    /// exists — still keeps that measure alive. Marking one object used too many is
    /// safe; marking one too few deletes live code.
    pub fn resolve_qualified(&self, table: &str, name: &str) -> Option<Resolved> {
        let folded_name = fold_name(name);
        if let Some(column) = self.columns.get(&(fold_name(table), folded_name.clone())) {
            return Some(Resolved::Column(*column));
        }
        self.measures
            .get(&folded_name)
            .copied()
            .map(Resolved::Measure)
    }

    /// Resolves an unqualified reference, `[Name]`, to **all** its candidates.
    ///
    /// `home_table` is the row-context table of the expression the reference was
    /// found in; pass `None` where there is none. See [`UnqualifiedMatches`] for why
    /// both a measure and a column can come back at once.
    pub fn resolve_unqualified(&self, name: &str, home_table: Option<&str>) -> UnqualifiedMatches {
        let folded_name = fold_name(name);
        UnqualifiedMatches {
            measure: self.measures.get(&folded_name).copied(),
            column: home_table.and_then(|table| {
                self.columns
                    .get(&(fold_name(table), folded_name.clone()))
                    .copied()
            }),
        }
    }

    /// Looks up a hierarchy on a specific table, as written in `ISINSCOPE('Date'[Calendar])`
    /// or in a PBIR hierarchy binding. Hierarchy names are only unique per table, so
    /// there is no unqualified form and no cross-table fallback.
    pub fn resolve_hierarchy(&self, table: &str, name: &str) -> Option<HierarchyHandle> {
        self.hierarchies
            .get(&(fold_name(table), fold_name(name)))
            .copied()
    }

    /// Looks up a model-level shared M expression by name — how one M query
    /// references a parameter or another query.
    pub fn resolve_expression(&self, name: &str) -> Option<ExpressionHandle> {
        self.expressions.get(&fold_name(name)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, Hierarchy, Measure, SharedExpression, Table};

    fn column(name: &str) -> Column {
        Column {
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn measure(name: &str) -> Measure {
        Measure {
            name: name.to_string(),
            expression: "0".to_string(),
            ..Default::default()
        }
    }

    /// Fixture with hand-checked positions. Every assertion below names these
    /// indices as literals, so a resolution that drifts by one position fails.
    ///
    /// ```text
    /// table 0  "Sales"   columns  0 "Amount"  1 "Beløb"
    ///                    measures 0 "Total Sales"  1 "Antal"
    /// table 1  "Dato"    columns  0 "Måned"   1 "Antal"
    ///                    measures 0 "Omsætning"
    ///                    hierarchies 0 "Kalender"
    /// table 2  "sales"   columns  0 "Amount"        <- duplicate of table 0
    /// expressions 0 "Server"  1 "Database"
    /// ```
    ///
    /// "Antal" is deliberately both a measure (on `Sales`) and a column (on `Dato`):
    /// that is the ambiguity the zero-false-positive rule exists for.
    fn fixture() -> TabularDatabase {
        TabularDatabase {
            name: Some("Contoso".to_string()),
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
                    measures: vec![measure("Omsætning")],
                    hierarchies: vec![Hierarchy {
                        name: "Kalender".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                Table {
                    name: "sales".to_string(),
                    columns: vec![column("Amount")],
                    ..Default::default()
                },
            ],
            expressions: vec![
                SharedExpression {
                    name: "Server".to_string(),
                    expression: "\"contoso.database.windows.net\"".to_string(),
                },
                SharedExpression {
                    name: "Database".to_string(),
                    expression: "\"AdventureWorks\"".to_string(),
                },
            ],
            ..Default::default()
        }
    }

    // --- resolve_table ---------------------------------------------------------

    #[test]
    fn resolve_table_is_case_insensitive_including_danish() {
        let index = ModelIndex::build(&fixture());

        assert_eq!(index.resolve_table("SALES"), Some(TableHandle(0)));
        assert_eq!(index.resolve_table("Sales"), Some(TableHandle(0)));
        assert_eq!(index.resolve_table("dato"), Some(TableHandle(1)));
        assert_eq!(index.resolve_table("DATO"), Some(TableHandle(1)));
        assert_eq!(index.resolve_table("DATO"), index.resolve_table("dato"));

        assert_eq!(index.resolve_table("Salez"), None);
        assert_eq!(index.resolve_table(""), None);
    }

    // --- resolve_qualified -----------------------------------------------------

    #[test]
    fn resolve_qualified_finds_a_column_on_the_named_table() {
        let index = ModelIndex::build(&fixture());

        assert_eq!(
            index.resolve_qualified("sales", "AMOUNT"),
            Some(Resolved::Column(ColumnHandle {
                table: 0,
                column: 0
            }))
        );
        assert_eq!(
            index.resolve_qualified("DATO", "måned"),
            Some(Resolved::Column(ColumnHandle {
                table: 1,
                column: 0
            }))
        );
        assert_eq!(
            index.resolve_qualified("Dato", "MÅNED"),
            Some(Resolved::Column(ColumnHandle {
                table: 1,
                column: 0
            }))
        );

        // "Amount" is a column of Sales and not a measure anywhere, so a wrong table
        // prefix has nothing to fall back to: not the Sales column, not anything.
        assert_eq!(index.resolve_qualified("Dato", "Amount"), None);
        assert_eq!(index.resolve_qualified("Sales", "Nope"), None);
    }

    #[test]
    fn resolve_qualified_finds_a_measure_on_its_home_table() {
        let index = ModelIndex::build(&fixture());

        assert_eq!(
            index.resolve_qualified("SALES", "total sales"),
            Some(Resolved::Measure(MeasureHandle {
                table: 0,
                measure: 0
            }))
        );
        assert_eq!(
            index.resolve_qualified("Dato", "OMSÆTNING"),
            Some(Resolved::Measure(MeasureHandle {
                table: 1,
                measure: 0
            }))
        );
    }

    #[test]
    fn resolve_qualified_keeps_a_measure_alive_under_a_wrong_table_prefix() {
        let index = ModelIndex::build(&fixture());

        // "Total Sales" lives on Sales, but is written with the Dato prefix.
        assert_eq!(
            index.resolve_qualified("Dato", "Total Sales"),
            Some(Resolved::Measure(MeasureHandle {
                table: 0,
                measure: 0
            }))
        );
        // A prefix naming a table that does not exist at all resolves too.
        assert_eq!(
            index.resolve_qualified("Ukendt Tabel", "omsætning"),
            Some(Resolved::Measure(MeasureHandle {
                table: 1,
                measure: 0
            }))
        );
        // But an unknown table with an unknown name stays unresolved.
        assert_eq!(index.resolve_qualified("Ukendt Tabel", "Måned"), None);
    }

    #[test]
    fn resolve_qualified_prefers_the_named_tables_column_over_a_global_measure() {
        let index = ModelIndex::build(&fixture());

        // "Antal" is a column of Dato and a measure of Sales. Qualified by Dato it
        // must be the column: the measure fallback is a last resort, not a shortcut.
        assert_eq!(
            index.resolve_qualified("Dato", "Antal"),
            Some(Resolved::Column(ColumnHandle {
                table: 1,
                column: 1
            }))
        );
        // Qualified by any other table there is no such column, so the measure wins.
        assert_eq!(
            index.resolve_qualified("Sales", "antal"),
            Some(Resolved::Measure(MeasureHandle {
                table: 0,
                measure: 1
            }))
        );
    }

    // --- resolve_unqualified ---------------------------------------------------

    #[test]
    fn resolve_unqualified_finds_a_global_measure_without_a_home_table() {
        let index = ModelIndex::build(&fixture());

        // Headline: measures are model-global, so a Danish measure on Dato resolves
        // from an expression that has no row context at all.
        let found = index.resolve_unqualified("OMSÆTNING", None);
        assert_eq!(
            found.measure,
            Some(MeasureHandle {
                table: 1,
                measure: 0
            })
        );
        assert_eq!(found.column, None);
        assert!(!found.is_empty());
        assert_eq!(
            found.primary(),
            Some(Resolved::Measure(MeasureHandle {
                table: 1,
                measure: 0
            }))
        );
    }

    #[test]
    fn resolve_unqualified_returns_both_candidates_for_an_ambiguous_name() {
        let index = ModelIndex::build(&fixture());

        // [Antal] inside a Dato row context could be either object; both must live.
        let found = index.resolve_unqualified("antal", Some("Dato"));
        assert_eq!(
            found.measure,
            Some(MeasureHandle {
                table: 0,
                measure: 1
            })
        );
        assert_eq!(
            found.column,
            Some(ColumnHandle {
                table: 1,
                column: 1
            })
        );
        assert!(!found.is_empty());
        assert_eq!(
            found.primary(),
            Some(Resolved::Measure(MeasureHandle {
                table: 0,
                measure: 1
            }))
        );

        // Without a home table there is no column candidate to add.
        let no_home = index.resolve_unqualified("antal", None);
        assert_eq!(
            no_home.measure,
            Some(MeasureHandle {
                table: 0,
                measure: 1
            })
        );
        assert_eq!(no_home.column, None);
    }

    #[test]
    fn resolve_unqualified_falls_back_to_the_home_table_column() {
        let index = ModelIndex::build(&fixture());

        // "Beløb" is only ever a column of Sales.
        let found = index.resolve_unqualified("BELØB", Some("sales"));
        assert_eq!(
            found.column,
            Some(ColumnHandle {
                table: 0,
                column: 1
            })
        );
        assert_eq!(found.measure, None);
        assert_eq!(
            found.primary(),
            Some(Resolved::Column(ColumnHandle {
                table: 0,
                column: 1
            }))
        );

        // The same name against the wrong row context matches nothing: a column is
        // never global.
        let wrong_home = index.resolve_unqualified("Beløb", Some("Dato"));
        assert!(wrong_home.is_empty());
        assert_eq!(wrong_home.primary(), None);
    }

    #[test]
    fn resolve_unqualified_is_empty_for_an_unknown_name() {
        let index = ModelIndex::build(&fixture());

        let found = index.resolve_unqualified("Ukendt", Some("Sales"));
        assert!(found.is_empty());
        assert_eq!(found.measure, None);
        assert_eq!(found.column, None);
        assert_eq!(found.primary(), None);

        // An unknown home table is equally harmless.
        assert!(index
            .resolve_unqualified("Ukendt", Some("Ukendt Tabel"))
            .is_empty());
    }

    // --- resolve_hierarchy / resolve_expression --------------------------------

    #[test]
    fn resolve_hierarchy_is_case_insensitive_and_table_scoped() {
        let index = ModelIndex::build(&fixture());

        assert_eq!(
            index.resolve_hierarchy("dato", "KALENDER"),
            Some(HierarchyHandle {
                table: 1,
                hierarchy: 0
            })
        );
        assert_eq!(
            index.resolve_hierarchy("DATO", "Kalender"),
            Some(HierarchyHandle {
                table: 1,
                hierarchy: 0
            })
        );

        // No cross-table fallback, and no confusion with columns or measures.
        assert_eq!(index.resolve_hierarchy("Sales", "Kalender"), None);
        assert_eq!(index.resolve_hierarchy("Dato", "Måned"), None);
        assert_eq!(index.resolve_hierarchy("Ukendt Tabel", "Kalender"), None);
    }

    #[test]
    fn resolve_expression_is_case_insensitive() {
        let index = ModelIndex::build(&fixture());

        assert_eq!(
            index.resolve_expression("SERVER"),
            Some(ExpressionHandle(0))
        );
        assert_eq!(
            index.resolve_expression("Server"),
            Some(ExpressionHandle(0))
        );
        assert_eq!(
            index.resolve_expression("database"),
            Some(ExpressionHandle(1))
        );
        assert_eq!(index.resolve_expression("Ukendt"), None);
    }

    // --- duplicate tolerance ---------------------------------------------------

    #[test]
    fn build_keeps_the_first_of_two_duplicate_table_names() {
        // Tables 0 ("Sales") and 2 ("sales") fold to the same key.
        let index = ModelIndex::build(&fixture());

        assert_eq!(index.resolve_table("Sales"), Some(TableHandle(0)));
        assert_eq!(index.resolve_table("sales"), Some(TableHandle(0)));
        // The duplicate's columns must not shadow the original's either.
        assert_eq!(
            index.resolve_qualified("Sales", "Amount"),
            Some(Resolved::Column(ColumnHandle {
                table: 0,
                column: 0
            }))
        );
        assert_eq!(
            index.resolve_unqualified("Amount", Some("SALES")).column,
            Some(ColumnHandle {
                table: 0,
                column: 0
            })
        );
    }

    // --- accessors -------------------------------------------------------------

    #[test]
    fn accessors_round_trip_resolved_handles() {
        let db = fixture();
        let index = ModelIndex::build(&db);

        let table = index.resolve_table("SALES").unwrap();
        assert_eq!(db.table(table).unwrap().name, "Sales");

        let Some(Resolved::Column(column)) = index.resolve_qualified("sales", "BELØB") else {
            panic!("expected a column");
        };
        assert_eq!(
            column,
            ColumnHandle {
                table: 0,
                column: 1
            }
        );
        assert_eq!(db.column(column).unwrap().name, "Beløb");

        let measure = index
            .resolve_unqualified("omsætning", None)
            .measure
            .unwrap();
        assert_eq!(
            measure,
            MeasureHandle {
                table: 1,
                measure: 0
            }
        );
        assert_eq!(db.measure(measure).unwrap().name, "Omsætning");

        let hierarchy = index.resolve_hierarchy("DATO", "kalender").unwrap();
        assert_eq!(db.hierarchy(hierarchy).unwrap().name, "Kalender");

        let expression = index.resolve_expression("DATABASE").unwrap();
        assert_eq!(expression, ExpressionHandle(1));
        assert_eq!(db.shared_expression(expression).unwrap().name, "Database");
        assert_eq!(
            db.shared_expression(expression).unwrap().expression,
            "\"AdventureWorks\""
        );
    }

    #[test]
    fn object_id_preserves_original_casing() {
        let db = fixture();
        let index = ModelIndex::build(&db);

        // Looked up in upper case; the id must carry the model's own casing, because
        // the id is what diagnostics print.
        let measure = index.resolve_qualified("SALES", "TOTAL SALES").unwrap();
        assert_eq!(
            db.object_id(measure).unwrap().to_string(),
            "'Sales'[Total Sales]"
        );

        let column = index.resolve_qualified("DATO", "MÅNED").unwrap();
        assert_eq!(db.object_id(column).unwrap().to_string(), "'Dato'[Måned]");

        // Column and measure of the same name are distinct ids, not just distinct text.
        let antal_column = Resolved::Column(ColumnHandle {
            table: 1,
            column: 1,
        });
        let antal_measure = Resolved::Measure(MeasureHandle {
            table: 0,
            measure: 1,
        });
        assert_ne!(
            db.object_id(antal_column).unwrap(),
            db.object_id(antal_measure).unwrap()
        );
        assert_eq!(
            db.object_id(antal_column).unwrap().to_string(),
            "'Dato'[Antal]"
        );
        assert_eq!(
            db.object_id(antal_measure).unwrap().to_string(),
            "'Sales'[Antal]"
        );
    }

    #[test]
    fn accessors_return_none_for_stale_handles() {
        let db = fixture();

        assert!(db.table(TableHandle(9)).is_none());
        assert!(db
            .column(ColumnHandle {
                table: 9,
                column: 9
            })
            .is_none());
        // In-range table, out-of-range column.
        assert!(db
            .column(ColumnHandle {
                table: 0,
                column: 9
            })
            .is_none());
        assert!(db
            .measure(MeasureHandle {
                table: 9,
                measure: 0
            })
            .is_none());
        // Table 2 has no measures at all.
        assert!(db
            .measure(MeasureHandle {
                table: 2,
                measure: 0
            })
            .is_none());
        assert!(db
            .hierarchy(HierarchyHandle {
                table: 0,
                hierarchy: 0
            })
            .is_none());
        assert!(db.shared_expression(ExpressionHandle(9)).is_none());

        assert!(db
            .object_id(Resolved::Column(ColumnHandle {
                table: 9,
                column: 9
            }))
            .is_none());
        assert!(db
            .object_id(Resolved::Measure(MeasureHandle {
                table: 9,
                measure: 9
            }))
            .is_none());
    }
}
