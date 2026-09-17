//! Broken report bindings: written field references that resolve to nothing
//! in the model, and bindings that land on artifacts whose own expressions no
//! longer resolve (issue #60).
//!
//! The graph builder swallows resolution misses — "an unresolvable name is
//! data, not an error" (`docs/name-resolution.md`). This module is the second
//! reader of that same data: where the builder asks *what stays alive*, it
//! asks *did the written reference name anything at all*. A miss says the
//! visual is broken — the table is gone, or the column/measure/hierarchy is
//! gone — and every direction of the claim is conservative, mirroring the
//! liveness rule inverted: over-keeping does not apply here, under-claiming
//! does. A false "broken" is itself a breakage claim, so anything the
//! machinery might resolve is treated as resolved.

use std::collections::{HashMap, HashSet};

use crate::dax::{self, RawRef, unescape_name};
use crate::identity::{NameKey, ObjectId, fold_name};
use crate::model::index::ModelIndex;
use crate::model::{PartitionSource, Table, TabularDatabase};
use crate::report::{FieldTarget, ReportModel};

use super::builder::table_struct;
use super::provenance::BindingEdge;

/// Why one report binding is broken — the static approximation of the error
/// state the engine would render.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BrokenReason {
    /// The written qualifier names a table the model does not have.
    TableNotFound,
    /// The table exists but the written column does not.
    FieldNotFound,
    /// The written measure matches no report measure and no model measure.
    MeasureNotFound,
    /// The table exists but carries no such hierarchy, and no column
    /// variation resolves the reference.
    HierarchyNotFound,
    /// The hierarchy exists but the written level (or the column it drills
    /// through) does not.
    LevelNotFound,
    /// The binding resolves, but to an artifact whose own DAX binds at least
    /// one field reference to nothing — the visual inherits the artifact's
    /// error state.
    BoundArtifactBroken {
        /// The broken artifact the binding lands on.
        artifact: ObjectId,
    },
}

/// One report binding that is broken: where it lives, what it wrote, and why
/// it fails to resolve. A broken binding has no [`ObjectId`] of its own — the
/// written target plus the binding's provenance identify it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenBinding {
    /// The binding's site: report, page, visual, bookmark, layout, and what
    /// kind of binding it is.
    pub edge: BindingEdge,
    /// The written field reference that failed, as the report stated it.
    pub target: FieldTarget,
    /// Why the reference does not resolve.
    pub reason: BrokenReason,
}

/// The sort key of a broken binding: where the binding lives (report, page,
/// visual, bookmark, layout), then the written target, then the reason —
/// display strings, not object identity, since there is no [`ObjectId`] to
/// sort by.
type SortKey<'a> = (
    Option<&'a NameKey>,
    Option<&'a NameKey>,
    Option<&'a NameKey>,
    Option<&'a NameKey>,
    bool,
    String,
    &'a BrokenReason,
);

impl BrokenBinding {
    /// The deterministic order, per [`SortKey`].
    pub(super) fn sort_key(&self) -> SortKey<'_> {
        (
            self.edge.report.as_ref(),
            self.edge.page.as_ref(),
            self.edge.visual.as_ref(),
            self.edge.bookmark.as_ref(),
            self.edge.mobile,
            self.target.to_string(),
            &self.reason,
        )
    }
}

/// The artifacts — every model and report object owning DAX — whose
/// expressions bind at least one field reference to nothing: the static
/// approximation of the engine's error state. Key: the artifact. Value: the
/// written forms of its unresolved references, sorted and deduplicated.
///
/// Built-in function calls and bare table candidates are never breakage (the
/// lexer emits them conservatively), and a qualified reference naming a
/// hierarchy or calculation item resolves through the same extended
/// candidates the graph's liveness uses.
pub(super) fn broken_artifacts(
    db: &TabularDatabase,
    reports: &[&ReportModel],
    index: &ModelIndex,
) -> HashMap<ObjectId, Vec<String>> {
    let mut out: HashMap<ObjectId, Vec<String>> = HashMap::new();
    let mut scan = |text: &str,
                    home_table: Option<&str>,
                    owner: &ObjectId,
                    report_measures: &HashSet<String>| {
        // The names query time can introduce: extension columns named by a
        // string literal (`ADDCOLUMNS(t, "@Krav", …)`, `SELECTCOLUMNS(t,
        // "Ordning", …)`, `GROUPBY`, `ROW`, `DATATABLE`) and the table
        // constructor's fixed defaults (`{…}` names its single column
        // `Value`, row constructors `Value1`, `Value2`, …). Both are
        // lexically visible without scope analysis, and the
        // over-approximation is under-claim — the worst case is a typo that
        // coincides with a string in the same measure going unflagged.
        let mut query_time: HashSet<String> = dax::quoted_names(text)
            .into_iter()
            .map(|name| fold_name(name.as_ref()))
            .collect();
        query_time.extend(constructors::COLUMN_NAMES.map(str::to_string));
        for raw in dax::references(text) {
            if matches!(raw, RawRef::Field { .. })
                && !field_resolves(db, index, home_table, &raw, report_measures, &query_time)
            {
                let written = raw
                    .to_field_ref()
                    .expect("a field reference materializes")
                    .to_string();
                out.entry(owner.clone()).or_default().push(written);
            }
        }
    };
    for expression in db.dax_expressions() {
        let owner = expression.owner.to_object_id();
        scan(
            expression.text,
            expression.home_table,
            &owner,
            &HashSet::new(),
        );
    }
    for report in reports {
        // A report measure referencing a sibling report measure is the graph's
        // ordinary report-measure edge (`add_expression_edges`), not breakage.
        let report_measures: HashSet<String> = report
            .measures
            .iter()
            .map(|measure| fold_name(measure.name.as_str()))
            .collect();
        for expression in report.dax_expressions() {
            let owner = expression.owner.to_object_id();
            scan(
                expression.text,
                expression.home_table,
                &owner,
                &report_measures,
            );
        }
    }
    for refs in out.values_mut() {
        refs.sort();
        refs.dedup();
    }
    out
}

/// Whether one field reference resolves, for breakage purposes: the binder's
/// answer, plus everything the binder cannot know that the engine still
/// resolves — the extended candidates (a qualified reference may name a
/// hierarchy or a calculation item), a calculated table's lexical output
/// schema, the report's own measures for an unqualified name, `@`-prefixed
/// extension columns, and the query-time column names introduced in the same
/// expression. Deliberately *not* the qualifying-table fallback — a reference
/// whose field is missing on an existing declared table is exactly the
/// breakage this module exists to report.
fn field_resolves(
    db: &TabularDatabase,
    index: &ModelIndex,
    home_table: Option<&str>,
    raw: &RawRef<'_>,
    report_measures: &HashSet<String>,
    query_time: &HashSet<String>,
) -> bool {
    if !dax::bind(db, index, home_table, raw.clone()).is_unresolved() {
        return true;
    }
    let RawRef::Field { table, name, .. } = raw else {
        return false;
    };
    let folded = fold_name(unescape_name(name).as_ref());
    match table {
        // Query-time columns are table-less by construction, and the engine
        // resolves the shapes below without a model object to bind: an `@`
        // prefix is the convention that keeps extension-column names out of
        // the model's namespace, a report measure is reachable from its own
        // report's expressions, and a query-time name comes from this same
        // expression's string literals or constructor defaults.
        None => {
            name.starts_with('@')
                || report_measures.contains(&folded)
                || query_time.contains(&folded)
        }
        Some(table) => {
            let table = &unescape_name(table);
            named_hierarchy_or_item(db, index, table, &folded)
                || calculated_table_field_resolves(db, index, table, &folded)
        }
    }
}

/// The fixed column names of DAX table constructors: `{1, 2, 3}` names its
/// single column `Value`; `{(a, b), (c, d)}` names them `Value1`, `Value2`, ….
mod constructors {
    /// The folded defaults, widest set a realistic constructor needs.
    pub const COLUMN_NAMES: [&str; 11] = [
        "value", "value1", "value2", "value3", "value4", "value5", "value6", "value7", "value8",
        "value9", "value10",
    ];
}

/// Whether `table` carries a hierarchy or calculation item named `folded` —
/// the extended candidates the binder does not know, shared with the
/// builder's liveness policy.
pub(super) fn named_hierarchy_or_item(
    db: &TabularDatabase,
    index: &ModelIndex,
    table: &str,
    folded: &str,
) -> bool {
    let Some(t): Option<&Table> = table_struct(db, index, table) else {
        return false;
    };
    if t.hierarchies.iter().any(|h| fold_name(&h.name) == folded) {
        return true;
    }
    t.calculation_group.as_ref().is_some_and(|group| {
        group
            .items
            .iter()
            .any(|item| fold_name(&item.name) == folded)
    })
}

/// Whether `table` is a calculated table whose partition expression makes
/// `folded` lexically visible as a field name. A calculated table has no
/// schema of its own — its columns are whatever the expression returns — so
/// a qualified miss on it cannot be read as breakage the way a miss on a
/// declared table can. The lexical candidates stand in for the engine's
/// output schema: string literals (`DATATABLE` headers, `ADDCOLUMNS`/
/// `SELECTCOLUMNS` names), the constructor defaults, the columns written in
/// the expression, and every column of the tables it references (the
/// wrapped-table shape `FILTER`/`CALCULATETABLE` passes through). An
/// over-approximation in the safe direction — a rename out of the
/// expression's visible names still flags.
pub(super) fn calculated_table_field_resolves(
    db: &TabularDatabase,
    index: &ModelIndex,
    table: &str,
    folded: &str,
) -> bool {
    let Some(candidates) = calculated_table_candidates(db, index, table) else {
        return false;
    };
    candidates.contains(folded)
}

/// The folded field names a calculated table's partition expression makes
/// lexically visible, or `None` when the table has no calculated partition.
/// Only names that resolve are admitted: a stale reference inside the
/// expression is breakage the artifact pass reports, not a schema the
/// engine materializes.
fn calculated_table_candidates(
    db: &TabularDatabase,
    index: &ModelIndex,
    table: &str,
) -> Option<HashSet<String>> {
    let t = table_struct(db, index, table)?;
    let expression = t
        .partitions
        .iter()
        .find_map(|partition| match &partition.source {
            PartitionSource::Calculated { expression } => Some(expression.as_str()),
            _ => None,
        })?;
    let mut candidates: HashSet<String> = dax::quoted_names(expression)
        .into_iter()
        .map(|name| fold_name(name.as_ref()))
        .collect();
    candidates.extend(constructors::COLUMN_NAMES.map(str::to_string));
    for raw in dax::references(expression) {
        match &raw {
            RawRef::Field { table, name, .. } => {
                if let Some(qualifier) = table {
                    // A written `'X'[C]` admits X's whole column set — the
                    // shape `FILTER`/`VALUES` passes through. C itself is
                    // already among them when it exists; when it does not,
                    // the stale name is the artifact pass's breakage, not a
                    // candidate.
                    extend_table_columns(db, index, &unescape_name(qualifier), &mut candidates);
                } else if !dax::bind(db, index, None, raw.clone()).is_unresolved() {
                    // An unqualified name only joins the schema when it
                    // binds — a stale one must stay flaggable.
                    candidates.insert(fold_name(unescape_name(name).as_ref()));
                }
            }
            RawRef::Table { name, .. } => {
                extend_table_columns(db, index, &unescape_name(name), &mut candidates);
            }
            RawRef::Function { .. } => {}
        }
    }
    Some(candidates)
}

/// Adds every column of `table` to `candidates`, when the table exists.
fn extend_table_columns(
    db: &TabularDatabase,
    index: &ModelIndex,
    table: &str,
    candidates: &mut HashSet<String>,
) {
    if let Some(t) = table_struct(db, index, table) {
        candidates.extend(t.columns.iter().map(|column| fold_name(&column.name)));
    }
}

/// The base measure name of a KPI-style synthesized variant, when the name
/// carries one of the engine's suffixes. A KPI visual binds the goal, status,
/// and trend variants of its measure — written names the model does not carry
/// and that resolve to nothing, but that the engine materializes, so they are
/// resolved, not flagged (issue #60).
pub(super) fn kpi_variant_base(name: &str) -> Option<&str> {
    const SUFFIXES: [&str; 4] = ["goal", "status", "trend", "value"];
    let folded = fold_name(name);
    let suffix = SUFFIXES
        .into_iter()
        .filter_map(|suffix| folded.strip_suffix(suffix))
        .max_by_key(|rest| rest.len())?;
    let base = name[..suffix.len()].trim_end();
    (!base.is_empty()).then_some(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, Measure, Table};

    fn db() -> TabularDatabase {
        TabularDatabase {
            tables: vec![Table {
                name: "Sales".to_string(),
                columns: vec![
                    Column {
                        name: "Amount".to_string(),
                        ..Default::default()
                    },
                    Column {
                        name: "Region".to_string(),
                        ..Default::default()
                    },
                ],
                measures: vec![Measure {
                    name: "Total".to_string(),
                    expression: "SUM('Sales'[Amount])".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    mod kpi_variants {
        use super::*;

        #[test]
        fn the_engine_suffixes_strip_to_the_base_measure() {
            for (name, base) in [
                ("Total Goal", "Total"),
                ("Total Status", "Total"),
                ("Total Trend", "Total"),
                ("Total Value", "Total"),
                ("total goal", "total"),
            ] {
                assert_eq!(kpi_variant_base(name), Some(base), "{name}");
            }
        }

        #[test]
        fn names_without_a_suffix_and_bare_suffixes_do_not_strip() {
            assert_eq!(kpi_variant_base("Total"), None);
            assert_eq!(kpi_variant_base("Goal"), None, "no base left");
            assert_eq!(kpi_variant_base("Value"), None);
            assert_eq!(kpi_variant_base("Sales Value Growth"), None);
        }

        #[test]
        fn the_longest_suffix_wins_when_names_stack() {
            assert_eq!(kpi_variant_base("Sales Goal Value"), Some("Sales Goal"));
        }
    }

    mod artifact_pass {
        use super::*;

        #[test]
        fn a_measure_referencing_a_missing_column_is_broken() {
            let mut model = db();
            model.tables[0].measures.push(Measure {
                name: "Broken".to_string(),
                expression: "SUM('Sales'[Nope]) + [Total]".to_string(),
                ..Default::default()
            });
            let index = ModelIndex::build(&model);

            let broken = broken_artifacts(&model, &[], &index);

            let refs = &broken[&ObjectId::Measure {
                table: NameKey::new("Sales"),
                measure: NameKey::new("Broken"),
            }];
            assert_eq!(refs, &["'Sales'[Nope]".to_string()]);
        }

        #[test]
        fn builtins_tables_and_resolving_refs_are_not_breakage() {
            let mut model = db();
            model.tables[0].measures.push(Measure {
                name: "Fine".to_string(),
                expression: "COUNTROWS(Missing) + SUM('Sales'[Amount])".to_string(),
                ..Default::default()
            });
            let index = ModelIndex::build(&model);

            assert!(broken_artifacts(&model, &[], &index).is_empty());
        }

        /// A qualified reference naming a hierarchy resolves through the
        /// extended candidates (`ISINSCOPE('Date'[Calendar])`): the binder
        /// does not know hierarchies, so treating its miss as breakage would
        /// flag a healthy measure.
        #[test]
        fn a_hierarchy_reference_resolves() {
            let model = TabularDatabase {
                tables: vec![Table {
                    name: "Date".to_string(),
                    columns: vec![Column {
                        name: "Year".to_string(),
                        ..Default::default()
                    }],
                    hierarchies: vec![crate::model::Hierarchy {
                        name: "Calendar".to_string(),
                        levels: vec![crate::model::HierarchyLevel {
                            name: "Year".to_string(),
                            column: "Year".to_string(),
                        }],
                        is_hidden: false,
                    }],
                    measures: vec![Measure {
                        name: "In Scope".to_string(),
                        expression: "ISINSCOPE('Date'[Calendar])".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let index = ModelIndex::build(&model);

            assert!(broken_artifacts(&model, &[], &index).is_empty());
        }

        /// A reference whose table exists but whose field does not is exactly
        /// the breakage the qualifying-table fallback must not paper over.
        #[test]
        fn a_missing_field_on_a_live_table_is_breakage() {
            let mut model = db();
            model.tables[0].measures.push(Measure {
                name: "Stale".to_string(),
                expression: "SUM('Sales'[Color])".to_string(),
                ..Default::default()
            });
            let index = ModelIndex::build(&model);

            let broken = broken_artifacts(&model, &[], &index);
            assert_eq!(broken.len(), 1);
        }

        #[test]
        fn a_report_measure_body_is_scanned_too() {
            let model = db();
            let index = ModelIndex::build(&model);
            let mut report = ReportModel::default();
            report.measures.push(crate::report::ReportMeasure {
                name: NameKey::new("Local"),
                expression: "[Total] + [Gone]".to_string(),
                format_string: None,
            });

            let broken = broken_artifacts(&model, &[&report], &index);

            let refs = &broken[&ObjectId::ReportMeasure {
                measure: NameKey::new("Local"),
            }];
            assert_eq!(refs, &["[Gone]".to_string()]);
        }

        /// The kundechef-DB shape: a report measure built on sibling report
        /// measures. The graph keeps the siblings alive through exactly these
        /// unqualified names (`add_expression_edges`), so the breakage pass
        /// must not call them unresolvable.
        #[test]
        fn a_report_measure_referencing_a_sibling_resolves() {
            let model = db();
            let index = ModelIndex::build(&model);
            let mut report = ReportModel::default();
            report.measures.push(crate::report::ReportMeasure {
                name: NameKey::new("Outer"),
                expression: "DIVIDE([Inner], [Base], 0)".to_string(),
                format_string: None,
            });
            report.measures.push(crate::report::ReportMeasure {
                name: NameKey::new("Inner"),
                expression: "1".to_string(),
                format_string: None,
            });
            report.measures.push(crate::report::ReportMeasure {
                name: NameKey::new("Base"),
                expression: "2".to_string(),
                format_string: None,
            });

            let broken = broken_artifacts(&model, &[&report], &index);

            assert!(
                !broken.contains_key(&ObjectId::ReportMeasure {
                    measure: NameKey::new("Outer"),
                }),
                "the siblings resolve: {broken:?}"
            );
        }

        /// The SQLBI extension-column pattern: `ADDCOLUMNS` names a
        /// query-time column `"@Krav"` and the filter reads it back as
        /// `[@Krav]`. The engine materializes it; only the genuinely missing
        /// `'Sales'[Nope]` is breakage.
        #[test]
        fn an_extension_column_reference_resolves() {
            let mut model = db();
            model.tables[0].measures.push(Measure {
                name: "Kvalificerede".to_string(),
                expression: "COUNTROWS(FILTER(ADDCOLUMNS(VALUES('Sales'[Amount]), \"@Krav\", [Total]), [@Krav] > 0)) + SUM('Sales'[Nope])"
                    .to_string(),
                ..Default::default()
            });
            let index = ModelIndex::build(&model);

            let broken = broken_artifacts(&model, &[], &index);

            let refs = &broken[&ObjectId::Measure {
                table: NameKey::new("Sales"),
                measure: NameKey::new("Kvalificerede"),
            }];
            assert_eq!(refs, &["'Sales'[Nope]".to_string()]);
        }

        /// The table constructor's default column: `VAR Dele = { … }` names
        /// its column `Value`, and `[Value]` reads it back through the
        /// variable — unresolvable to the binder, resolved by the engine.
        #[test]
        fn a_constructor_value_column_resolves() {
            let mut model = db();
            model.tables[0].measures.push(Measure {
                name: "Dele".to_string(),
                expression: "VAR Dele = { \"a\", \"b\" } RETURN CONCATENATEX(FILTER(Dele, NOT ISBLANK([Value])), [Value], \" | \")"
                    .to_string(),
                ..Default::default()
            });
            let index = ModelIndex::build(&model);

            let broken = broken_artifacts(&model, &[], &index);
            assert!(
                !broken.contains_key(&ObjectId::Measure {
                    table: NameKey::new("Sales"),
                    measure: NameKey::new("Dele"),
                }),
                "the constructor column resolves: {broken:?}"
            );
        }

        /// A string-named extension column (`SELECTCOLUMNS`/`GROUPBY`) reads
        /// back by the same name it was introduced with.
        #[test]
        fn a_string_named_extension_column_resolves() {
            let mut model = db();
            model.tables[0].measures.push(Measure {
                name: "Ordninger".to_string(),
                expression: "COUNTROWS(FILTER(SELECTCOLUMNS('Sales', \"Ordning\", 'Sales'[Amount]), [Ordning] > 0))".to_string(),
                ..Default::default()
            });
            let index = ModelIndex::build(&model);

            assert!(broken_artifacts(&model, &[], &index).is_empty());
        }

        /// The Bosteder shape: a calculated table whose only schema is its
        /// `DATATABLE` headers, and a measure reading one of them. The header
        /// is lexically visible in the partition expression — engine
        /// materialized, not breakage.
        #[test]
        fn a_measure_on_a_calculated_table_reading_a_datatable_header_resolves() {
            let model = calculated_colaxis("Group");
            let index = ModelIndex::build(&model);

            assert!(broken_artifacts(&model, &[], &index).is_empty());
        }

        /// Rename the header and the measure's reference to the old name is
        /// real breakage — the case a blanket "calculated ⇒ resolved" rule
        /// would have gone silent on.
        #[test]
        fn a_measure_reading_a_renamed_datatable_header_still_flags() {
            let model = calculated_colaxis("Gruppe");
            let index = ModelIndex::build(&model);

            let broken = broken_artifacts(&model, &[], &index);
            let refs = &broken[&ObjectId::Measure {
                table: NameKey::new("Sales"),
                measure: NameKey::new("Label"),
            }];
            assert_eq!(
                refs,
                &["'ColAxis'[Group]".to_string()],
                "the old header name is no longer visible"
            );
        }

        /// The shared fixture: `ColAxis` as a calculated `DATATABLE` table
        /// with `header` as one of its column names, plus a measure reading
        /// `'ColAxis'[Group]` — `header` controls whether that reading
        /// resolves.
        fn calculated_colaxis(header: &str) -> TabularDatabase {
            TabularDatabase {
                tables: vec![
                    Table {
                        name: "ColAxis".to_string(),
                        partitions: vec![crate::model::Partition {
                            name: "ColAxis".to_string(),
                            source: crate::model::PartitionSource::Calculated {
                                expression: format!(
                                    "DATATABLE(\"{header}\", STRING, {{\"Outlook\"}})"
                                ),
                            },
                        }],
                        ..Default::default()
                    },
                    Table {
                        name: "Sales".to_string(),
                        columns: vec![Column {
                            name: "Amount".to_string(),
                            ..Default::default()
                        }],
                        measures: vec![Measure {
                            name: "Label".to_string(),
                            expression: "CONCATENATEX('ColAxis', 'ColAxis'[Group], \" | \")"
                                .to_string(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }
        }
    }

    /// The extended-candidate lookup answers only for hierarchies and
    /// calculation items — columns and measures are the binder's job, so a
    /// column name here is deliberately `false`.
    #[test]
    fn named_hierarchy_or_item_answers_like_the_builder() {
        let model = db();
        let index = ModelIndex::build(&model);
        assert!(
            !named_hierarchy_or_item(&model, &index, "Sales", "amount"),
            "a column is the binder's candidate, not an extended one"
        );
        assert!(!named_hierarchy_or_item(&model, &index, "Sales", "nope"));
        assert!(!named_hierarchy_or_item(&model, &index, "Ghost", "x"));
    }
}
