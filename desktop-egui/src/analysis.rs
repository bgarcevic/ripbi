//! Desktop adapter over `ripbi_app::scan`: the same pipeline `ripbi scan`
//! runs, called with typed inputs and returning typed results. Notes the scan
//! makes along the way are collected for the coverage tab.

use std::path::{Path, PathBuf};

use ripbi_app::ScanError;
use ripbi_app::output::ScanOutput;
use ripbi_app::scan::{self, Note, NoteLevel, Observer, Scan, ScanRequest, Target};

#[derive(Debug)]
pub struct Analysis {
    pub output: ScanOutput,
    /// The pairing header and every note, in the order the scan made them.
    pub log: Vec<String>,
}

impl Analysis {
    /// The `{ result, diagnostics }` export: `result` is exactly what
    /// `ripbi scan --json` prints.
    pub fn export(&self) -> Result<String, String> {
        let mut json = Vec::new();
        ripbi_app::json::write(&mut json, &self.output).map_err(|error| error.to_string())?;
        let result: serde_json::Value =
            serde_json::from_slice(&json).map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&serde_json::json!({
            "result": result,
            "diagnostics": self.log,
        }))
        .map_err(|error| error.to_string())
    }
}

/// Collects what a CLI would print on stderr.
#[derive(Default)]
struct Log(Vec<String>);

impl Observer for Log {
    fn resolved(&mut self, target: &Target<'_>) -> Result<(), ScanError> {
        self.0.push(format!(
            "{} · {} report(s): {}",
            target.model.display(),
            target.reports.len(),
            target
                .reports
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if let Some(bound) = target.bound {
            for path in &bound.ignored_elsewhere {
                self.0.push(format!(
                    "ignored {} (bound to another model)",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    fn note(&mut self, note: Note) -> Result<(), ScanError> {
        let marker = match note.level {
            NoteLevel::Info => "·",
            NoteLevel::Warn => "!",
        };
        self.0.push(format!("{marker} {}", note.text));
        Ok(())
    }
}

pub fn run(model: &Path, reports: &[PathBuf]) -> Result<Analysis, String> {
    if !model.is_absolute() || !model.exists() {
        return Err("Select an existing model using its full path.".into());
    }
    if reports
        .iter()
        .any(|path| !path.is_absolute() || !path.exists())
    {
        return Err("A report selection no longer exists. Remove it and select it again.".into());
    }
    // `ripbi.toml` is read from the model's folder upward, as the CLI would
    // when run beside it.
    let cwd = if model.is_dir() {
        model
    } else {
        model.parent().ok_or("The model has no parent folder.")?
    };
    let request = ScanRequest {
        cwd: cwd.to_path_buf(),
        model: Some(model.to_path_buf()),
        reports: reports.to_vec(),
        no_input: true,
        verbose: true,
        with_tables: true,
        ..ScanRequest::default()
    };
    let mut log = Log::default();
    let scanned = scan::run(&request, &mut log).map_err(|error| error.to_string())?;
    match scanned {
        Scan::Done(outcome) => Ok(Analysis {
            output: outcome.output,
            log: log.0,
        }),
        Scan::Skipped { model, .. } => Err(format!(
            "No reports bound to {} were found. Add the reports' .pbip projects or a folder to search.",
            model.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_sample_with_real_discovery_and_graph() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../samples")
            .canonicalize()
            .unwrap();
        let model = root.join("AdventureWorks Sales.SemanticModel");
        let analysis = run(&model, std::slice::from_ref(&root)).unwrap();
        let output = &analysis.output;
        assert!(output.objects > 0);
        assert_eq!(output.objects, output.reachable + output.unused_raw);
        assert!(!output.reports.is_empty());
        assert!(analysis.export().unwrap().contains("\"schema_version\": 1"));
    }

    #[test]
    fn discovers_reports_beside_the_model_when_none_are_listed() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../samples")
            .canonicalize()
            .unwrap();
        let model = root.join("AdventureWorks Sales.SemanticModel");
        let analysis = run(&model, &[]).unwrap();
        assert!(!analysis.output.reports.is_empty());
    }
}
