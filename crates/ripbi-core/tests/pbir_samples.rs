//! Integration: every real PBIR export under `samples/` parses with zero
//! notices. The samples are real Fabric Git Integration exports — PBIR is
//! preview and their `$schema` versions run ahead of the published schemas —
//! so these runs keep the curated ignore lists honest: a new key in a future
//! export fails here until it is parsed or deliberately ignored.

use std::path::{Path, PathBuf};

use ripbi_core::ingest::report;
use ripbi_core::report::{BindingKind, FieldTarget};

/// The `.Report` folders shipped in the repository.
fn samples() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("samples");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("samples/ is part of the repository")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".Report"))
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "the samples must be present");
    out
}

#[test]
fn every_sample_parses_with_no_notices() {
    for sample in samples() {
        let ingested = report(&sample)
            .unwrap_or_else(|error| panic!("{} must parse: {error}", sample.display()));
        assert!(
            ingested.skips.is_empty(),
            "{} must parse without notices: {:#?}",
            sample.display(),
            ingested.skips
        );
    }
}

/// The one sample whose shape is pinned: its single page, its donut chart's
/// wells, and its sort — fields the AdventureWorks model defines.
#[test]
fn adventure_works_parses_to_its_known_shape() {
    let sample = samples()
        .into_iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "AdventureWorks Sales.Report")
        })
        .expect("the AdventureWorks sample is part of the repository");
    let ingested = report(&sample).expect("the sample parses");

    let report = ingested.value;
    assert_eq!(
        report.dataset,
        ripbi_core::DatasetReference::ByPath {
            path: "../AdventureWorks Sales.SemanticModel".to_string(),
        }
    );
    assert_eq!(report.pages.len(), 1);
    assert_eq!(report.pages[0].name.as_str(), "ReportSection");
    assert_eq!(report.pages[0].display_name.as_deref(), Some("Overview"));

    let donut = report.pages[0]
        .visuals
        .iter()
        .find(|visual| visual.visual_type == "donutChart")
        .expect("the donut chart is on the page");
    let category = &donut
        .wells
        .iter()
        .find(|well| well.role == "Category")
        .unwrap()
        .projections[0];
    assert_eq!(
        category.target,
        FieldTarget::Column {
            table: ripbi_core::NameKey::new("Product"),
            column: ripbi_core::NameKey::new("Category"),
        }
    );
    assert!(category.active);
    let y = &donut
        .wells
        .iter()
        .find(|well| well.role == "Y")
        .unwrap()
        .projections[0];
    assert_eq!(
        y.target,
        FieldTarget::Measure {
            home_table: Some(ripbi_core::NameKey::new("Sales")),
            measure: ripbi_core::NameKey::new("Cost"),
        }
    );
    assert_eq!(donut.sorts.len(), 1);
}

/// Bookmarks are roots: their saved filters and projections must reach the
/// binding set with bookmark provenance.
#[test]
fn bookmark_filters_reach_the_root_set() {
    let ai_sample = samples()
        .into_iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "Artificial Intelligence Sample.Report")
        })
        .expect("the AI sample is part of the repository");
    let ingested = report(&ai_sample).expect("the sample parses");

    assert!(
        !ingested.value.bookmarks.is_empty(),
        "the AI sample carries bookmarks"
    );

    let bookmarked_status_filter = ingested
        .value
        .bindings()
        .into_iter()
        .find(|binding| {
            binding.bookmark.is_some()
                && binding.kind == BindingKind::Filter
                && binding.target.to_string() == "'Opportunities'[Status]"
        })
        .expect("a bookmark's saved Opportunities[Status] filter is a root");
    assert!(
        bookmarked_status_filter.page.is_some(),
        "the filter was saved on a page section"
    );
}
