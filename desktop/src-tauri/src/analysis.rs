//! Desktop adapter for the CLI's established discovery and scan contract.

use std::path::{Path, PathBuf};

use ripbi_cli::{Streams, cli::ScanArgs, scan};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Analysis {
    result: serde_json::Value,
    diagnostics: String,
}

pub fn run(model: &str, reports: &[String]) -> Result<Analysis, String> {
    let model = Path::new(model);
    if !model.is_absolute() || !model.exists() {
        return Err("Select an existing model using its full path.".into());
    }
    if reports.is_empty() {
        return Err("Add at least one report or a folder containing reports.".into());
    }
    if reports
        .iter()
        .any(|path| !Path::new(path).is_absolute() || !Path::new(path).exists())
    {
        return Err("A report selection no longer exists. Remove it and select it again.".into());
    }
    let cwd = if model.is_dir() {
        model
    } else {
        model.parent().ok_or("The model has no parent folder.")?
    };
    let args = ScanArgs {
        model: Some(model.to_path_buf()),
        reports: reports.iter().map(PathBuf::from).collect(),
        json: true,
        verbose: true,
        no_color: true,
        no_input: true,
        ..ScanArgs::default()
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = scan::run_in(
        &args,
        cwd,
        &mut Streams {
            out: &mut out,
            err: &mut err,
            input: &mut std::io::empty(),
            stdin_is_tty: false,
            stdout_is_tty: false,
            stderr_is_tty: false,
        },
    );
    let diagnostics = String::from_utf8_lossy(&err).into_owned();
    if code == scan::EXIT_ERROR {
        return Err(diagnostics);
    }
    let result: serde_json::Value = serde_json::from_slice(&out)
        .map_err(|error| format!("Could not read scan results: {error}"))?;
    if result["schema_version"] != 1 {
        return Err("Unsupported scan result version.".into());
    }
    Ok(Analysis {
        result,
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_sample_with_real_discovery_and_graph() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../samples")
            .canonicalize()
            .unwrap();
        let model = root.join("AdventureWorks Sales.SemanticModel");
        let analysis = run(
            model.to_str().unwrap(),
            &[root.to_string_lossy().into_owned()],
        )
        .unwrap();
        let summary = &analysis.result["summary"];
        assert_eq!(
            summary["objects"].as_u64().unwrap(),
            summary["reachable"].as_u64().unwrap() + summary["unused_total"].as_u64().unwrap()
        );
        assert!(!analysis.result["reports"].as_array().unwrap().is_empty());
        let direct = run(
            model.to_str().unwrap(),
            &[root
                .join("AdventureWorks Sales.Report")
                .to_string_lossy()
                .into_owned()],
        )
        .unwrap();
        assert_eq!(analysis.result["summary"], direct.result["summary"]);
    }

    #[test]
    fn refuses_empty_report_selection() {
        assert!(
            run(env!("CARGO_MANIFEST_DIR"), &[])
                .unwrap_err()
                .contains("at least one")
        );
    }

    #[test]
    fn refuses_missing_report_paths() {
        assert!(
            run(env!("CARGO_MANIFEST_DIR"), &["missing-report".into()])
                .unwrap_err()
                .contains("no longer exists")
        );
    }
}
