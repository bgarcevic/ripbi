//! `ripbi.toml` — the per-project config file, shared by everyone who works on
//! the model and checked into version control. Precedence follows
//! `docs/cli-ux-guidelines.md`: flags over config; the config fills in
//! whatever the flags leave unset.
//!
//! Shape:
//!
//! ```toml
//! target = "samples/AdventureWorks Sales.SemanticModel"
//! reports = ["samples/AdventureWorks Sales.Report"]
//!
//! [scan]
//! # Object-name globs suppressed from the unused report.
//! ignore = ["'*Time Intelligence'[*]"]
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::ScanError;

/// The config file's parsed contents, with relative paths resolved against the
/// directory the file lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Scan target for a bare `ripbi scan`, as an absolute or config-relative path.
    pub target: Option<PathBuf>,
    /// Report roots for a scan that discovers none of its own.
    pub reports: Vec<PathBuf>,
    /// Object-name glob patterns suppressed from the unused report.
    pub ignore: Vec<String>,
}

/// A loaded config plus the directory its paths were resolved against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    /// The directory containing `ripbi.toml`.
    pub root: PathBuf,
    /// The parsed, path-resolved config.
    pub config: Config,
}

/// The file format. Unknown fields are ignored: config drift must never fail
/// a scan, mirroring core's ingestion policy.
#[derive(Debug, Default, Deserialize)]
struct FileFormat {
    target: Option<String>,
    #[serde(default)]
    reports: Vec<String>,
    scan: Option<ScanSection>,
}

#[derive(Debug, Default, Deserialize)]
struct ScanSection {
    #[serde(default)]
    ignore: Vec<String>,
}

/// Finds and loads `ripbi.toml` in `start` or its nearest ancestor.
///
/// Returns `None` when no ancestor has one. A file that exists but cannot be
/// parsed is a hard error: silently ignoring config the user wrote would run
/// the scan under rules they never see.
pub fn find_in(start: &Path) -> Result<Option<Loaded>, ScanError> {
    for ancestor in start.ancestors() {
        let path = ancestor.join("ripbi.toml");
        if !path.is_file() {
            continue;
        }
        let root = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let text = fs::read_to_string(&path)
            .map_err(|error| ScanError::new(format!("cannot read {}: {error}", path.display())))?;
        let file: FileFormat = toml::from_str(&text).map_err(|error| {
            ScanError::new(format!("cannot parse {}: {error}", path.display()))
                .with_hint("fix the TOML syntax, or move the file out of the way")
        })?;
        let resolve = |written: &String| resolve_against(&root, written);
        let target = file.target.as_ref().map(&resolve);
        let reports: Vec<PathBuf> = file.reports.iter().map(&resolve).collect();
        return Ok(Some(Loaded {
            root,
            config: Config {
                target,
                reports,
                ignore: file.scan.map(|scan| scan.ignore).unwrap_or_default(),
            },
        }));
    }
    Ok(None)
}

/// Resolves one written path: relative paths hang off the config's directory.
fn resolve_against(root: &Path, written: &str) -> PathBuf {
    let path = PathBuf::from(written);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn finds_the_config_from_a_nested_directory() {
        let temp = TempDir::new("nested");
        temp.write(
            "ripbi.toml",
            "target = \"model.SemanticModel\"\n[scan]\nignore = [\"_*\"]\n",
        );
        let nested = temp.mkdir("a/b");

        let loaded = find_in(&nested).expect("no error").expect("config found");

        assert_eq!(loaded.root, temp.0);
        assert_eq!(
            loaded.config.target,
            Some(temp.0.join("model.SemanticModel"))
        );
        assert_eq!(loaded.config.ignore, vec!["_*".to_string()]);
    }

    #[test]
    fn no_config_anywhere_is_none() {
        let temp = TempDir::new("absent");
        let deep = temp.mkdir("x/y");

        assert!(find_in(&deep).expect("no error").is_none());
    }

    #[test]
    fn a_broken_config_is_an_error_not_a_silent_ignore() {
        let temp = TempDir::new("broken");
        temp.write("ripbi.toml", "target = [unterminated");

        let error = find_in(&temp.0).expect_err("parse failure");

        assert!(error.message.contains("cannot parse"));
        assert!(error.hint.is_some());
    }

    #[test]
    fn unknown_fields_are_ignored_and_reports_resolve_relatively() {
        let temp = TempDir::new("drift");
        temp.write(
            "ripbi.toml",
            "future_key = 1\nreports = [\"r.Report\"]\n[scan]\nignore = []\nfuture_section = true\n",
        );

        let loaded = find_in(&temp.0).expect("no error").expect("config found");

        assert_eq!(loaded.config.reports, vec![temp.0.join("r.Report")]);
    }
}
