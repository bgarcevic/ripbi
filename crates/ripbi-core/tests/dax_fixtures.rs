//! Integration tests for DAX lexing and reference resolution: the golden
//! `Mini.SemanticModel` (every assertion hand-checked against its known DAX)
//! and the full sample corpus (real-world DAX must lex, extract, and bind
//! without ever failing).

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_core::dax;
use ripbi_core::identity::{NameKey, ObjectId};
use ripbi_core::ingest::semantic_model;
use ripbi_core::model::index::ModelIndex;
use ripbi_core::model::{DaxExpressionKind, DaxExpressionRef, TabularDatabase};

fn fixture(groups: &[&str]) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tmdl");
    for group in groups {
        path.push(group);
    }
    path
}

fn golden() -> TabularDatabase {
    semantic_model(&fixture(&["golden", "Mini.SemanticModel"]))
        .expect("golden fixture parses")
        .value
}

/// Binds every reference of one expression, returning the bindings.
fn bound<'a>(
    db: &TabularDatabase,
    index: &ModelIndex,
    expression: &DaxExpressionRef<'a>,
) -> Vec<ripbi_core::Binding<'a>> {
    dax::references(expression.text)
        .into_iter()
        .map(|raw| dax::bind(db, index, expression.home_table, raw))
        .collect()
}

#[test]
fn golden_calculated_column_binds_to_its_column() {
    let db = golden();
    let index = ModelIndex::build(&db);

    let expression = db
        .dax_expressions()
        .into_iter()
        .find(|expression| expression.kind == DaxExpressionKind::CalculatedColumn)
        .expect("the Mini model has one calculated column");
    assert_eq!(expression.text, "DIVIDE([Sales Amount], 10)");
    assert_eq!(expression.home_table, Some("Sales"));
    assert_eq!(
        expression.owner.to_object_id(),
        ObjectId::Column {
            table: NameKey::new("Sales"),
            column: NameKey::new("Margin %"),
        }
    );

    let bindings = bound(&db, &index, &expression);
    assert_eq!(bindings.len(), 2, "DIVIDE( plus [Sales Amount]");
    assert!(
        bindings[0].is_unresolved(),
        "DIVIDE is not a model function"
    );
    assert_eq!(
        bindings[1].targets(),
        [ObjectId::Column {
            table: NameKey::new("Sales"),
            column: NameKey::new("Sales Amount"),
        }]
    );
}

#[test]
fn golden_measures_bind_in_both_reference_forms() {
    let db = golden();
    let index = ModelIndex::build(&db);
    let expressions = db.dax_expressions();

    // The `Sales` measure: one built-in call plus a qualified column reference.
    let sales = expressions
        .iter()
        .find(|expression| {
            expression.kind == DaxExpressionKind::Measure
                && expression.text == "SUM('Sales'[Sales Amount])"
        })
        .expect("the Sales measure exists");
    let bindings = bound(&db, &index, sales);
    assert_eq!(bindings.len(), 2);
    assert_eq!(
        bindings[1].targets(),
        [ObjectId::Column {
            table: NameKey::new("Sales"),
            column: NameKey::new("Sales Amount"),
        }]
    );

    // `Growth %`: `[Sales]` is unqualified and resolves to the measure; the
    // model has no `Date` table, so `'Date'[Date]` stays unresolved — data,
    // not an error.
    let growth = expressions
        .iter()
        .find(|expression| {
            expression.kind == DaxExpressionKind::Measure && expression.text.contains("DATEADD")
        })
        .expect("the Growth % measure exists");
    let bindings = bound(&db, &index, growth);
    let measure_targets = bindings
        .iter()
        .filter(|binding| {
            binding.targets()
                == &[ObjectId::Measure {
                    table: NameKey::new("Sales"),
                    measure: NameKey::new("Sales"),
                }][..]
        })
        .count();
    assert_eq!(
        measure_targets, 2,
        "both [Sales] references keep the measure alive"
    );
    assert!(
        bindings.iter().any(|binding| binding.is_unresolved()
            && matches!(binding_targets(binding), Some(("Date", "Date")))),
        "'Date'[Date] has no table to resolve against and surfaces as data"
    );
}

/// The raw names of an unresolved binding's reference, if it is a qualified
/// field.
fn binding_targets<'a>(binding: &'a ripbi_core::Binding<'a>) -> Option<(&'a str, &'a str)> {
    match binding {
        ripbi_core::Binding::Unresolved {
            raw:
                ripbi_core::RawRef::Field {
                    table: Some(table),
                    name,
                    ..
                },
        } => Some((*table, *name)),
        _ => None,
    }
}

#[test]
fn golden_rls_filter_binds_its_table_and_column() {
    let db = golden();
    let index = ModelIndex::build(&db);

    let expression = db
        .dax_expressions()
        .into_iter()
        .find(|expression| expression.kind == DaxExpressionKind::RlsFilter)
        .expect("the Administrators role has one filter");
    assert_eq!(expression.text, "FILTER('Sales', [Sales Amount] > 0)");
    // The home table of an RLS filter is the filtered table, not the role.
    assert_eq!(expression.home_table, Some("Sales"));

    let bindings = bound(&db, &index, &expression);
    assert_eq!(bindings.len(), 3, "FILTER(, 'Sales', [Sales Amount]");
    assert_eq!(
        bindings[1].targets(),
        [ObjectId::Table {
            table: NameKey::new("Sales"),
        }]
    );
    assert_eq!(
        bindings[2].targets(),
        [ObjectId::Column {
            table: NameKey::new("Sales"),
            column: NameKey::new("Sales Amount"),
        }]
    );
}

#[test]
fn golden_format_strings_produce_no_references() {
    let db = golden();
    let format_strings = db
        .dax_expressions()
        .into_iter()
        .filter(|expression| {
            matches!(
                expression.kind,
                DaxExpressionKind::MeasureFormatString
                    | DaxExpressionKind::CalculationItemFormatString
                    | DaxExpressionKind::CalculationGroupNoSelectionFormatString
                    | DaxExpressionKind::CalculationGroupMultipleOrEmptySelectionFormatString
            )
        })
        .collect::<Vec<_>>();
    assert!(
        format_strings.len() >= 3,
        "the golden model carries several dynamic format strings"
    );
    for expression in format_strings {
        // Function calls (SELECTEDMEASUREFORMATSTRING) are fine — they resolve
        // to nothing — but a format string never names a column, table, or
        // measure.
        let object_refs = dax::references(expression.text)
            .into_iter()
            .filter(|found| !matches!(found, ripbi_core::RawRef::Function { .. }))
            .count();
        assert_eq!(
            object_refs, 0,
            "a format string must never yield object references: {expression:?}"
        );
    }
}

#[test]
fn every_golden_expression_lexes_and_binds() {
    let db = golden();
    let index = ModelIndex::build(&db);

    let expressions = db.dax_expressions();
    assert!(expressions.len() >= 15, "the golden model is rich in DAX");

    let mut bound_targets = 0usize;
    let mut unresolved = 0usize;
    for expression in &expressions {
        for binding in bound(&db, &index, expression) {
            if binding.is_unresolved() {
                unresolved += 1;
            } else {
                bound_targets += binding.targets().len();
            }
        }
    }
    assert!(
        bound_targets >= 10,
        "the golden model has many live references"
    );
    assert!(
        unresolved > 0,
        "built-in calls are expected to stay unresolved"
    );
}

#[test]
fn every_sample_lexes_and_binds_without_failure() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let mut samples: Vec<PathBuf> = fs::read_dir(dir)
        .expect("samples/ exists beside the workspace root")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".SemanticModel"))
        })
        .collect();
    samples.sort();
    assert!(samples.len() >= 8, "expected the full sample set");

    for sample in &samples {
        let db = semantic_model(sample)
            .unwrap_or_else(|error| panic!("{} failed to parse: {error}", sample.display()))
            .value;
        let index = ModelIndex::build(&db);

        let mut bound_targets = 0usize;
        for expression in db.dax_expressions() {
            for binding in dax::references(expression.text)
                .into_iter()
                .map(|raw| dax::bind(&db, &index, expression.home_table, raw))
            {
                bound_targets += binding.targets().len();
            }
        }
        assert!(
            bound_targets >= 10,
            "{} resolved only {bound_targets} references",
            sample.display()
        );
    }
}

#[test]
fn adventure_works_measure_binds_through_its_quoted_form() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let adventure_works = fs::read_dir(dir)
        .expect("samples/ exists")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("AdventureWorks") && name.ends_with(".SemanticModel")
                })
        })
        .expect("AdventureWorks sample present");

    let db = semantic_model(&adventure_works)
        .expect("AdventureWorks parses")
        .value;
    let index = ModelIndex::build(&db);

    let sales = db
        .tables
        .iter()
        .find(|table| table.name == "Sales")
        .expect("the Sales table exists")
        .measures
        .iter()
        .find(|measure| measure.name == "Sales")
        .expect("the Sales measure exists");
    assert_eq!(sales.expression, "SUM('Sales'[Sales Amount])");

    let bindings: Vec<_> = dax::references(&sales.expression)
        .into_iter()
        .map(|raw| dax::bind(&db, &index, Some("Sales"), raw))
        .collect();
    assert_eq!(bindings.len(), 2);
    assert_eq!(
        bindings[1].targets(),
        [ObjectId::Column {
            table: NameKey::new("Sales"),
            column: NameKey::new("Sales Amount"),
        }]
    );
}
