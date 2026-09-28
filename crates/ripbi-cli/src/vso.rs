//! `scan --azure-devops`: one `##vso[task.logissue]` logging command per
//! finding, so an Azure Pipelines run lists each finding as a warning or
//! error on the build summary — no extension or artifact needed. The rules,
//! messages, and sites are `sarif.rs`'s; the contract lives in
//! `docs/output.md` under "Azure DevOps".

use std::io;
use std::path::Path;

use crate::render::ScanOutput;
use crate::sarif::{self, Existing, Issue, Locator};

/// Writes the logging commands for one scan. Findings that already existed
/// under `--compare-root` are dropped, as in every mode but SARIF.
///
/// # Errors
/// Propagates stream write failures.
pub fn write(
    out: &mut dyn io::Write,
    report: &ScanOutput,
    locator: &Locator,
    cwd: &Path,
) -> io::Result<()> {
    let existing = Existing::default();
    for issue in sarif::issues(report, &existing, locator)
        .iter()
        .filter(|issue| !issue.existing)
    {
        writeln!(out, "{}", command(issue, cwd))?;
    }
    Ok(())
}

fn command(issue: &Issue, cwd: &Path) -> String {
    let mut properties = vec![("type", issue.level().to_owned())];
    if let Some(site) = issue.site {
        properties.push(("sourcepath", source_path(&site.path, cwd)));
        if let Some(line) = site.line {
            properties.push(("linenumber", line.to_string()));
        }
    }
    properties.push(("code", issue.rule_id().to_owned()));
    let properties: String = properties
        .iter()
        .map(|(key, value)| format!("{key}={};", escape_property(value)))
        .collect();
    format!(
        "##vso[task.logissue {properties}]{}",
        escape_data(&issue.message)
    )
}

/// Relative to the working directory with `/` separators when beneath it —
/// the repository root in a pipeline — else the absolute path.
fn source_path(path: &Path, cwd: &Path) -> String {
    sarif::relative_segments(path, cwd).map_or_else(
        || sarif::normalize(&cwd.join(path)).display().to_string(),
        |segments| segments.join("/"),
    )
}

/// The agent's message escaping (azure-pipelines-task-lib `escapedata`):
/// `%` first, so the other escapes are not themselves re-escaped.
fn escape_data(text: &str) -> String {
    text.replace('%', "%AZP25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Property values additionally escape the command's own delimiters.
fn escape_property(text: &str) -> String {
    escape_data(text).replace(']', "%5D").replace(';', "%3B")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimiters_are_escaped() {
        assert_eq!(escape_data("a%b\r\nc]"), "a%AZP25b%0D%0Ac]");
        assert_eq!(escape_property("x;y]z%"), "x%3By%5Dz%AZP25");
    }

    #[test]
    fn source_paths_are_relative_beneath_the_working_directory() {
        let cwd = Path::new("/repo");
        assert_eq!(
            source_path(Path::new("/repo/./A B.SemanticModel/model.tmdl"), cwd),
            "A B.SemanticModel/model.tmdl"
        );
    }
}
