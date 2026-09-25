//! The PBIT was exported from Microsoft's public 2026 AdventureWorks PBIX;
//! `Revenue Opportunities.pbix` is Microsoft's public PBIX itself.
//!
//! `microsoft_desktop_samples_corpus` is opt-in: point
//! `RIPBI_PBI_DESKTOP_SAMPLES` at a local clone of
//! <https://github.com/microsoft/powerbi-desktop-samples> to run it.

use std::path::{Path, PathBuf};

use ripbi_core::TabularDatabase;
use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest::{report, semantic_model};

fn samples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples")
}

#[test]
fn adventureworks_pbit_matches_its_pbip_conversion() {
    let root = samples();
    let archive = root.join("AdventureWorks Sales.pbit");
    let tmdl = semantic_model(&root.join("AdventureWorks Sales.SemanticModel")).unwrap();
    let tmsl = semantic_model(&archive).unwrap();
    let pbir = report(&root.join("AdventureWorks Sales.Report")).unwrap();
    let archived_report = report(&archive).unwrap();

    assert!(tmsl.skips.is_empty(), "{:#?}", tmsl.skips);
    assert!(
        archived_report.skips.is_empty(),
        "{:#?}",
        archived_report.skips
    );
    assert_eq!(tmsl.value.tables.len(), tmdl.value.tables.len());
    assert_eq!(
        archived_report.value.bindings().len(),
        pbir.value.bindings().len()
    );

    let model_m = tmdl.value.m_expressions();
    let archive_m = tmsl.value.m_expressions();
    assert_eq!(archive_m.len(), model_m.len());
    let mut expected_m: Vec<_> = model_m
        .iter()
        .map(|item| (item.owner.to_object_id(), item.text))
        .collect();
    let mut actual_m: Vec<_> = archive_m
        .iter()
        .map(|item| (item.owner.to_object_id(), item.text))
        .collect();
    expected_m.sort();
    actual_m.sort();
    assert_eq!(actual_m, expected_m);

    let original = DependencyGraph::build(&tmdl.value, &[&pbir.value]);
    let exported = DependencyGraph::build(&tmsl.value, &[&archived_report.value]);
    assert_eq!(exported.unused_objects(), original.unused_objects());
}

/// Microsoft's public `Revenue Opportunities.pbix` (2026 samples revamp)
/// carries only a compressed ABF `DataModel`; its PBIP conversion sits beside
/// it. Both must normalize to the same model and the same unused set.
#[test]
fn revenue_opportunities_pbix_data_model_matches_its_pbip_conversion() {
    let root = samples();
    assert_pbix_matches_pbip(
        &root.join("Revenue Opportunities.pbix"),
        &root.join("Revenue Opportunities.SemanticModel"),
        &root.join("Revenue Opportunities.Report"),
    );
}

/// `Revenue Opportunities Features` is the same sample after adding, in Power
/// BI Desktop, what the public samples lack: a calculation group with
/// selection expressions, RLS and metadata-permission roles, a DAX function
/// (compatibility level 1702), a KPI, a dynamic format string, detail rows,
/// and an M parameter. Desktop saved the PBIX and the PBIP from one session,
/// so the two models must agree field for field, not just by identity.
#[test]
fn revenue_opportunities_features_pbix_matches_its_pbip_save() {
    let root = samples();
    let archive = root.join("Revenue Opportunities Features.pbix");
    let model = root.join("Revenue Opportunities Features.SemanticModel");
    assert_pbix_matches_pbip(
        &archive,
        &model,
        &root.join("Revenue Opportunities Features.Report"),
    );
    let abf = semantic_model(&archive).unwrap().value;
    let mut tmdl = semantic_model(&model).unwrap().value;
    // A PBIP names the model after its folder; the PBIX catalog does not.
    tmdl.name = None;
    assert_eq!(abf, tmdl);

    // Guard against both sides silently dropping what the fixture exists for.
    let table = |name: &str| abf.tables.iter().find(|table| table.name == name).unwrap();
    let group = table("Time Intelligence")
        .calculation_group
        .as_ref()
        .unwrap();
    let items: Vec<_> = group.items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(items, ["Current", "YTD", "PY"]);
    assert_eq!(
        group.multiple_or_empty_selection_expression.as_deref(),
        Some("BLANK()")
    );
    assert_eq!(
        group.no_selection_expression.as_deref(),
        Some("SELECTEDMEASURE()")
    );
    let roles: Vec<_> = abf.roles.iter().map(|role| role.name.as_str()).collect();
    assert_eq!(roles, ["East Region", "No Partner Metadata"]);
    assert!(
        abf.roles[0].table_permissions[0]
            .filter_expression
            .is_some()
    );
    assert_eq!(abf.functions.len(), 1);
    assert_eq!(abf.expressions.len(), 1);
    let measure = |name: &str| {
        table("Calculations")
            .measures
            .iter()
            .find(|measure| measure.name == name)
            .unwrap()
    };
    assert!(measure("Revenue KPI").kpi.is_some());
    let dynamic = measure("Revenue Dynamic");
    assert!(dynamic.format_string_expression.is_some());
    assert!(dynamic.detail_rows_expression.is_some());
}

/// Sample pairs whose committed PBIP legitimately differs from the public
/// PBIX, and why.
const KNOWN_DIVERGENT: &[(&str, &str)] = &[
    (
        "Artificial Intelligence Sample",
        "the PBIP was converted from an earlier revision (renamed columns, fewer measures)",
    ),
    (
        "Regional Sales Sample",
        "the legacy Layout binds 'Street Hierarchy' levels through a `From` alias \
         (`SourceRef.Source`), which the report reader does not resolve to the \
         hierarchy, so the PBIX's own report leaves it and Accounts[Country] unused",
    ),
];

/// Every Microsoft sample PBIX with a committed PBIP conversion must match
/// it, and every other PBIX/PBIT in the corpus (2018-2020 files carry older
/// catalog schemas) must ingest, or fail cleanly as a thin report.
#[test]
fn microsoft_desktop_samples_corpus() {
    let Some(corpus) = std::env::var_os("RIPBI_PBI_DESKTOP_SAMPLES").map(PathBuf::from) else {
        eprintln!("skipped: set RIPBI_PBI_DESKTOP_SAMPLES to a powerbi-desktop-samples clone");
        return;
    };
    let root = samples();
    let revamp = corpus.join("2026 Power BI Samples Revamp");
    for entry in std::fs::read_dir(&revamp).unwrap() {
        let pbix = entry.unwrap().path();
        let Some(stem) = pbix.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let model = root.join(format!("{stem}.SemanticModel"));
        if pbix.extension().is_some_and(|ext| ext == "pbix") && model.is_dir() {
            if let Some((_, reason)) = KNOWN_DIVERGENT.iter().find(|(name, _)| *name == stem) {
                eprintln!("parity skipped: {stem}: {reason}");
                continue;
            }
            eprintln!("parity: {stem}");
            assert_pbix_matches_pbip(&pbix, &model, &root.join(format!("{stem}.Report")));
        }
    }
    let mut stack = vec![corpus];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_none_or(|name| name != ".git") {
                    stack.push(path);
                }
                continue;
            }
            let is_archive = path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("pbix") || ext.eq_ignore_ascii_case("pbit")
            });
            if !is_archive {
                continue;
            }
            match semantic_model(&path) {
                Ok(model) => {
                    eprintln!(
                        "ok: {} ({} tables, {} skips)",
                        path.display(),
                        model.value.tables.len(),
                        model.skips.len()
                    );
                    let report = report(&path).unwrap_or_else(|error| {
                        panic!("{}: report failed: {error}", path.display())
                    });
                    let graph = DependencyGraph::build(&model.value, &[&report.value]);
                    let _ = graph.unused_objects();
                }
                Err(error) => {
                    let message = error.to_string();
                    assert!(
                        message.contains("no embedded semantic model"),
                        "{}: {message}",
                        path.display()
                    );
                    eprintln!("thin: {}", path.display());
                }
            }
        }
    }
}

fn assert_pbix_matches_pbip(archive: &Path, model: &Path, report_dir: &Path) {
    let tmdl = semantic_model(model).unwrap();
    let abf = semantic_model(archive).unwrap();
    let pbir = report(report_dir).unwrap();
    let archived_report = report(archive).unwrap();
    assert!(
        abf.skips.is_empty(),
        "{}: {:#?}",
        archive.display(),
        abf.skips
    );
    assert_identities_match(&abf.value, &tmdl.value);

    let original = DependencyGraph::build(&tmdl.value, &[&pbir.value]);
    let with_pbip_report = DependencyGraph::build(&abf.value, &[&pbir.value]);
    let with_own_report = DependencyGraph::build(&abf.value, &[&archived_report.value]);
    let expected = original.unused_objects();
    for graph in [&with_pbip_report, &with_own_report] {
        let actual = graph.unused_objects();
        let only_actual: Vec<_> = actual
            .iter()
            .filter(|item| !expected.contains(item))
            .collect();
        let only_expected: Vec<_> = expected
            .iter()
            .filter(|item| !actual.contains(item))
            .collect();
        assert!(
            only_actual.is_empty() && only_expected.is_empty(),
            "{}: only in PBIX: {only_actual:#?}
only in PBIP: {only_expected:#?}",
            archive.display()
        );
    }
}

/// Object identities, DAX text, and M text must agree between two ingests of
/// the same model (order-insensitive).
fn assert_identities_match(actual: &TabularDatabase, expected: &TabularDatabase) {
    fn objects(model: &TabularDatabase) -> Vec<String> {
        let mut out = Vec::new();
        for table in &model.tables {
            out.push(format!("table {}", table.name));
            for column in &table.columns {
                out.push(format!(
                    "column {}[{}] {:?}",
                    table.name, column.name, column.kind
                ));
            }
            for measure in &table.measures {
                out.push(format!("measure {}[{}]", table.name, measure.name));
            }
            for hierarchy in &table.hierarchies {
                out.push(format!(
                    "hierarchy {}[{}] {:?}",
                    table.name, hierarchy.name, hierarchy.levels
                ));
            }
            for partition in &table.partitions {
                out.push(format!("partition {}[{}]", table.name, partition.name));
            }
        }
        for relationship in &model.relationships {
            out.push(format!(
                "relationship {}[{}] -> {}[{}] active={}",
                relationship.from_table,
                relationship.from_column,
                relationship.to_table,
                relationship.to_column,
                relationship.is_active
            ));
        }
        for role in &model.roles {
            out.push(format!("role {}", role.name));
        }
        for expression in &model.expressions {
            out.push(format!("expression {}", expression.name));
        }
        for function in &model.functions {
            out.push(format!("function {}", function.name));
        }
        out.sort();
        out
    }
    fn texts(model: &TabularDatabase) -> Vec<(String, String)> {
        let mut out: Vec<_> = model
            .dax_expressions()
            .iter()
            .map(|item| {
                (
                    format!("{:?}", item.owner.to_object_id()),
                    normalize(item.text),
                )
            })
            .chain(model.m_expressions().iter().map(|item| {
                (
                    format!("{:?}", item.owner.to_object_id()),
                    normalize(item.text),
                )
            }))
            .collect();
        out.sort();
        out
    }
    fn normalize(text: &str) -> String {
        text.replace("\r\n", "\n").trim().to_string()
    }
    fn assert_same<T: PartialEq + std::fmt::Debug>(what: &str, actual: &[T], expected: &[T]) {
        let only_actual: Vec<_> = actual
            .iter()
            .filter(|item| !expected.contains(item))
            .collect();
        let only_expected: Vec<_> = expected
            .iter()
            .filter(|item| !actual.contains(item))
            .collect();
        assert!(
            only_actual.is_empty() && only_expected.is_empty(),
            "{what}: only in PBIX: {only_actual:#?}
only in PBIP: {only_expected:#?}"
        );
    }
    assert_same("objects", &objects(actual), &objects(expected));
    assert_same("expressions", &texts(actual), &texts(expected));
}
