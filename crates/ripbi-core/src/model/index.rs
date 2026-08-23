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
    #[must_use]
    pub fn primary(&self) -> Option<Resolved> {
        match (self.measure, self.column) {
            (Some(measure), _) => Some(Resolved::Measure(measure)),
            (None, Some(column)) => Some(Resolved::Column(column)),
            (None, None) => None,
        }
    }

    /// True when the name matched nothing. An unresolved reference is data — a
    /// stale expression, a typo, a table removed by hand — not an error.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.measure.is_none() && self.column.is_none()
    }
}

/// One table's share of the index: everything reachable by name *within* a table.
///
/// Handles are stored whole rather than as bare positions because two tables can
/// fold to the same name. The entry then belongs to the first of them, while a
/// handle inserted from the second still points at the table it actually came from.
#[derive(Debug, Clone, Default)]
struct TableEntry {
    /// Index of the first table with this folded name.
    table: usize,
    /// Folded column name → handle.
    columns: HashMap<String, ColumnHandle>,
    /// Folded hierarchy name → handle.
    hierarchies: HashMap<String, HierarchyHandle>,
}

/// Case-insensitive lookup index over a [`TabularDatabase`].
///
/// Build it once, after ingestion, with [`ModelIndex::build`]. Building never fails:
/// duplicate names are invalid in a valid model but tolerated here, with the **first**
/// occurrence kept and later ones ignored.
///
/// Per-table names are nested under their table rather than keyed by a
/// `(table, name)` pair, so a lookup folds each half once and allocates no tuple —
/// this is the DAX lexer's hot path, one call per reference in every expression.
#[derive(Debug, Clone)]
pub struct ModelIndex {
    /// Folded table name → that table's names.
    tables: HashMap<String, TableEntry>,
    /// Folded measure name → measure handle. Global: measure names are unique
    /// across the whole model, not just within their home table.
    measures: HashMap<String, MeasureHandle>,
    /// Folded shared-expression name → expression handle.
    expressions: HashMap<String, ExpressionHandle>,
}

impl ModelIndex {
    /// Indexes every table, column, measure, hierarchy, and shared expression.
    ///
    /// Runs in one pass over the model, folding each name once. On a duplicate
    /// folded name the first occurrence is kept.
    #[must_use]
    pub fn build(db: &TabularDatabase) -> Self {
        let mut tables: HashMap<String, TableEntry> = HashMap::new();
        let mut measures: HashMap<String, MeasureHandle> = HashMap::new();
        let mut expressions: HashMap<String, ExpressionHandle> = HashMap::new();

        for (table_idx, table) in db.tables.iter().enumerate() {
            // `or_insert_with` — not `insert` — is what makes the first occurrence win.
            let entry = tables
                .entry(fold_name(&table.name))
                .or_insert_with(|| TableEntry {
                    table: table_idx,
                    ..TableEntry::default()
                });

            for (column_idx, column) in table.columns.iter().enumerate() {
                entry
                    .columns
                    .entry(fold_name(&column.name))
                    .or_insert(ColumnHandle {
                        table: table_idx,
                        column: column_idx,
                    });
            }

            for (hierarchy_idx, hierarchy) in table.hierarchies.iter().enumerate() {
                entry
                    .hierarchies
                    .entry(fold_name(&hierarchy.name))
                    .or_insert(HierarchyHandle {
                        table: table_idx,
                        hierarchy: hierarchy_idx,
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
        }

        for (expression_idx, expression) in db.expressions.iter().enumerate() {
            expressions
                .entry(fold_name(&expression.name))
                .or_insert(ExpressionHandle(expression_idx));
        }

        Self {
            tables,
            measures,
            expressions,
        }
    }

    /// Looks up a table by name, case-insensitively.
    #[must_use]
    pub fn resolve_table(&self, name: &str) -> Option<TableHandle> {
        self.tables
            .get(&fold_name(name))
            .map(|entry| TableHandle(entry.table))
    }

    /// Resolves a qualified reference, `Table[Name]`.
    ///
    /// The named table's columns are tried first. Falling through to a measure is
    /// deliberate and conservative: measure names are model-global, so a qualified
    /// reference carrying a wrong or stale table prefix — `'Dato'[Total Sales]` for a
    /// measure that lives on `Sales`, or a prefix naming a table that no longer
    /// exists — still keeps that measure alive. Marking one object used too many is
    /// safe; marking one too few deletes live code.
    ///
    /// # Examples
    ///
    /// ```
    /// # use ripbi_core::{Measure, ModelIndex, Table, TabularDatabase};
    /// # let db = TabularDatabase {
    /// #     tables: vec![
    /// #         Table {
    /// #             name: "Sales".to_string(),
    /// #             measures: vec![Measure { name: "Total".to_string(), ..Default::default() }],
    /// #             ..Default::default()
    /// #         },
    /// #         Table { name: "Dato".to_string(), ..Default::default() },
    /// #     ],
    /// #     ..Default::default()
    /// # };
    /// // The model has one measure, `Total`, whose home table is `Sales`.
    /// let index = ModelIndex::build(&db);
    ///
    /// // A stale prefix still keeps it alive: measure names are model-global.
    /// assert!(index.resolve_qualified("Dato", "Total").is_some());
    /// assert!(index.resolve_qualified("No Such Table", "total").is_some());
    ///
    /// // A name that matches nothing resolves to nothing.
    /// assert!(index.resolve_qualified("Sales", "Nope").is_none());
    /// ```
    #[must_use]
    pub fn resolve_qualified(&self, table: &str, name: &str) -> Option<Resolved> {
        let folded_name = fold_name(name);
        let column = self
            .tables
            .get(&fold_name(table))
            .and_then(|entry| entry.columns.get(&folded_name));
        if let Some(column) = column {
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
    ///
    /// # Examples
    ///
    /// ```
    /// # use ripbi_core::{Column, Measure, ModelIndex, Table, TabularDatabase};
    /// # let db = TabularDatabase {
    /// #     tables: vec![
    /// #         Table {
    /// #             name: "Sales".to_string(),
    /// #             measures: vec![Measure { name: "Antal".to_string(), ..Default::default() }],
    /// #             ..Default::default()
    /// #         },
    /// #         Table {
    /// #             name: "Dato".to_string(),
    /// #             columns: vec![Column { name: "Antal".to_string(), ..Default::default() }],
    /// #             ..Default::default()
    /// #         },
    /// #     ],
    /// #     ..Default::default()
    /// # };
    /// // `Antal` is a measure on `Sales` and, separately, a column of `Dato`.
    /// let index = ModelIndex::build(&db);
    ///
    /// // Inside a `Dato` row context both are live candidates, so both come back.
    /// let ambiguous = index.resolve_unqualified("ANTAL", Some("Dato"));
    /// assert!(ambiguous.measure.is_some());
    /// assert!(ambiguous.column.is_some());
    ///
    /// // With no row context there is no column candidate to consider.
    /// assert!(index.resolve_unqualified("antal", None).column.is_none());
    ///
    /// // An unknown name is data, not an error.
    /// assert!(index.resolve_unqualified("Ukendt", Some("Dato")).is_empty());
    /// ```
    #[must_use]
    pub fn resolve_unqualified(&self, name: &str, home_table: Option<&str>) -> UnqualifiedMatches {
        let folded_name = fold_name(name);
        UnqualifiedMatches {
            measure: self.measures.get(&folded_name).copied(),
            column: home_table.and_then(|table| {
                self.tables
                    .get(&fold_name(table))
                    .and_then(|entry| entry.columns.get(&folded_name))
                    .copied()
            }),
        }
    }

    /// Looks up a hierarchy on a specific table, as written in `ISINSCOPE('Date'[Calendar])`
    /// or in a PBIR hierarchy binding. Hierarchy names are only unique per table, so
    /// there is no unqualified form and no cross-table fallback.
    #[must_use]
    pub fn resolve_hierarchy(&self, table: &str, name: &str) -> Option<HierarchyHandle> {
        self.tables
            .get(&fold_name(table))?
            .hierarchies
            .get(&fold_name(name))
            .copied()
    }

    /// Looks up a model-level shared M expression by name — how one M query
    /// references a parameter or another query.
    #[must_use]
    pub fn resolve_expression(&self, name: &str) -> Option<ExpressionHandle> {
        self.expressions.get(&fold_name(name)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, Hierarchy, Measure, SharedExpression, Table};
    use rstest::rstest;

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
    fn model() -> TabularDatabase {
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

    fn index() -> ModelIndex {
        ModelIndex::build(&model())
    }

    fn column_handle(table: usize, column: usize) -> Resolved {
        Resolved::Column(ColumnHandle { table, column })
    }

    fn measure_handle(table: usize, measure: usize) -> Resolved {
        Resolved::Measure(MeasureHandle { table, measure })
    }

    mod resolve_table {
        use super::*;

        #[rstest]
        #[case::upper("SALES", 0)]
        #[case::as_written("Sales", 0)]
        #[case::danish_lower("dato", 1)]
        #[case::danish_upper("DATO", 1)]
        fn finds_a_table_ignoring_case(#[case] name: &str, #[case] expected: usize) {
            assert_eq!(index().resolve_table(name), Some(TableHandle(expected)));
        }

        #[rstest]
        #[case::misspelled("Salez")]
        #[case::empty("")]
        fn returns_none_for_an_unknown_name(#[case] name: &str) {
            assert_eq!(index().resolve_table(name), None);
        }
    }

    mod resolve_qualified {
        use super::*;

        #[rstest]
        #[case::lower_table_upper_column("sales", "AMOUNT", 0, 0)]
        #[case::upper_table_danish_column("DATO", "måned", 1, 0)]
        #[case::danish_column_upper("Dato", "MÅNED", 1, 0)]
        fn finds_a_column_on_the_named_table(
            #[case] table: &str,
            #[case] name: &str,
            #[case] expected_table: usize,
            #[case] expected_column: usize,
        ) {
            assert_eq!(
                index().resolve_qualified(table, name),
                Some(column_handle(expected_table, expected_column))
            );
        }

        /// Measure names are model-global, so a qualified reference resolves to one
        /// even when the prefix is wrong or names a table that no longer exists —
        /// keeping it alive rather than reporting a live measure as unused.
        #[rstest]
        #[case::on_its_home_table("SALES", "total sales", 0, 0)]
        #[case::danish_on_its_home_table("Dato", "OMSÆTNING", 1, 0)]
        #[case::under_a_wrong_table_prefix("Dato", "Total Sales", 0, 0)]
        #[case::under_a_nonexistent_table_prefix("Ukendt Tabel", "omsætning", 1, 0)]
        fn finds_a_measure(
            #[case] table: &str,
            #[case] name: &str,
            #[case] expected_table: usize,
            #[case] expected_measure: usize,
        ) {
            assert_eq!(
                index().resolve_qualified(table, name),
                Some(measure_handle(expected_table, expected_measure))
            );
        }

        #[rstest]
        // "Amount" is a column of Sales and a measure nowhere, so a wrong prefix has
        // nothing to fall back to.
        #[case::column_of_a_different_table("Dato", "Amount")]
        #[case::unknown_name("Sales", "Nope")]
        #[case::unknown_table_and_name("Ukendt Tabel", "Måned")]
        fn returns_none_when_nothing_matches(#[case] table: &str, #[case] name: &str) {
            assert_eq!(index().resolve_qualified(table, name), None);
        }

        /// "Antal" is a column of Dato and a measure of Sales. The measure fallback is
        /// a last resort, not a shortcut past the named table's own columns.
        #[test]
        fn binds_to_the_named_tables_column_when_both_exist() {
            assert_eq!(
                index().resolve_qualified("Dato", "Antal"),
                Some(column_handle(1, 1))
            );
        }

        #[test]
        fn binds_to_the_measure_when_the_named_table_has_no_such_column() {
            assert_eq!(
                index().resolve_qualified("Sales", "antal"),
                Some(measure_handle(0, 1))
            );
        }
    }

    mod resolve_unqualified {
        use super::*;

        /// Measures are model-global, so a Danish measure on `Dato` resolves from an
        /// expression with no row context at all.
        #[test]
        fn finds_a_global_measure_without_a_home_table() {
            assert_eq!(
                index().resolve_unqualified("OMSÆTNING", None).measure,
                Some(MeasureHandle {
                    table: 1,
                    measure: 0
                })
            );
        }

        #[test]
        fn offers_no_column_candidate_without_a_home_table() {
            assert_eq!(index().resolve_unqualified("OMSÆTNING", None).column, None);
        }

        /// `[Antal]` inside a `Dato` row context could bind to either object, so both
        /// come back and both stay alive.
        #[test]
        fn returns_the_measure_candidate_for_an_ambiguous_name() {
            assert_eq!(
                index().resolve_unqualified("antal", Some("Dato")).measure,
                Some(MeasureHandle {
                    table: 0,
                    measure: 1
                })
            );
        }

        #[test]
        fn returns_the_column_candidate_for_an_ambiguous_name() {
            assert_eq!(
                index().resolve_unqualified("antal", Some("Dato")).column,
                Some(ColumnHandle {
                    table: 1,
                    column: 1
                })
            );
        }

        #[test]
        fn drops_the_column_candidate_when_there_is_no_home_table() {
            assert_eq!(index().resolve_unqualified("antal", None).column, None);
        }

        #[test]
        fn finds_a_column_of_the_home_table() {
            assert_eq!(
                index().resolve_unqualified("BELØB", Some("sales")).column,
                Some(ColumnHandle {
                    table: 0,
                    column: 1
                })
            );
        }

        #[test]
        fn offers_no_measure_candidate_for_a_column_only_name() {
            assert_eq!(
                index().resolve_unqualified("BELØB", Some("sales")).measure,
                None
            );
        }

        /// A column is never global: the same name against the wrong row context is
        /// not a match.
        #[test]
        fn matches_nothing_against_the_wrong_home_table() {
            let found = index().resolve_unqualified("Beløb", Some("Dato"));

            assert!(found.is_empty(), "expected no candidates, got {found:?}");
        }

        #[rstest]
        #[case::known_home_table(Some("Sales"))]
        #[case::unknown_home_table(Some("Ukendt Tabel"))]
        #[case::no_home_table(None)]
        fn is_empty_for_an_unknown_name(#[case] home_table: Option<&str>) {
            let found = index().resolve_unqualified("Ukendt", home_table);

            assert!(found.is_empty(), "expected no candidates, got {found:?}");
        }

        #[test]
        fn primary_prefers_the_measure_when_a_name_is_ambiguous() {
            assert_eq!(
                index().resolve_unqualified("antal", Some("Dato")).primary(),
                Some(measure_handle(0, 1))
            );
        }

        #[test]
        fn primary_returns_the_column_when_there_is_no_measure() {
            assert_eq!(
                index()
                    .resolve_unqualified("BELØB", Some("sales"))
                    .primary(),
                Some(column_handle(0, 1))
            );
        }

        #[test]
        fn primary_is_none_for_an_unknown_name() {
            assert_eq!(
                index()
                    .resolve_unqualified("Ukendt", Some("Sales"))
                    .primary(),
                None
            );
        }
    }

    mod resolve_hierarchy {
        use super::*;

        #[rstest]
        #[case::lower_table_upper_name("dato", "KALENDER")]
        #[case::upper_table_as_written("DATO", "Kalender")]
        fn finds_a_hierarchy_ignoring_case(#[case] table: &str, #[case] name: &str) {
            assert_eq!(
                index().resolve_hierarchy(table, name),
                Some(HierarchyHandle {
                    table: 1,
                    hierarchy: 0
                })
            );
        }

        #[rstest]
        // Hierarchy names are unique per table only: no cross-table fallback, and no
        // confusion with the columns or measures of the same table.
        #[case::another_table("Sales", "Kalender")]
        #[case::a_column_name("Dato", "Måned")]
        #[case::unknown_table("Ukendt Tabel", "Kalender")]
        fn returns_none(#[case] table: &str, #[case] name: &str) {
            assert_eq!(index().resolve_hierarchy(table, name), None);
        }
    }

    mod resolve_expression {
        use super::*;

        #[rstest]
        #[case::upper("SERVER", 0)]
        #[case::as_written("Server", 0)]
        #[case::lower("database", 1)]
        fn finds_an_expression_ignoring_case(#[case] name: &str, #[case] expected: usize) {
            assert_eq!(
                index().resolve_expression(name),
                Some(ExpressionHandle(expected))
            );
        }

        #[test]
        fn returns_none_for_an_unknown_name() {
            assert_eq!(index().resolve_expression("Ukendt"), None);
        }
    }

    /// Tables 0 ("Sales") and 2 ("sales") fold to the same key. Duplicates are invalid
    /// in a real model but occur in hand-edited files, so the first one wins.
    mod build_with_duplicate_names {
        use super::*;

        #[rstest]
        #[case::as_written("Sales")]
        #[case::as_the_duplicate_is_spelled("sales")]
        fn resolves_the_table_to_the_first_occurrence(#[case] name: &str) {
            assert_eq!(index().resolve_table(name), Some(TableHandle(0)));
        }

        #[test]
        fn resolves_a_shared_column_name_to_the_first_tables_column() {
            assert_eq!(
                index().resolve_qualified("Sales", "Amount"),
                Some(column_handle(0, 0))
            );
        }

        #[test]
        fn resolves_a_shared_column_name_unqualified_to_the_first_tables_column() {
            assert_eq!(
                index().resolve_unqualified("Amount", Some("SALES")).column,
                Some(ColumnHandle {
                    table: 0,
                    column: 0
                })
            );
        }
    }

    mod accessors {
        use super::*;

        #[test]
        fn a_resolved_table_handle_reaches_its_table() {
            let db = model();
            let handle = ModelIndex::build(&db).resolve_table("SALES").unwrap();

            assert_eq!(db.table(handle).unwrap().name, "Sales");
        }

        #[test]
        fn a_resolved_column_handle_reaches_its_column() {
            let db = model();
            let Some(Resolved::Column(handle)) =
                ModelIndex::build(&db).resolve_qualified("sales", "BELØB")
            else {
                panic!("expected 'sales'[BELØB] to resolve to a column");
            };

            assert_eq!(db.column(handle).unwrap().name, "Beløb");
        }

        #[test]
        fn a_resolved_measure_handle_reaches_its_measure() {
            let db = model();
            let handle = ModelIndex::build(&db)
                .resolve_unqualified("omsætning", None)
                .measure
                .unwrap();

            assert_eq!(db.measure(handle).unwrap().name, "Omsætning");
        }

        #[test]
        fn a_resolved_hierarchy_handle_reaches_its_hierarchy() {
            let db = model();
            let handle = ModelIndex::build(&db)
                .resolve_hierarchy("DATO", "kalender")
                .unwrap();

            assert_eq!(db.hierarchy(handle).unwrap().name, "Kalender");
        }

        #[test]
        fn a_resolved_expression_handle_reaches_its_expression() {
            let db = model();
            let handle = ModelIndex::build(&db)
                .resolve_expression("DATABASE")
                .unwrap();

            assert_eq!(db.shared_expression(handle).unwrap().name, "Database");
        }

        #[test]
        fn a_stale_table_handle_is_none() {
            assert!(model().table(TableHandle(9)).is_none());
        }

        #[test]
        fn a_column_handle_with_an_out_of_range_table_is_none() {
            let handle = ColumnHandle {
                table: 9,
                column: 9,
            };

            assert!(model().column(handle).is_none());
        }

        #[test]
        fn a_column_handle_with_an_out_of_range_column_is_none() {
            let handle = ColumnHandle {
                table: 0,
                column: 9,
            };

            assert!(model().column(handle).is_none());
        }

        #[test]
        fn a_measure_handle_with_an_out_of_range_table_is_none() {
            let handle = MeasureHandle {
                table: 9,
                measure: 0,
            };

            assert!(model().measure(handle).is_none());
        }

        #[test]
        fn a_measure_handle_into_a_table_without_measures_is_none() {
            let handle = MeasureHandle {
                table: 2,
                measure: 0,
            };

            assert!(model().measure(handle).is_none());
        }

        #[test]
        fn a_hierarchy_handle_into_a_table_without_hierarchies_is_none() {
            let handle = HierarchyHandle {
                table: 0,
                hierarchy: 0,
            };

            assert!(model().hierarchy(handle).is_none());
        }

        #[test]
        fn a_stale_expression_handle_is_none() {
            assert!(model().shared_expression(ExpressionHandle(9)).is_none());
        }

        #[test]
        fn an_object_id_for_a_stale_column_handle_is_none() {
            assert!(model().object_id(column_handle(9, 9)).is_none());
        }

        #[test]
        fn an_object_id_for_a_stale_measure_handle_is_none() {
            assert!(model().object_id(measure_handle(9, 9)).is_none());
        }
    }

    /// Ids are what diagnostics print, so they must carry the model's own casing even
    /// when the lookup that produced them was written in another one.
    mod object_id {
        use super::*;

        #[test]
        fn carries_the_models_casing_for_a_measure() {
            let db = model();
            let resolved = ModelIndex::build(&db)
                .resolve_qualified("SALES", "TOTAL SALES")
                .unwrap();

            assert_eq!(
                db.object_id(resolved).unwrap().to_string(),
                "'Sales'[Total Sales]"
            );
        }

        #[test]
        fn carries_the_models_casing_for_a_danish_column() {
            let db = model();
            let resolved = ModelIndex::build(&db)
                .resolve_qualified("DATO", "MÅNED")
                .unwrap();

            assert_eq!(db.object_id(resolved).unwrap().to_string(), "'Dato'[Måned]");
        }

        #[test]
        fn distinguishes_a_column_from_a_measure_of_the_same_name() {
            let db = model();

            assert_ne!(
                db.object_id(column_handle(1, 1)).unwrap(),
                db.object_id(measure_handle(0, 1)).unwrap()
            );
        }

        #[rstest]
        #[case::the_dato_column(column_handle(1, 1), "'Dato'[Antal]")]
        #[case::the_sales_measure(measure_handle(0, 1), "'Sales'[Antal]")]
        fn renders_each_antal_under_its_own_table(
            #[case] resolved: Resolved,
            #[case] expected: &str,
        ) {
            assert_eq!(model().object_id(resolved).unwrap().to_string(), expected);
        }
    }
}
