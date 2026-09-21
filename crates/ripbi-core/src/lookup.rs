//! User-written object references resolved to stable [`ObjectId`]s — the shared
//! lookup every command that addresses a model object by name goes through
//! (`ripbi deps` today), so ambiguity rules and typo suggestions behave
//! identically everywhere.
//!
//! A reference may be written in any of the forms the model itself displays:
//! the full display form (`hierarchy 'Date'[Calendar]`), a member shorthand
//! (`'Sales'[Total Sales]`, `Sales[Total]`), a bare name (`Sales`), or an
//! unqualified field (`[Total]`). Ambiguity is never guessed away — a bare
//! name that matches both a table and a measure, or a bracketed name matching a
//! column and a measure, comes back as
//! [`ReferenceError::Ambiguous`] with every candidate for the caller to
//! present.

use std::fmt;

use crate::dax::{RawRef, references, unescape_name};
use crate::identity::{NameKey, ObjectId, fold_name};

/// Why a user-written reference resolved to no single object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceError {
    /// No object matches the reference.
    NotFound {
        /// The reference as written.
        input: String,
        /// The closest matching object, when one is close enough to suggest.
        /// Boxed to keep the error small enough to return by value.
        suggestion: Option<Box<ObjectId>>,
    },
    /// The reference matches several objects. The user must disambiguate —
    /// usually by qualifying the reference, or by writing the object's full
    /// display form.
    Ambiguous {
        /// The reference as written.
        input: String,
        /// Every matching object, sorted by object identity.
        candidates: Vec<ObjectId>,
    },
}

impl fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { input, .. } => write!(f, "object \"{input}\" was not found"),
            Self::Ambiguous { input, .. } => write!(f, "\"{input}\" matches multiple objects"),
        }
    }
}

/// Resolves one user-written reference against a set of objects — the graph's
/// nodes, or any other object listing. Exactly one object must match; see the
/// module docs for the accepted forms.
pub fn resolve_reference<'a>(
    objects: impl IntoIterator<Item = &'a ObjectId>,
    input: &str,
) -> Result<ObjectId, ReferenceError> {
    let objects: Vec<&ObjectId> = objects.into_iter().collect();
    let input = input.trim();

    // The display form is the universal escape hatch: every object's
    // `Display` — `table 'Sales'`, `relationship 'A'[X] -> 'B'[Y]`, … — is
    // matchable verbatim, covering the kinds no shorthand exists for.
    let folded_input = fold_name(input);
    let exact: Vec<ObjectId> = objects
        .iter()
        .filter(|id| fold_name(&id.to_string()) == folded_input)
        .map(|id| (*id).clone())
        .collect();
    match exact.as_slice() {
        [only] => return Ok(only.clone()),
        [_, _, ..] => {
            return Err(ambiguous(input, exact));
        }
        [] => {}
    }

    // Structured shorthands: member refs (`'T'[N]`, `[N]`) and bare names.
    let candidates = structured_candidates(&objects, input);
    match candidates.as_slice() {
        [only] => return Ok(only.clone()),
        [_, _, ..] => {
            return Err(ambiguous(input, candidates));
        }
        [] => {}
    }

    Err(ReferenceError::NotFound {
        input: input.to_string(),
        suggestion: suggest(&objects, input).map(Box::new),
    })
}

/// Packages a many-candidate outcome, with the candidates in the deterministic
/// order the caller should list them: object identity.
fn ambiguous(input: &str, mut candidates: Vec<ObjectId>) -> ReferenceError {
    candidates.sort();
    ReferenceError::Ambiguous {
        input: input.to_string(),
        candidates,
    }
}

/// The objects a structured shorthand form names. Zero refs (or several — the
/// user pasted an expression) match nothing.
fn structured_candidates(objects: &[&ObjectId], input: &str) -> Vec<ObjectId> {
    match references(input).as_slice() {
        [
            RawRef::Field {
                table: Some(table),
                name,
                ..
            },
        ] => members(objects, Some(&unescape_name(table)), name),
        [
            RawRef::Field {
                table: None, name, ..
            },
        ] => members(objects, None, name),
        // A bare name lives in the global namespaces: tables, measures,
        // shared expressions, and functions — the spec example's `"Sales"`
        // matching both the table and its same-named measure is exactly this.
        [RawRef::Table { name, .. }] => globals(objects, &unescape_name(name)),
        [RawRef::Function { name, .. }] => {
            named(objects, name, |id| matches!(id, ObjectId::Function { .. }))
        }
        _ => Vec::new(),
    }
}

/// Column, measure, hierarchy, and calculation-item identities matching a
/// (possibly unqualified) member reference. A qualified reference carries its
/// table; an unqualified one matches every table's member of that name, plus
/// report measures, which shadow model measures in report contexts.
fn members(objects: &[&ObjectId], table: Option<&str>, name: &str) -> Vec<ObjectId> {
    let in_table = |id_table: &NameKey| table.is_none_or(|t| id_table.folded() == fold_name(t));
    objects
        .iter()
        .filter(|id| match id {
            ObjectId::Column { table, column } => {
                in_table(table) && column.folded() == fold_name(name)
            }
            ObjectId::Measure { table, measure } => {
                in_table(table) && measure.folded() == fold_name(name)
            }
            ObjectId::Hierarchy { table, hierarchy } => {
                in_table(table) && hierarchy.folded() == fold_name(name)
            }
            ObjectId::CalculationItem { table, item } => {
                in_table(table) && item.folded() == fold_name(name)
            }
            ObjectId::ReportMeasure { measure } => {
                table.is_none() && measure.folded() == fold_name(name)
            }
            _ => false,
        })
        .map(|id| (*id).clone())
        .collect()
}

/// The globally-unique namespaces a bare identifier can name: tables, measures,
/// shared expressions, and user-defined functions.
fn globals(objects: &[&ObjectId], name: &str) -> Vec<ObjectId> {
    named(objects, name, |id| {
        matches!(
            id,
            ObjectId::Table { .. }
                | ObjectId::Measure { .. }
                | ObjectId::Expression { .. }
                | ObjectId::Function { .. }
        )
    })
}

/// Objects of kinds `of_kind` whose single name matches `name`.
fn named(objects: &[&ObjectId], name: &str, of_kind: impl Fn(&ObjectId) -> bool) -> Vec<ObjectId> {
    objects
        .iter()
        .filter(|id| of_kind(id) && single_name(id).is_some_and(|n| n.folded() == fold_name(name)))
        .map(|id| (*id).clone())
        .collect()
}

/// The one name an object is known by, for the kinds that have exactly one.
fn single_name(id: &ObjectId) -> Option<&NameKey> {
    match id {
        ObjectId::Table { table } => Some(table),
        ObjectId::Measure { measure, .. } => Some(measure),
        ObjectId::Expression { name } => Some(name),
        ObjectId::Function { name } => Some(name),
        ObjectId::Role { role } => Some(role),
        ObjectId::ReportMeasure { measure } => Some(measure),
        ObjectId::Column { column, .. } => Some(column),
        ObjectId::Hierarchy { hierarchy, .. } => Some(hierarchy),
        ObjectId::CalculationItem { item, .. } => Some(item),
        ObjectId::Partition { partition, .. } => Some(partition),
        ObjectId::Relationship { .. } => None,
    }
}

/// The closest match worth suggesting for a reference that matched nothing —
/// the object whose written form is nearest by edit distance, under a
/// threshold loose enough for one transposition on a long name and tight
/// enough that nothing absurd appears for gibberish.
fn suggest(objects: &[&ObjectId], input: &str) -> Option<ObjectId> {
    let folded_input = fold_name(input);
    // Loose for long inputs (one wrong word can cost several edits), never
    // tighter than two, so a one-letter typo on a short name still suggests.
    let threshold = (folded_input.len() / 4).max(2);
    let mut best: Option<(usize, &ObjectId)> = None;
    for id in objects.iter().copied() {
        for key in match_keys(id) {
            let distance = edit_distance(&fold_name(&key), &folded_input);
            if distance > threshold {
                continue;
            }
            // Ties go to the smaller object identity, keeping the choice
            // deterministic.
            let better = match best {
                None => true,
                Some((best_distance, best_id)) => {
                    distance < best_distance || (distance == best_distance && id < best_id)
                }
            };
            if better {
                best = Some((distance, id));
            }
        }
    }
    best.map(|(_, id)| id.clone())
}

/// Every written form an object is addressable by: its full display form, its
/// shorthands, and its bare name.
fn match_keys(id: &ObjectId) -> Vec<String> {
    let mut keys = vec![id.to_string()];
    match id {
        ObjectId::Table { table } => {
            keys.push(table.as_str().to_string());
        }
        ObjectId::Column { table, column }
        | ObjectId::Measure {
            table,
            measure: column,
        }
        | ObjectId::Hierarchy {
            table,
            hierarchy: column,
        }
        | ObjectId::CalculationItem {
            table,
            item: column,
        } => {
            keys.push(format!("{}[{}]", table.quoted(), column.as_str()));
            keys.push(column.as_str().to_string());
        }
        ObjectId::Role { role }
        | ObjectId::Expression { name: role }
        | ObjectId::Function { name: role } => {
            keys.push(role.as_str().to_string());
        }
        ObjectId::ReportMeasure { measure } => {
            keys.push(format!("[{}]", measure.as_str()));
            keys.push(measure.as_str().to_string());
        }
        ObjectId::Partition { .. } | ObjectId::Relationship { .. } => {}
    }
    keys
}

/// Damerau–Levenshtein distance with the optimal-string-alignment
/// restriction — adjacent transpositions count as one edit, which is what a
/// typo usually is.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut two_back: Vec<usize> = Vec::new();
    let mut current: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        current[0] = i;
        for j in 1..=b.len() {
            let substitution = usize::from(a[i - 1] != b[j - 1]);
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + substitution);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                current[j] = current[j].min(two_back[j - 2] + 1);
            }
        }
        two_back = previous.clone();
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

impl crate::graph::DependencyGraph {
    /// Resolves a user-written reference against this graph's objects — the
    /// entry commands take, so ambiguity and suggestions always reflect the
    /// graph a command is about to traverse. See
    /// [`resolve_reference`](self) is the free-function form over any object
    /// listing; see its docs for the accepted forms.
    pub fn resolve_reference(&self, input: &str) -> Result<ObjectId, ReferenceError> {
        resolve_reference(self.object_ids(), input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(table: &str, name: &str) -> ObjectId {
        ObjectId::Column {
            table: NameKey::new(table),
            column: NameKey::new(name),
        }
    }

    fn measure(table: &str, name: &str) -> ObjectId {
        ObjectId::Measure {
            table: NameKey::new(table),
            measure: NameKey::new(name),
        }
    }

    fn table(name: &str) -> ObjectId {
        ObjectId::Table {
            table: NameKey::new(name),
        }
    }

    fn hierarchy(table: &str, name: &str) -> ObjectId {
        ObjectId::Hierarchy {
            table: NameKey::new(table),
            hierarchy: NameKey::new(name),
        }
    }

    fn calculation_item(table: &str, name: &str) -> ObjectId {
        ObjectId::CalculationItem {
            table: NameKey::new(table),
            item: NameKey::new(name),
        }
    }

    fn objects() -> Vec<ObjectId> {
        vec![
            table("Sales"),
            column("Sales", "Amount"),
            column("Sales", "Key"),
            measure("Sales", "Total Sales"),
            measure("Sales", "Sales"),
            table("Exchange Rates"),
            column("Exchange Rates", "Rate"),
            hierarchy("Date", "Calendar"),
            calculation_item("Time Intelligence", "YTD"),
            ObjectId::Expression {
                name: NameKey::new("Param1"),
            },
            ObjectId::Function {
                name: NameKey::new("MyFunc"),
            },
            ObjectId::ReportMeasure {
                measure: NameKey::new("Local Total"),
            },
        ]
    }

    fn resolve(input: &str) -> Result<ObjectId, ReferenceError> {
        let ids = objects();
        resolve_reference(ids.iter(), input)
    }

    mod accepted_forms {
        use super::*;

        #[test]
        fn the_display_form_matches_verbatim() {
            assert_eq!(
                resolve("'Sales'[Amount]").unwrap(),
                column("Sales", "Amount")
            );
            assert_eq!(
                resolve("hierarchy 'Date'[Calendar]").unwrap(),
                hierarchy("Date", "Calendar")
            );
            assert_eq!(resolve("table 'Sales'").unwrap(), table("Sales"));
        }

        #[test]
        fn a_member_shorthand_matches_any_member_kind_in_its_table() {
            assert_eq!(
                resolve("'Sales'[Amount]").unwrap(),
                column("Sales", "Amount")
            );
            assert_eq!(
                resolve("Sales[Total Sales]").unwrap(),
                measure("Sales", "Total Sales")
            );
            assert_eq!(
                resolve("'Date'[Calendar]").unwrap(),
                hierarchy("Date", "Calendar")
            );
            assert_eq!(
                resolve("'Time Intelligence'[YTD]").unwrap(),
                calculation_item("Time Intelligence", "YTD")
            );
        }

        #[test]
        fn lookup_is_case_insensitive() {
            assert_eq!(
                resolve("'SALES'[amount]").unwrap(),
                column("Sales", "Amount")
            );
        }

        #[test]
        fn a_bare_table_name_resolves_to_the_table() {
            // No same-named member here: `Sales` names exactly one object.
            let ids = [table("Sales"), column("Sales", "Amount")];

            assert_eq!(
                resolve_reference(ids.iter(), "Sales").unwrap(),
                table("Sales")
            );
            assert_eq!(
                resolve("'Exchange Rates'").unwrap(),
                table("Exchange Rates")
            );
        }

        #[test]
        fn a_bare_global_measure_name_is_ambiguous_with_its_table() {
            // The issue's example: "Sales" is both the table and the
            // same-named measure — never guessed, always listed.
            let error = resolve("Sales").unwrap_err();

            let ReferenceError::Ambiguous { candidates, .. } = error else {
                panic!("expected ambiguity");
            };
            assert_eq!(candidates, vec![table("Sales"), measure("Sales", "Sales")]);
        }

        #[test]
        fn a_bare_shared_expression_or_function_name_resolves() {
            assert_eq!(
                resolve("Param1").unwrap(),
                ObjectId::Expression {
                    name: NameKey::new("Param1")
                }
            );
            assert_eq!(
                resolve("MyFunc").unwrap(),
                ObjectId::Function {
                    name: NameKey::new("MyFunc")
                }
            );
        }

        #[test]
        fn an_unqualified_bracketed_name_matches_every_table() {
            let ids = [
                table("Sales"),
                column("Sales", "Key"),
                table("Dato"),
                column("Dato", "Key"),
                measure("Sales", "Key"),
            ];
            let error = resolve_reference(ids.iter(), "[Key]").unwrap_err();

            let ReferenceError::Ambiguous { candidates, .. } = error else {
                panic!("expected ambiguity");
            };
            // Sorted by object identity: `Dato` folds before `Sales`, and
            // columns sort before the measure (variant order).
            assert_eq!(
                candidates,
                vec![
                    column("Dato", "Key"),
                    column("Sales", "Key"),
                    measure("Sales", "Key"),
                ]
            );
        }

        #[test]
        fn an_unqualified_bracketed_name_can_resolve_to_a_report_measure() {
            assert_eq!(
                resolve("[Local Total]").unwrap(),
                ObjectId::ReportMeasure {
                    measure: NameKey::new("Local Total")
                }
            );
        }

        #[test]
        fn a_doubled_quote_in_the_table_name_is_unescaped() {
            let ids = [column("Sales's Data", "Amount")];

            let resolved = resolve_reference(ids.iter(), "'Sales''s Data'[Amount]").unwrap();

            assert_eq!(resolved, column("Sales's Data", "Amount"));
        }

        #[test]
        fn a_function_call_resolves_the_function() {
            assert_eq!(
                resolve("MyFunc(").unwrap(),
                ObjectId::Function {
                    name: NameKey::new("MyFunc")
                }
            );
        }

        #[test]
        fn a_qualified_reference_does_not_cross_tables() {
            // `Total Sales` is a measure on `Sales`; another table's qualifier
            // must not adopt it — the user asked for a member of `Dato`.
            let ids = [table("Dato"), measure("Sales", "Total Sales")];

            assert!(matches!(
                resolve_reference(ids.iter(), "'Dato'[Total Sales]"),
                Err(ReferenceError::NotFound { .. })
            ));
        }
    }

    mod ambiguity {
        use super::*;

        #[test]
        fn a_column_and_measure_sharing_a_name_are_listed() {
            let ids = [column("Sales", "Total"), measure("Sales", "Total")];

            let error = resolve_reference(ids.iter(), "'Sales'[Total]").unwrap_err();

            let ReferenceError::Ambiguous { candidates, input } = error else {
                panic!("expected ambiguity");
            };
            assert_eq!(input, "'Sales'[Total]");
            assert_eq!(
                candidates,
                vec![column("Sales", "Total"), measure("Sales", "Total")]
            );
        }

        #[test]
        fn an_expression_pasted_as_a_reference_matches_nothing() {
            assert!(matches!(
                resolve("[Amount] + [Rate]"),
                Err(ReferenceError::NotFound {
                    suggestion: None,
                    ..
                })
            ));
        }
    }

    mod suggestions {
        use super::*;

        #[test]
        fn a_transposed_member_name_suggests_the_real_one() {
            let error = resolve("'Sales'[Totla Sales]").unwrap_err();

            let ReferenceError::NotFound { suggestion, .. } = error else {
                panic!("expected not-found");
            };
            assert_eq!(suggestion, Some(Box::new(measure("Sales", "Total Sales"))));
        }

        #[test]
        fn a_short_table_typo_suggests_the_table() {
            let error = resolve("Sals").unwrap_err();

            let ReferenceError::NotFound { suggestion, .. } = error else {
                panic!("expected not-found");
            };
            assert_eq!(suggestion, Some(Box::new(table("Sales"))));
        }

        #[test]
        fn gibberish_suggests_nothing() {
            let error = resolve("zzzzqqqq").unwrap_err();

            let ReferenceError::NotFound { suggestion, .. } = error else {
                panic!("expected not-found");
            };
            assert_eq!(suggestion, None);
        }

        #[test]
        fn an_unterminated_bracket_still_resolves() {
            // The lexer yields the reference it can from `'sales'[TOTAL SALES`
            // — folded comparison resolves it like any other casing.
            assert_eq!(
                resolve("'sales'[TOTAL SALES").unwrap(),
                measure("Sales", "Total Sales")
            );
        }
    }

    mod distance {
        use super::*;

        #[test]
        fn counts_insertions_deletions_and_substitutions() {
            assert_eq!(edit_distance("kitten", "sitting"), 3);
            assert_eq!(edit_distance("", "abc"), 3);
            assert_eq!(edit_distance("abc", ""), 3);
            assert_eq!(edit_distance("same", "same"), 0);
        }

        #[test]
        fn counts_an_adjacent_transposition_as_one_edit() {
            assert_eq!(edit_distance("totla", "total"), 1);
            assert_eq!(edit_distance("abcd", "badc"), 2);
        }
    }

    #[test]
    fn empty_input_is_not_found() {
        assert!(matches!(
            resolve("   "),
            Err(ReferenceError::NotFound {
                suggestion: None,
                ..
            })
        ));
    }
}
