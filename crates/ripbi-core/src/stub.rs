//! A minimal PBIR report stub bound to a semantic-model folder (issue #130).
//!
//! Power BI Desktop only opens a `.pbip` that names a report, so a model-only
//! PBIP/TMDL project cannot be opened, refreshed, and saved with its data —
//! and without that save there is no `.pbi/cache.abf` for issue #129 to read
//! storage sizes from. [`report_stub`] produces the smallest report Desktop
//! opens: one empty page, no visuals, no filters, so it adds **no
//! reachability roots**. Its `report.json` carries the [`STUB_ANNOTATION`]
//! annotation, which the PBIR reader surfaces as [`ReportModel::stub`] so a
//! scan can say it saw a stub instead of counting it silently.
//!
//! This module only builds file contents; writing them is the caller's job
//! (the CLI's `stub-report` command), keeping this crate free of side effects
//! beyond reading.
//!
//! [`ReportModel::stub`]: crate::ReportModel::stub

use std::path::PathBuf;

use serde_json::{Value, json};

/// The `report.json` annotation name that marks a ripbi-generated stub.
pub const STUB_ANNOTATION: &str = "ripbi.stub";

/// The stub's single page folder and `name`.
const PAGE_NAME: &str = "ripbiStub";

/// One file of a report stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubFile {
    /// Path relative to the folder that holds the semantic-model folder, with
    /// forward slashes (e.g. `Sales.Report/definition.pbir`).
    pub path: PathBuf,
    /// UTF-8 contents, ending in a newline.
    pub contents: String,
}

/// The files of a report stub named `name`, bound by relative path to the
/// sibling semantic-model folder `model_folder` (e.g. `Sales.SemanticModel`).
///
/// Returns `<name>.pbip` and the `<name>.Report/` item: `definition.pbir`
/// (`datasetReference.byPath` = `../<model_folder>`), and a PBIR `definition/`
/// with `version.json`, `report.json`, `pages/pages.json`, and one empty page.
/// No `.platform` is written: its `logicalId` must be unique per workspace, and
/// Desktop creates the file on first save.
///
/// The output is deterministic, so it can be pinned as a golden fixture.
#[must_use]
pub fn report_stub(name: &str, model_folder: &str) -> Vec<StubFile> {
    let report = format!("{name}.Report");
    let file = |path: String, value: &Value| StubFile {
        path: PathBuf::from(path),
        contents: pretty(value),
    };
    vec![
        file(
            format!("{name}.pbip"),
            &json!({
                "$schema": "https://developer.microsoft.com/json-schemas/fabric/pbip/pbipProperties/1.0.0/schema.json",
                "version": "1.0",
                "artifacts": [{ "report": { "path": report } }],
                "settings": { "enableAutoRecovery": true }
            }),
        ),
        file(
            format!("{report}/definition.pbir"),
            &json!({
                "$schema": "https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/2.0.0/schema.json",
                "version": "4.0",
                "datasetReference": { "byPath": { "path": format!("../{model_folder}") } }
            }),
        ),
        file(
            format!("{report}/definition/version.json"),
            &json!({
                "$schema": "https://developer.microsoft.com/json-schemas/fabric/item/report/definition/versionMetadata/1.0.0/schema.json",
                "version": "2.0.0"
            }),
        ),
        file(format!("{report}/definition/report.json"), &report_json()),
        file(
            format!("{report}/definition/pages/pages.json"),
            &json!({
                "$schema": "https://developer.microsoft.com/json-schemas/fabric/item/report/definition/pagesMetadata/1.1.0/schema.json",
                "pageOrder": [PAGE_NAME],
                "activePageName": PAGE_NAME
            }),
        ),
        file(
            format!("{report}/definition/pages/{PAGE_NAME}/page.json"),
            &json!({
                "$schema": "https://developer.microsoft.com/json-schemas/fabric/item/report/definition/page/2.1.0/schema.json",
                "name": PAGE_NAME,
                "displayName": "ripbi stub",
                "displayOption": "FitToPage",
                "height": 720,
                "width": 1280
            }),
        ),
    ]
}

/// `report.json`: the schema's required base theme (the built-in one current
/// Desktop writes, resolved from its own shared resources) plus the stub
/// annotation.
fn report_json() -> Value {
    let version = json!({ "visual": "2.12.0", "report": "3.4.0", "page": "2.3.1" });
    json!({
        "$schema": "https://developer.microsoft.com/json-schemas/fabric/item/report/definition/report/3.3.0/schema.json",
        "themeCollection": {
            "baseTheme": {
                "name": "Fluent2-CY26SU08",
                "reportVersionAtImport": version,
                "type": "SharedResources"
            }
        },
        "resourcePackages": [{
            "name": "SharedResources",
            "type": "SharedResources",
            "items": [{
                "name": "Fluent2-CY26SU08",
                "path": "BaseThemes/Fluent2-CY26SU08.json",
                "type": "BaseTheme"
            }]
        }],
        "annotations": [{
            "name": STUB_ANNOTATION,
            "value": "Generated by `ripbi stub-report` so Desktop can refresh and save the model. Binds nothing."
        }]
    })
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}
