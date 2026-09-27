//! Integration: the issue #130 report stub. Its exact files are pinned by a
//! golden fixture (`tests/fixtures/stub/`), and the stub of every sample
//! `.SemanticModel` reads back through the PBIR reader as a bound, flagged
//! report with no bindings and no notices.

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_core::ingest::report;
use ripbi_core::{DatasetReference, report_stub};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Writes `name`'s stub beside `model_folder` under `root`; returns the report folder.
fn write_stub(root: &Path, name: &str, model_folder: &str) -> PathBuf {
    for file in report_stub(name, model_folder) {
        let path = root.join(&file.path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, file.contents).unwrap();
    }
    root.join(format!("{name}.Report"))
}

#[test]
fn the_stub_matches_the_golden_fixture() {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stub");
    let files = report_stub("Sales", "Sales.SemanticModel");
    for file in &files {
        let expected = fs::read_to_string(golden.join(&file.path))
            .unwrap_or_else(|error| panic!("{}: {error}", file.path.display()))
            .replace("\r\n", "\n");
        assert_eq!(file.contents, expected, "{}", file.path.display());
    }
    assert_eq!(files.len(), 6, "a new stub file needs a golden copy");
}

#[test]
fn every_sample_models_stub_reads_back_with_no_bindings_or_notices() {
    let samples = repo().join("samples");
    let mut models: Vec<String> = fs::read_dir(&samples)
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.ends_with(".SemanticModel"))
        .collect();
    models.sort();
    assert!(!models.is_empty(), "the samples must be present");

    let temp = tempfile::tempdir().unwrap();
    for model in models {
        let stem = model.trim_end_matches(".SemanticModel");
        let folder = write_stub(temp.path(), stem, &model);
        let ingested = report(&folder).unwrap_or_else(|error| panic!("{model}: {error}"));
        assert!(ingested.skips.is_empty(), "{model}: {:?}", ingested.skips);
        let stub = ingested.value;
        assert!(stub.stub, "{model}");
        assert!(stub.bindings().is_empty(), "{model}");
        assert!(stub.dax_expressions().is_empty(), "{model}");
        assert_eq!(stub.pages.len(), 1, "{model}");
        assert_eq!(
            stub.dataset,
            DatasetReference::ByPath {
                path: format!("../{model}")
            },
            "{model}"
        );
    }
}

#[test]
fn a_real_report_is_not_a_stub() {
    let sample = repo().join("samples/AdventureWorks Sales.Report");
    assert!(!report(&sample).unwrap().value.stub);
}
