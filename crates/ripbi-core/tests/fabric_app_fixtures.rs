//! Fabric apps as report consumers: the app's string literals are roots once
//! resolved against the model — written DAX, and bare names composed into
//! DAX at run time — while tests, comments, agent kits, and dependencies are
//! never read.

use std::path::PathBuf;

use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest::{is_fabric_app, report, semantic_model};
use ripbi_core::{BindingSite, BrokenReason, NameKey, ObjectId, Provenance};

fn fixture(part: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fabric-app")
        .join(part)
}

fn column(table: &str, column: &str) -> ObjectId {
    ObjectId::Column {
        table: NameKey::new(table),
        column: NameKey::new(column),
    }
}

fn measure(table: &str, measure: &str) -> ObjectId {
    ObjectId::Measure {
        table: NameKey::new(table),
        measure: NameKey::new(measure),
    }
}

#[test]
fn an_app_reads_its_own_source_only() {
    let app = fixture("app");
    assert!(is_fabric_app(&app));
    let ingested = report(&app).expect("reads the app");
    assert!(ingested.skips.is_empty(), "{:?}", ingested.skips);
    let app = ingested.value;
    assert_eq!(app.name.as_deref(), Some("Ledger App"));
    assert!(app.pages.is_empty());
    let files: std::collections::BTreeSet<&str> = app
        .code
        .iter()
        .map(|literal| literal.file.as_str())
        .collect();
    assert_eq!(
        files.into_iter().collect::<Vec<_>>(),
        ["src/config.ts", "src/lib/query.ts"]
    );
}

#[test]
fn app_literals_keep_what_the_app_queries() {
    let model = semantic_model(&fixture("Ledger.SemanticModel"))
        .expect("reads the model")
        .value;
    let app = report(&fixture("app")).expect("reads the app").value;
    let graph = DependencyGraph::build(&model, &[&app]);
    let unused: Vec<ObjectId> = graph
        .unused_objects()
        .into_iter()
        .map(|unused| unused.id)
        .collect();

    // Bare names from the config, scoped to the table the app names.
    for live in [column("Ledger", "Year"), column("Ledger", "Account")] {
        assert!(!unused.contains(&live), "{live:?} should be live");
    }
    // A column name no named table has falls back to every such column.
    assert!(!unused.contains(&column("Date", "Month")));
    // Written DAX in a template run, and what the measure reads.
    assert!(!unused.contains(&measure("Metrics", "Revenue")));
    assert!(!unused.contains(&column("Ledger", "Value")));

    // Scoped away: 'Year' names Ledger's column, not Date's.
    assert!(unused.contains(&column("Date", "Year")));
    // Named only in a comment and a test file.
    assert!(unused.contains(&column("Ledger", "Quarter")));
    // Named only by the agent kit and node_modules.
    assert!(unused.contains(&measure("Metrics", "Kit Example")));
}

#[test]
fn an_app_root_says_where_the_literal_is() {
    let model = semantic_model(&fixture("Ledger.SemanticModel"))
        .expect("reads the model")
        .value;
    let app = report(&fixture("app")).expect("reads the app").value;
    let graph = DependencyGraph::build(&model, &[&app]);
    let sites: Vec<String> = graph
        .roots_of(&column("Ledger", "Year"))
        .into_iter()
        .filter_map(|provenance| match provenance {
            Provenance::Binding(edge) => match &edge.kind {
                BindingSite::Code { file, line } => Some(format!("{file}:{line}")),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(sites, ["src/config.ts:5"]);
    let edge = graph
        .roots_of(&column("Ledger", "Year"))
        .into_iter()
        .find_map(|provenance| match provenance {
            Provenance::Binding(edge) => Some(edge.to_string()),
            _ => None,
        })
        .expect("bound");
    assert_eq!(edge, "code at src/config.ts:5 in report 'Ledger App'");
}

#[test]
fn an_unmatched_literal_is_never_a_broken_binding() {
    let model = semantic_model(&fixture("Ledger.SemanticModel"))
        .expect("reads the model")
        .value;
    let app = report(&fixture("app")).expect("reads the app").value;
    let graph = DependencyGraph::build(&model, &[&app]);
    assert!(graph.broken_bindings().is_empty());
}

/// Inside a DAX literal anchored to the model by a reference that resolves,
/// a reference that does not is breakage. Names the app's queries introduce
/// count wherever the app writes them; selectors and a literal with nothing
/// left to anchor it are text.
#[test]
fn an_anchored_query_literal_reports_what_no_longer_resolves() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fabric-app-broken");
    let model = semantic_model(&root.join("Books.SemanticModel"))
        .expect("reads the model")
        .value;
    let app = report(&root.join("app")).expect("reads the app").value;
    let graph = DependencyGraph::build(&model, &[&app]);
    let mut broken: Vec<String> = graph
        .broken_bindings()
        .iter()
        .map(|binding| {
            let BindingSite::Code { file, line } = &binding.edge.kind else {
                panic!("an app binding sits in code: {binding:?}");
            };
            let reason = match &binding.reason {
                BrokenReason::BoundArtifactBroken { artifact, .. } => {
                    format!("bound artifact {artifact}")
                }
                reason => format!("{reason:?}"),
            };
            format!("{file}:{line} {} {reason}", binding.target)
        })
        .collect();
    broken.sort();
    assert_eq!(
        broken,
        [
            "src/queries.ts:11 'Budget'[Amount] TableNotFound",
            "src/queries.ts:9 'Ledger'[Region] FieldNotFound",
            "src/queries.ts:9 [Margin] MeasureNotFound",
            "src/queries.ts:9 [Stale Margin] bound artifact 'Metrics'[Stale Margin]",
        ]
    );
}
