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
//! # A workspace-monitoring query export: objects its queries name count as
//! # used by `scan` and show under Impact in `deps`.
//! queries_from = "exports/semantic-model-logs.csv"
//!
//! [scan]
//! # Object-name globs suppressed from the unused report.
//! ignore = ["'*Time Intelligence'[*]"]
//! # Storage sizes for a model without its own catalog: an .abf, .pbix, or .vpax,
//! # or "none" to turn off .pbi/cache.abf auto-detection.
//! stats_from = "exports/Sales.abf"
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
    /// `[scan].stats_from`: where storage sizes come from (issue #129).
    pub stats_from: Option<StatsSetting>,
    /// `queries_from`: a workspace-monitoring query-log export, shared by
    /// `scan` and `deps`.
    pub queries_from: Option<PathBuf>,
    /// `queries_item`: which model's rows to read from a workspace-wide
    /// query log, by `ItemName` or `ItemId`.
    pub queries_item: Option<String>,
}

/// A `--stats-from` or `[scan].stats_from` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatsSetting {
    /// `none`: attach nothing, not even an auto-detected `.pbi/cache.abf`.
    Off,
    /// An `.abf` backup or a `.pbix` with its data.
    Path(PathBuf),
}

impl StatsSetting {
    /// Reads a written value: `none` (any case) turns storage off; anything
    /// else is a path, resolved against `base` when relative.
    pub fn parse(base: &Path, written: &Path) -> Self {
        if written
            .to_str()
            .is_some_and(|text| text.eq_ignore_ascii_case("none"))
        {
            return Self::Off;
        }
        Self::Path(resolve_against(base, &written.to_string_lossy()))
    }
}

/// A loaded config plus the directory its paths were resolved against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    /// The directory containing `ripbi.toml`.
    pub root: PathBuf,
    /// The parsed, path-resolved config.
    pub config: Config,
}

/// The user-authored file format. Reject unknown keys so a typo cannot
/// silently change the analysis.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFormat {
    target: Option<String>,
    #[serde(default)]
    reports: Vec<String>,
    queries_from: Option<String>,
    queries_item: Option<String>,
    scan: Option<ScanSection>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanSection {
    #[serde(default)]
    ignore: Vec<String>,
    stats_from: Option<String>,
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
        let queries_from = file.queries_from.as_ref().map(&resolve);
        let scan = file.scan.unwrap_or_default();
        let stats_from = scan
            .stats_from
            .as_deref()
            .map(|written| StatsSetting::parse(&root, Path::new(written)));
        return Ok(Some(Loaded {
            root,
            config: Config {
                target,
                reports,
                ignore: scan.ignore,
                stats_from,
                queries_from,
                queries_item: file.queries_item,
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
    fn reports_resolve_relatively() {
        let temp = TempDir::new("drift");
        temp.write(
            "ripbi.toml",
            "reports = [\"r.Report\"]\n[scan]\nignore = []\n",
        );

        let loaded = find_in(&temp.0).expect("no error").expect("config found");

        assert_eq!(loaded.config.reports, vec![temp.0.join("r.Report")]);
    }

    #[test]
    fn stats_from_resolves_relatively_and_none_turns_it_off() {
        let temp = TempDir::new("storage");
        temp.write(
            "ripbi.toml",
            "[scan]
stats_from = \"exports/m.abf\"
",
        );
        let loaded = find_in(&temp.0).expect("no error").expect("config found");
        assert_eq!(
            loaded.config.stats_from,
            Some(StatsSetting::Path(temp.0.join("exports/m.abf")))
        );

        temp.write(
            "ripbi.toml",
            "[scan]
stats_from = \"None\"
",
        );
        let loaded = find_in(&temp.0).expect("no error").expect("config found");
        assert_eq!(loaded.config.stats_from, Some(StatsSetting::Off));
    }

    #[test]
    fn unknown_config_keys_fail_with_a_parse_error() {
        let temp = TempDir::new("typo");
        temp.write("ripbi.toml", "[scan]\nignroe = [\"*\"]\n");
        let error = find_in(&temp.0).expect_err("unknown key");
        assert!(error.message.contains("ignroe"));
    }
}
