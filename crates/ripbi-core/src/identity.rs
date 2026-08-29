//! Shared object-identity layer for the tabular AST, the report AST, and the DAX lexer.
//!
//! Analysis Services compares object names case-insensitively under the invariant
//! culture, so every name that participates in equality, hashing, or graph lookups is
//! wrapped in [`NameKey`]. Original casing is preserved for display; only the folded
//! form is ever compared.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Canonical case folding for object-name comparison, matching the Analysis
/// Services engine's case-insensitive (invariant-culture) semantics.
/// Unicode-aware: Danish "MÅNED" == "måned". Locale-insensitive by design.
pub(crate) fn fold_name(s: &str) -> String {
    s.to_lowercase()
}

/// Writes a name as a single-quoted DAX identifier, doubling any internal quote.
///
/// Allocation-free: the input is emitted in slices around each quote character.
pub(crate) struct Quoted<'a>(pub(crate) &'a str);

impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("'")?;
        let mut rest = self.0;
        while let Some(i) = rest.find('\'') {
            f.write_str(&rest[..i])?;
            f.write_str("''")?;
            rest = &rest[i + 1..];
        }
        f.write_str(rest)?;
        f.write_str("'")
    }
}

/// An object name that compares, hashes, and orders case-insensitively while
/// preserving the original casing for display.
///
/// The folded form is computed once at construction, so equality and hashing are
/// plain string operations on a precomputed field.
///
/// [`std::borrow::Borrow<str>`] is deliberately **not** implemented: `Borrow` requires
/// that the borrowed value hash and compare identically to the owner, which cannot hold
/// when [`Eq`]/[`Hash`] use `folded` while [`as_str`](NameKey::as_str) yields `original`.
///
/// # Examples
///
/// ```
/// use ripbi_core::NameKey;
///
/// // Case is irrelevant to identity, in ASCII and beyond.
/// assert_eq!(NameKey::new("Sales"), NameKey::new("SALES"));
/// assert_eq!(NameKey::new("MÅNED"), NameKey::new("måned"));
///
/// // ...but the model's own casing survives for display.
/// assert_eq!(NameKey::new("SaLeS").as_str(), "SaLeS");
/// ```
#[derive(Debug, Clone)]
pub struct NameKey {
    original: String,
    folded: String,
}

impl NameKey {
    /// Creates a key from a name as written in the source model.
    pub fn new(name: impl Into<String>) -> Self {
        let original = name.into();
        let folded = fold_name(&original);
        Self { original, folded }
    }

    /// The name with its original casing, as written in the source model.
    pub fn as_str(&self) -> &str {
        &self.original
    }

    /// The case-folded form used for equality, hashing, and ordering.
    pub fn folded(&self) -> &str {
        &self.folded
    }
}

impl PartialEq for NameKey {
    fn eq(&self, other: &Self) -> bool {
        self.folded == other.folded
    }
}

impl Eq for NameKey {}

impl Hash for NameKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.folded.hash(state);
    }
}

impl PartialOrd for NameKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NameKey {
    /// Orders by the folded form only. Tie-breaking on `original` would make two
    /// `Eq` keys compare as `Less`/`Greater`, violating the `Ord`/`Eq` consistency
    /// contract that `BTreeMap` and `sort` rely on.
    fn cmp(&self, other: &Self) -> Ordering {
        self.folded.cmp(&other.folded)
    }
}

impl fmt::Display for NameKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.original)
    }
}

impl From<&str> for NameKey {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for NameKey {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// An unresolved field reference as written in DAX or in a report binding.
///
/// Holds the *logical* name: quote-unescaping (`''` → `'`) is the producer's job — the
/// DAX lexer or the PBIR parser — so `'Sales''s Data'[Amount]` arrives here as the table
/// name `Sales's Data`. [`Display`](fmt::Display) re-applies the escaping.
///
/// Unresolved means the reference has not yet been bound to an [`ObjectId`]: `[Total]`
/// could be a measure or a column of the current row context.
///
/// # Examples
///
/// ```
/// use ripbi_core::{FieldRef, NameKey};
///
/// let qualified = FieldRef {
///     table: Some(NameKey::new("Sales's Data")),
///     name: NameKey::new("Amount"),
/// };
/// // Display re-applies DAX quoting, doubling the internal quote.
/// assert_eq!(qualified.to_string(), "'Sales''s Data'[Amount]");
///
/// let unqualified = FieldRef { table: None, name: NameKey::new("Total") };
/// assert_eq!(unqualified.to_string(), "[Total]");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldRef {
    /// Qualifying table, if the reference was written qualified.
    /// `'Sales'[Amount]` → `Some("Sales")`; `[Total]` → `None`.
    pub table: Option<NameKey>,
    /// The column or measure name inside the square brackets.
    pub name: NameKey,
}

impl fmt::Display for FieldRef {
    /// Emits valid DAX. Table names are always single-quoted — quoting is optional in
    /// DAX only for names without spaces or punctuation, so quoting unconditionally is
    /// always correct. The bracketed part is not escaped: `]` cannot appear in an
    /// Analysis Services object name, so there is nothing to escape.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(table) = &self.table {
            write!(f, "{}", Quoted(table.as_str()))?;
        }
        write!(f, "[{}]", self.name.as_str())
    }
}

/// Stable, case-insensitive identity of a model or report object — the node key
/// of the dependency graph.
///
/// Every name is a [`NameKey`], so two `ObjectId`s that differ only in casing are the
/// same node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectId {
    /// A table.
    Table {
        /// Table name.
        table: NameKey,
    },
    /// A column, identified by its owning table.
    Column {
        /// Owning table.
        table: NameKey,
        /// Column name.
        column: NameKey,
    },
    /// A measure. The home table is part of the identity for display purposes only;
    /// the engine guarantees measure names are unique across the whole model.
    Measure {
        /// Home table.
        table: NameKey,
        /// Measure name.
        measure: NameKey,
    },
    /// A hierarchy defined on a table.
    Hierarchy {
        /// Owning table.
        table: NameKey,
        /// Hierarchy name.
        hierarchy: NameKey,
    },
    /// A partition (Power Query / M source) of a table.
    Partition {
        /// Owning table.
        table: NameKey,
        /// Partition name.
        partition: NameKey,
    },
    /// A security role.
    Role {
        /// Role name.
        role: NameKey,
    },
    /// An item of a calculation group, identified by the calculation group's table.
    CalculationItem {
        /// Calculation group table.
        table: NameKey,
        /// Calculation item name.
        item: NameKey,
    },
    /// A shared model-level M expression (e.g. a parameter or a shared query).
    Expression {
        /// Expression name.
        name: NameKey,
    },
    /// A user-defined DAX function (TOM function). Names are model-global.
    Function {
        /// Function name.
        name: NameKey,
    },
    /// A report-level measure (reportExtensions.json). Lives in the report, not the
    /// model, so it does not share the model's measure namespace: a distinct variant
    /// avoids ever conflating the two.
    ReportMeasure {
        /// Report measure name; unique within its report.
        measure: NameKey,
    },
}

impl fmt::Display for ObjectId {
    /// Human-readable form for diagnostics. Quoted names use the same `''` escaping
    /// as [`FieldRef`]; bracketed names are unescaped for the same reason.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectId::Table { table } => {
                write!(f, "table {}", Quoted(table.as_str()))
            }
            ObjectId::Column { table, column } => {
                write!(f, "{}[{}]", Quoted(table.as_str()), column.as_str())
            }
            ObjectId::Measure { table, measure } => {
                write!(f, "{}[{}]", Quoted(table.as_str()), measure.as_str())
            }
            ObjectId::Hierarchy { table, hierarchy } => {
                write!(
                    f,
                    "hierarchy {}[{}]",
                    Quoted(table.as_str()),
                    hierarchy.as_str()
                )
            }
            ObjectId::Partition { table, partition } => {
                write!(
                    f,
                    "partition {}[{}]",
                    Quoted(table.as_str()),
                    partition.as_str()
                )
            }
            ObjectId::Role { role } => {
                write!(f, "role {}", Quoted(role.as_str()))
            }
            ObjectId::CalculationItem { table, item } => {
                write!(
                    f,
                    "calculation item {}[{}]",
                    Quoted(table.as_str()),
                    item.as_str()
                )
            }
            ObjectId::Expression { name } => {
                write!(f, "expression {}", Quoted(name.as_str()))
            }
            ObjectId::Function { name } => {
                write!(f, "function {}", Quoted(name.as_str()))
            }
            ObjectId::ReportMeasure { measure } => {
                write!(f, "report measure {}", Quoted(measure.as_str()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::collections::HashSet;

    fn column(table: &str, column: &str) -> ObjectId {
        ObjectId::Column {
            table: NameKey::new(table),
            column: NameKey::new(column),
        }
    }

    fn qualified(table: &str, name: &str) -> FieldRef {
        FieldRef {
            table: Some(NameKey::new(table)),
            name: NameKey::new(name),
        }
    }

    fn unqualified(name: &str) -> FieldRef {
        FieldRef {
            table: None,
            name: NameKey::new(name),
        }
    }

    mod fold_name {
        use super::*;

        #[rstest]
        #[case::ascii("SaLeS", "sales")]
        #[case::danish_a_ring("MÅNED", "måned")]
        #[case::danish_ae_and_o_slash("ÆRØ", "ærø")]
        fn lowercases(#[case] input: &str, #[case] expected: &str) {
            assert_eq!(fold_name(input), expected);
        }
    }

    mod name_key {
        use super::*;

        #[rstest]
        #[case::ascii_upper("Sales", "SALES")]
        #[case::ascii_lower("Sales", "sales")]
        #[case::danish_a_ring("MÅNED", "måned")]
        #[case::danish_ae_and_o_slash("Ærø", "ærø")]
        fn compares_equal_ignoring_case(#[case] left: &str, #[case] right: &str) {
            assert_eq!(NameKey::new(left), NameKey::new(right));
        }

        #[rstest]
        #[case::one_letter_apart("Sales", "Salez")]
        #[case::danish_suffix("Måned", "Måneder")]
        fn compares_unequal_when_letters_differ(#[case] left: &str, #[case] right: &str) {
            assert_ne!(NameKey::new(left), NameKey::new(right));
        }

        #[test]
        fn hashes_case_variants_into_one_entry() {
            let set = HashSet::from([NameKey::new("Sales"), NameKey::new("SALES")]);

            assert_eq!(set.len(), 1);
        }

        #[test]
        fn hashes_distinct_names_separately() {
            let set = HashSet::from([NameKey::new("Sales"), NameKey::new("Salez")]);

            assert_eq!(set.len(), 2);
        }

        #[rstest]
        #[case::mixed_case("sAlEs")]
        #[case::upper("SALES")]
        fn is_found_in_a_set_under_any_casing(#[case] probe: &str) {
            let set = HashSet::from([NameKey::new("Sales")]);

            assert!(
                set.contains(&NameKey::new(probe)),
                "{probe:?} should match the stored key \"Sales\""
            );
        }

        #[test]
        fn is_not_found_in_a_set_by_a_prefix() {
            let set = HashSet::from([NameKey::new("Sales")]);

            assert!(
                !set.contains(&NameKey::new("Sale")),
                "folding must not truncate: \"Sale\" is a different name"
            );
        }

        #[test]
        fn as_str_keeps_the_original_casing() {
            assert_eq!(NameKey::new("SaLeS").as_str(), "SaLeS");
        }

        #[test]
        fn display_keeps_the_original_casing() {
            assert_eq!(NameKey::new("SaLeS").to_string(), "SaLeS");
        }

        #[test]
        fn folded_is_the_lowercased_form() {
            assert_eq!(NameKey::new("SaLeS").folded(), "sales");
        }

        /// `Ord` must agree with `Eq`, or `BTreeMap` and `sort` misbehave: two keys
        /// that differ only in case have to compare `Equal`, never by their original
        /// spelling.
        #[rstest]
        #[case::case_variants_are_equal("ABC", "abc", Ordering::Equal)]
        #[case::earlier_letter_is_less("abc", "abd", Ordering::Less)]
        #[case::later_letter_is_greater("ABD", "abc", Ordering::Greater)]
        fn orders_by_folded_name(
            #[case] left: &str,
            #[case] right: &str,
            #[case] expected: Ordering,
        ) {
            assert_eq!(NameKey::new(left).cmp(&NameKey::new(right)), expected);
        }

        #[test]
        fn supports_comparison_operators() {
            assert!(
                NameKey::new("abc") < NameKey::new("abd"),
                "PartialOrd must follow Ord"
            );
        }
    }

    mod object_id {
        use super::*;

        #[test]
        fn compares_equal_ignoring_case() {
            assert_eq!(column("Sales", "Amount"), column("SALES", "AMOUNT"));
        }

        #[test]
        fn compares_unequal_when_a_name_differs() {
            assert_ne!(column("Sales", "Amount"), column("Sales", "Amount2"));
        }

        /// A column and a measure can share a name; the variant keeps them apart.
        #[test]
        fn distinguishes_variants_carrying_the_same_names() {
            let measure = ObjectId::Measure {
                table: NameKey::new("Sales"),
                measure: NameKey::new("Amount"),
            };

            assert_ne!(column("Sales", "Amount"), measure);
        }

        #[test]
        fn hashes_case_variants_into_one_entry() {
            let set = HashSet::from([column("Sales", "Amount"), column("SALES", "AMOUNT")]);

            assert_eq!(set.len(), 1);
        }

        #[test]
        fn hashes_distinct_columns_separately() {
            let set = HashSet::from([column("Sales", "Amount"), column("Sales", "Amount2")]);

            assert_eq!(set.len(), 2);
        }

        #[test]
        fn hashes_a_column_and_a_measure_separately() {
            let measure = ObjectId::Measure {
                table: NameKey::new("Sales"),
                measure: NameKey::new("Amount"),
            };
            let set = HashSet::from([column("Sales", "Amount"), measure]);

            assert_eq!(set.len(), 2);
        }
    }

    mod field_ref {
        use super::*;

        #[rstest]
        #[case::qualified(qualified("Sales", "Amount"), "'Sales'[Amount]")]
        #[case::internal_quote_is_doubled(
            qualified("Sales's Data", "Amount"),
            "'Sales''s Data'[Amount]"
        )]
        #[case::unqualified(unqualified("Total"), "[Total]")]
        fn displays_as_valid_dax(#[case] reference: FieldRef, #[case] expected: &str) {
            assert_eq!(reference.to_string(), expected);
        }

        #[test]
        fn compares_equal_ignoring_case() {
            assert_eq!(qualified("Sales", "Amount"), qualified("SALES", "AMOUNT"));
        }

        #[test]
        fn distinguishes_a_qualified_reference_from_an_unqualified_one() {
            assert_ne!(qualified("Sales", "Amount"), unqualified("Amount"));
        }
    }

    mod object_id_display {
        use super::*;

        #[rstest]
        #[case::table(ObjectId::Table { table: NameKey::new("Sales") }, "table 'Sales'")]
        #[case::column(column("Sales", "Amount"), "'Sales'[Amount]")]
        #[case::measure(
            ObjectId::Measure { table: NameKey::new("Sales"), measure: NameKey::new("Total") },
            "'Sales'[Total]"
        )]
        #[case::hierarchy(
            ObjectId::Hierarchy { table: NameKey::new("Date"), hierarchy: NameKey::new("Calendar") },
            "hierarchy 'Date'[Calendar]"
        )]
        #[case::partition(
            ObjectId::Partition {
                table: NameKey::new("Sales"),
                partition: NameKey::new("Sales-Part1"),
            },
            "partition 'Sales'[Sales-Part1]"
        )]
        #[case::role(ObjectId::Role { role: NameKey::new("Reader") }, "role 'Reader'")]
        #[case::calculation_item(
            ObjectId::CalculationItem {
                table: NameKey::new("Time Intelligence"),
                item: NameKey::new("YTD"),
            },
            "calculation item 'Time Intelligence'[YTD]"
        )]
        #[case::expression(
            ObjectId::Expression { name: NameKey::new("Param1") },
            "expression 'Param1'"
        )]
        #[case::function(
            ObjectId::Function { name: NameKey::new("Sales.Margin") },
            "function 'Sales.Margin'"
        )]
        #[case::report_measure(
            ObjectId::ReportMeasure { measure: NameKey::new("Growth %") },
            "report measure 'Growth %'"
        )]
        #[case::internal_quotes_are_doubled(
            column("Bob's 'Best' Data", "AmOuNt"),
            "'Bob''s ''Best'' Data'[AmOuNt]"
        )]
        #[case::quoted_name_keeps_its_casing(
            ObjectId::Table { table: NameKey::new("O'Brien") },
            "table 'O''Brien'"
        )]
        fn renders(#[case] id: ObjectId, #[case] expected: &str) {
            assert_eq!(id.to_string(), expected);
        }
    }
}
