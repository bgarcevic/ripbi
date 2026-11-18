//! Duplicate measures: measures whose DAX is the same once layout is ignored.
//!
//! Two expressions are the same when their token streams match after
//! [`fingerprint`] drops comments and whitespace and folds what the engine
//! compares case-insensitively (function, table, column, measure, and
//! variable names). String literals, numbers, and operators compare as
//! written. Nothing is resolved against the model: `Sales[Amount]` and
//! `'Sales'[Amount]` match because they are the same written reference, while
//! `[Amount]` and `Sales[Amount]` do not.
//!
//! Like breakage, the claim under-claims: an expression too short to be worth
//! consolidating (a bare reference, a literal, `BLANK()`) is never a
//! duplicate.

use std::collections::HashMap;

use super::lexer::{TokenKind, tokenize};
use crate::identity::{ObjectId, fold_name};
use crate::model::{DaxExpressionKind, TabularDatabase};
use crate::report::ReportModel;

/// Expressions with fewer significant tokens are never duplicates: `[Total]`,
/// `Sales[Amount]`, `0`, `BLANK()`.
const MIN_TOKENS: usize = 4;

/// Separates tokens in a fingerprint; no DAX token contains it unescaped in a
/// way that could make two different streams join to the same text, because
/// every token is also prefixed with its kind tag.
const SEP: char = '\u{1f}';

/// One measure in a [`DuplicateGroup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateMember {
    /// The measure: a model measure or a report-level measure.
    pub id: ObjectId,
    /// Position in the report slice for a report-level measure, whose name is
    /// unique only within its own report.
    pub report_index: Option<usize>,
}

/// Two or more measures sharing one expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    /// The measures, model measures first in model order, then report
    /// measures in report order. Never fewer than two.
    pub members: Vec<DuplicateMember>,
    /// The shared expression as the first member wrote it.
    pub expression: String,
}

/// The layout-independent form of a DAX expression, and how many significant
/// tokens it has. Equal fingerprints mean the expressions differ only in
/// whitespace, comments, name casing, or table-name quoting.
///
/// ```
/// use ripbi_core::dax::fingerprint;
///
/// let (a, _) = fingerprint("SUM ( Sales[Amount] ) -- total");
/// let (b, tokens) = fingerprint("sum('sales'[AMOUNT])");
/// assert_eq!(a, b);
/// assert_eq!(tokens, 5);
/// ```
#[must_use]
pub fn fingerprint(expression: &str) -> (String, usize) {
    let mut out = String::with_capacity(expression.len());
    let mut count = 0;
    for token in tokenize(expression) {
        let tag = match token.kind {
            TokenKind::Eof | TokenKind::Comment => continue,
            // A quoted and a bare table name are the same reference.
            TokenKind::Identifier | TokenKind::QuotedTable => 'i',
            TokenKind::BracketName => 'b',
            TokenKind::String => 's',
            TokenKind::DateTime => 'd',
            TokenKind::Number => 'n',
            TokenKind::QueryParameter => 'q',
            _ => 'o',
        };
        count += 1;
        out.push(tag);
        match token.kind {
            TokenKind::QuotedTable => {
                let inner = token.text.strip_prefix('\'').unwrap_or(token.text);
                let inner = inner.strip_suffix('\'').unwrap_or(inner);
                out.push_str(&fold_name(&inner.replace("''", "'")));
            }
            TokenKind::Identifier
            | TokenKind::BracketName
            | TokenKind::QueryParameter
            | TokenKind::DateTime => out.push_str(&fold_name(token.text)),
            _ => out.push_str(token.text),
        }
        out.push(SEP);
    }
    (out, count)
}

/// Every group of measures — model and report-level — whose expressions share
/// a [`fingerprint`]. Groups are ordered by their first member's position;
/// expressions shorter than four tokens never group.
#[must_use]
pub fn duplicate_measures(db: &TabularDatabase, reports: &[&ReportModel]) -> Vec<DuplicateGroup> {
    let mut groups: Vec<DuplicateGroup> = Vec::new();
    let mut by_fingerprint: HashMap<String, usize> = HashMap::new();
    let mut add = |text: &str, id: ObjectId, report_index: Option<usize>| {
        let (print, tokens) = fingerprint(text);
        if tokens < MIN_TOKENS {
            return;
        }
        let member = DuplicateMember { id, report_index };
        match by_fingerprint.get(&print) {
            Some(&position) => groups[position].members.push(member),
            None => {
                by_fingerprint.insert(print, groups.len());
                groups.push(DuplicateGroup {
                    members: vec![member],
                    expression: text.to_string(),
                });
            }
        }
    };
    for expression in db.dax_expressions() {
        if expression.kind == DaxExpressionKind::Measure {
            add(expression.text, expression.owner.to_object_id(), None);
        }
    }
    for (index, report) in reports.iter().enumerate() {
        for expression in report.dax_expressions() {
            if expression.kind == DaxExpressionKind::ReportMeasure {
                add(
                    expression.text,
                    expression.owner.to_object_id(),
                    Some(index),
                );
            }
        }
    }
    groups.retain(|group| group.members.len() > 1);
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NameKey;
    use crate::model::{Measure, Table};
    use crate::report::ReportMeasure;
    use rstest::rstest;

    #[rstest]
    #[case::whitespace("SUM(Sales[Amount])", "SUM ( Sales[Amount] )\n")]
    #[case::comments("SUM(Sales[Amount])", "/* total */ SUM(Sales[Amount]) -- sum")]
    #[case::function_case("SUM(Sales[Amount])", "sum(Sales[Amount])")]
    #[case::name_case("SUM(Sales[Amount])", "SUM(SALES[amount])")]
    #[case::table_quoting("SUM(Sales[Amount])", "SUM('Sales'[Amount])")]
    #[case::variable_case("VAR x = 1 RETURN x + 1", "var X = 1 return X + 1")]
    fn layout_does_not_change_the_fingerprint(#[case] a: &str, #[case] b: &str) {
        assert_eq!(fingerprint(a).0, fingerprint(b).0);
    }

    #[rstest]
    #[case::string_case("IF([A] = \"x\", 1, 2)", "IF([A] = \"X\", 1, 2)")]
    #[case::number("[A] * 1", "[A] * 10")]
    #[case::operator("[A] + [B]", "[A] - [B]")]
    #[case::qualifier("SUM([Amount])", "SUM(Sales[Amount])")]
    #[case::string_is_not_a_name("F(\"Sales\")", "F(Sales)")]
    #[case::token_boundary("[A] + [B]", "[A + B]")]
    fn meaning_changes_the_fingerprint(#[case] a: &str, #[case] b: &str) {
        assert_ne!(fingerprint(a).0, fingerprint(b).0);
    }

    fn measure(name: &str, expression: &str) -> Measure {
        Measure {
            name: name.to_string(),
            expression: expression.to_string(),
            ..Default::default()
        }
    }

    fn model() -> TabularDatabase {
        TabularDatabase {
            tables: vec![
                Table {
                    name: "Sales".to_string(),
                    measures: vec![
                        measure("Total", "SUM(Sales[Amount])"),
                        measure("Alias", "[Total]"),
                        measure("Other alias", "[Total]"),
                        measure("Units", "SUM(Sales[Units])"),
                    ],
                    ..Default::default()
                },
                Table {
                    name: "KPIs".to_string(),
                    measures: vec![measure("Revenue", "sum ( 'Sales'[Amount] ) // same")],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn names(group: &DuplicateGroup) -> Vec<String> {
        group.members.iter().map(|m| m.id.to_string()).collect()
    }

    #[test]
    fn measures_sharing_an_expression_group_across_tables() {
        let groups = duplicate_measures(&model(), &[]);
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(names(&groups[0]), ["'Sales'[Total]", "'KPIs'[Revenue]"]);
        assert_eq!(groups[0].expression, "SUM(Sales[Amount])");
    }

    #[test]
    fn trivial_expressions_never_group() {
        let groups = duplicate_measures(&model(), &[]);
        assert!(
            groups
                .iter()
                .all(|group| !names(group).contains(&"'Sales'[Alias]".to_string()))
        );
    }

    #[test]
    fn a_report_measure_groups_with_the_model_measure_it_copies() {
        let report = ReportModel {
            measures: vec![ReportMeasure {
                name: NameKey::new("Local total"),
                expression: "SUM(Sales[Amount])".to_string(),
                format_string: None,
            }],
            ..Default::default()
        };
        let groups = duplicate_measures(&model(), &[&report]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members.len(), 3);
        assert_eq!(groups[0].members[2].report_index, Some(0));
    }
}
