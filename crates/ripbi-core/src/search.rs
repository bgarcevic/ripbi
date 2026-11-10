//! Text search over every expression a model and its reports carry: the DAX
//! of measures, calculated columns and tables, calculation items, RLS
//! filters, functions, and report-level measures, and the Power Query (M) of
//! partitions and shared expressions.
//!
//! The search is line-based, like `grep`: one [`SearchHit`] per matching line,
//! with the owner of the expression and where in it the line sits. What a
//! match *is* belongs to the caller — [`search`] takes a line matcher, and
//! [`literal`] is the case-insensitive substring one. Nothing is lexed:
//! comments and string literals are searched like any other text, because a
//! person looking for `TODO` or a connection string wants exactly those.

use std::ops::Range;

use crate::identity::ObjectId;
use crate::model::{DaxExpressionKind, TabularDatabase};
use crate::report::ReportModel;

/// The language an expression is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// DAX.
    Dax,
    /// Power Query (M).
    M,
}

impl Language {
    /// The machine label: `dax` or `m`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Language::Dax => "dax",
            Language::M => "m",
        }
    }
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The object owning the expression.
    pub owner: ObjectId,
    /// Position in the report slice for a report-level measure, whose name is
    /// unique only within its own report.
    pub report_index: Option<usize>,
    /// The expression's language.
    pub language: Language,
    /// Which of the owner's expressions matched: `expression`,
    /// `format_string`, `detail_rows`, `kpi_target`, …
    pub property: &'static str,
    /// 1-based line within the expression.
    pub line: usize,
    /// 1-based column of the match within the line, in characters.
    pub column: usize,
    /// The matching line, without its line ending.
    pub text: String,
    /// The first match's byte range within [`text`](SearchHit::text).
    pub span: Range<usize>,
}

/// Every line of every DAX (`dax`) and M (`m`) expression that `find`
/// matches, in model order then report order. `find` returns the byte range
/// of a line's first match.
#[must_use]
pub fn search(
    db: &TabularDatabase,
    reports: &[&ReportModel],
    dax: bool,
    m: bool,
    find: &dyn Fn(&str) -> Option<Range<usize>>,
) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut scan = |text: &str,
                    owner: &dyn Fn() -> ObjectId,
                    report_index: Option<usize>,
                    language: Language,
                    property: &'static str| {
        for (number, line) in text.lines().enumerate() {
            let Some(span) = find(line) else { continue };
            if span.start > span.end || span.end > line.len() || !line.is_char_boundary(span.start)
            {
                continue;
            }
            hits.push(SearchHit {
                owner: owner(),
                report_index,
                language,
                property,
                line: number + 1,
                column: line[..span.start].chars().count() + 1,
                text: line.to_string(),
                span,
            });
        }
    };
    if dax {
        for expression in db.dax_expressions() {
            scan(
                expression.text,
                &|| expression.owner.to_object_id(),
                None,
                Language::Dax,
                property(expression.kind),
            );
        }
    }
    if m {
        for expression in db.m_expressions() {
            scan(
                expression.text,
                &|| expression.owner.to_object_id(),
                None,
                Language::M,
                "expression",
            );
        }
    }
    if dax {
        for (index, report) in reports.iter().enumerate() {
            for expression in report.dax_expressions() {
                scan(
                    expression.text,
                    &|| expression.owner.to_object_id(),
                    Some(index),
                    Language::Dax,
                    property(expression.kind),
                );
            }
        }
    }
    hits
}

/// Which of its owner's expressions a DAX expression is, as a machine label.
fn property(kind: DaxExpressionKind) -> &'static str {
    use DaxExpressionKind as Kind;
    match kind {
        Kind::Measure
        | Kind::CalculatedColumn
        | Kind::CalculatedTable
        | Kind::CalculationItem
        | Kind::Function
        | Kind::ReportMeasure => "expression",
        Kind::MeasureFormatString
        | Kind::CalculationItemFormatString
        | Kind::ReportMeasureFormatString => "format_string",
        Kind::MeasureDetailRows | Kind::TableDetailRows => "detail_rows",
        Kind::KpiTarget => "kpi_target",
        Kind::KpiStatus => "kpi_status",
        Kind::KpiTrend => "kpi_trend",
        Kind::ChangeDetection => "change_detection",
        Kind::RlsFilter => "rls_filter",
        Kind::CalculationGroupNoSelection => "no_selection",
        Kind::CalculationGroupNoSelectionFormatString => "no_selection_format_string",
        Kind::CalculationGroupMultipleOrEmptySelection => "multiple_or_empty_selection",
        Kind::CalculationGroupMultipleOrEmptySelectionFormatString => {
            "multiple_or_empty_selection_format_string"
        }
    }
}

/// A case-insensitive substring matcher for [`search`]. An empty needle
/// matches nothing.
///
/// ```
/// use ripbi_core::search::literal;
///
/// let find = literal("calculate");
/// assert_eq!(find("  CALCULATE([Total])"), Some(2..11));
/// assert_eq!(find("SUM(Sales[Amount])"), None);
/// ```
pub fn literal(needle: &str) -> impl Fn(&str) -> Option<Range<usize>> + use<> {
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    move |line: &str| {
        if needle.is_empty() {
            return None;
        }
        // Each lowered character with the byte range of the source character
        // it came from, so a match maps back onto the line as written even
        // where lowering changes a character's length.
        let folded: Vec<(char, usize, usize)> = line
            .char_indices()
            .flat_map(|(start, ch)| {
                let end = start + ch.len_utf8();
                ch.to_lowercase().map(move |lower| (lower, start, end))
            })
            .collect();
        folded.windows(needle.len()).find_map(|window| {
            window
                .iter()
                .zip(&needle)
                .all(|((ch, _, _), wanted)| ch == wanted)
                .then(|| window[0].1..window[needle.len() - 1].2)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NameKey;
    use crate::model::{Measure, Partition, PartitionSource, SharedExpression, Table};
    use crate::report::ReportMeasure;

    fn model() -> TabularDatabase {
        TabularDatabase {
            tables: vec![Table {
                name: "Sales".to_string(),
                measures: vec![Measure {
                    name: "Total".to_string(),
                    expression: "VAR x = 1\nRETURN\n    CALCULATE(SUM(Sales[Amount])) -- TODO"
                        .to_string(),
                    format_string_expression: Some("\"0.0\" & [Todo unit]".to_string()),
                    ..Default::default()
                }],
                partitions: vec![Partition {
                    name: "p".to_string(),
                    source: PartitionSource::M {
                        expression: "let\n    Source = Sql.Database(\"srv\", \"db\")\nin Source"
                            .to_string(),
                    },
                }],
                ..Default::default()
            }],
            expressions: vec![SharedExpression {
                name: "Server".to_string(),
                expression: "\"srv\" meta [IsParameterQuery=true]".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_hit_names_the_owner_the_line_and_the_column() {
        let hits = search(&model(), &[], true, true, &literal("calculate"));
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.owner.to_string(), "'Sales'[Total]");
        assert_eq!((hit.line, hit.column), (3, 5));
        assert_eq!(hit.property, "expression");
        assert_eq!(&hit.text[hit.span.clone()], "CALCULATE");
    }

    #[test]
    fn every_expression_property_is_searched() {
        let hits = search(&model(), &[], true, true, &literal("todo"));
        let properties: Vec<&str> = hits.iter().map(|hit| hit.property).collect();
        assert_eq!(properties, ["expression", "format_string"]);
    }

    #[test]
    fn the_language_switches_narrow_the_search() {
        let db = model();
        let both = search(&db, &[], true, true, &literal("srv"));
        assert_eq!(both.len(), 2);
        assert!(both.iter().all(|hit| hit.language == Language::M));
        assert!(search(&db, &[], true, false, &literal("srv")).is_empty());
        assert!(search(&db, &[], false, true, &literal("calculate")).is_empty());
    }

    #[test]
    fn report_measures_carry_their_report() {
        let report = ReportModel {
            measures: vec![ReportMeasure {
                name: NameKey::new("Local"),
                expression: "CALCULATE([Total])".to_string(),
                format_string: None,
            }],
            ..Default::default()
        };
        let hits = search(
            &model(),
            &[&report, &report],
            true,
            false,
            &literal("[total]"),
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[1].report_index, Some(1));
    }

    #[test]
    fn literal_matching_folds_case_across_scripts() {
        let find = literal("måned");
        assert_eq!(find("SUM(Dato[MÅNED])"), Some(9..15));
        assert_eq!(literal("")("anything"), None);
        // `İ` lowers to two characters; the span still covers whole characters.
        let find = literal("i\u{307}x");
        assert_eq!(find("aİx"), Some(1..4));
    }
}
