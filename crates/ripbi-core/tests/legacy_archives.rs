//! Small UTF-16 Layout archives derived from the structure of the public
//! pbixray `old-Retail-Analysis-Sample-PBIX.pbix` (MIT licensed).

use std::path::PathBuf;

use ripbi_core::ingest::{SkipKind, report, semantic_model};
use ripbi_core::model::PartitionSource;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/legacy")
        .join(name)
}

#[test]
fn legacy_layout_keeps_unknown_visual_textbox_and_visual_calculation_fields() {
    let parsed = report(&fixture("legacy-layout.pbix")).unwrap();
    assert_eq!(parsed.value.pages.len(), 1);
    assert_eq!(parsed.value.pages[0].visuals.len(), 3);
    assert_eq!(parsed.value.pages[0].visuals[0].visual_type, "futureVisual");
    assert_eq!(parsed.skips.len(), 2);
    assert!(
        parsed
            .skips
            .iter()
            .any(|notice| notice.kind == SkipKind::UnknownObject)
    );
    assert!(
        parsed
            .skips
            .iter()
            .any(|notice| notice.kind == SkipKind::MalformedValue)
    );

    let bindings = parsed.value.bindings();
    let written: Vec<_> = bindings
        .iter()
        .map(|binding| binding.target.to_string())
        .collect();
    for expected in [
        "'Sales'[Region]",
        "'Sales'[Amount]",
        "'Sales'[Quantity]",
        "'Sales'[Profit]",
        "'Sales'[Caption]",
    ] {
        assert!(
            written.iter().any(|field| field == expected),
            "missing {expected}: {written:?}"
        );
    }
    assert!(bindings.iter().any(|binding| {
        binding
            .visual
            .is_some_and(|visual| visual.as_str() == "TextBox")
            && binding.target.to_string() == "'Sales'[Caption]"
    }));
}

#[test]
fn missing_m_fails_with_a_coverage_error() {
    let error = semantic_model(&fixture("missing-m.pbit")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incomplete M expression coverage")
    );
}

#[test]
fn datamashup_recovers_partition_and_shared_m() {
    let model = semantic_model(&fixture("mashup-fallback.pbit"))
        .unwrap()
        .value;
    assert!(matches!(&model.tables[0].partitions[0].source,
        PartitionSource::M { expression } if expression.contains("Parameter")));
    assert_eq!(model.expressions.len(), 2);
    assert!(
        model
            .expressions
            .iter()
            .any(|item| item.name == "Parameter" && item.expression == "42")
    );
    assert!(
        model
            .expressions
            .iter()
            .any(|item| item.name == "Helper" && item.expression.starts_with("#table"))
    );
}

#[test]
fn malformed_archive_returns_an_error() {
    assert!(report(&fixture("layout-source.json")).is_err());
}

#[test]
fn model_archive_is_detected_by_contents_even_with_a_pbix_extension() {
    let path = std::env::temp_dir().join(format!(
        "ripbi-schema-{}-{}.pbix",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::copy(fixture("mashup-fallback.pbit"), &path).unwrap();
    let result = semantic_model(&path);
    std::fs::remove_file(&path).unwrap();
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn report_only_pbix_does_not_claim_to_decode_its_model() {
    let error = semantic_model(&fixture("modern-report.pbix")).unwrap_err();
    assert!(error.to_string().contains("issue #10"));
}
