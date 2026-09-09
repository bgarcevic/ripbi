//! Optional real-world smoke test over a private production model.
//!
//! ripbi's committed corpora are the `samples/` projects and the synthetic
//! fixtures; this test adds the other direction — a real model large enough
//! to break assumptions the small fixtures cannot. It runs only when
//! `RIPBI_REALWORLD_PBIP` points at a local `.pbip` project, and never
//! commits anything about the model it sees: no paths, no names, no counts.
//!
//! ```text
//! RIPBI_REALWORLD_PBIP=C:\models\Production.pbip cargo test -p ripbi --test realworld
//! ```

#[allow(dead_code)]
mod common;

use std::path::PathBuf;

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, run_scan};

fn realworld_pbip() -> Option<PathBuf> {
    std::env::var_os("RIPBI_REALWORLD_PBIP").map(PathBuf::from)
}

/// A full scan of the real model must complete without panicking, produce a
/// valid JSON payload, and keep the documented exit-code contract — nothing
/// more is asserted, because a private model has no committed ground truth.
#[test]
fn the_realworld_model_scans_clean() {
    let Some(sample) = realworld_pbip() else {
        eprintln!(
            "skipped: set RIPBI_REALWORLD_PBIP to a local .pbip project to run the \
             real-world smoke test"
        );
        return;
    };
    assert!(sample.exists(), "RIPBI_REALWORLD_PBIP points nowhere");

    let temp = TempDir::new("realworld");
    let args = ScanArgs {
        json: true,
        path: Some(sample),
        ..ScanArgs::default()
    };
    let (code, stdout, _) = run_scan(&args, &temp.0, "");
    assert!(
        code == 0 || code == 1,
        "the scan must not error (exit codes 0/1), got {code}"
    );

    let payload: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let findings = payload["unused"].as_array().expect("unused array");
    eprintln!("real-world scan: {} unused objects", findings.len());
}
