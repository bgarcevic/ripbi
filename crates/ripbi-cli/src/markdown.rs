//! `scan --markdown-file`: a GitHub-flavored Markdown summary of one scan,
//! shaped for a CI job summary or a pull request comment (issue #143) — the
//! counts, the findings, what the change fixed, and the worst tables. The
//! contract lives in `docs/output.md` under "Markdown".

use std::io;
use std::path::Path;

use crate::render::{self, GROUPS, ScanOutput};

/// Rows each findings list shows before it is cut short: a PR comment is
/// capped at 65,536 characters, and nobody reads past a few dozen rows there.
const MAX_ROWS: usize = 50;

/// Writes the Markdown summary for one scan.
///
/// # Errors
/// Propagates stream write failures.
pub fn write(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    let name = Path::new(&report.target).file_name().map_or_else(
        || report.target.clone(),
        |name| name.to_string_lossy().into_owned(),
    );
    writeln!(out, "### ripbi scan: {}", code(&name))?;
    writeln!(out)?;
    headline(out, report)?;
    counts(out, report)?;
    findings(out, report)?;
    fixed(out, report)?;
    worst_tables(out, report)?;
    notes(out, report)
}

/// `**2 new findings** · 1 fixed · 56 already in `../base``, or the plain
/// count without a comparison.
fn headline(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    let total = report.reported();
    let noun = if total == 1 { "finding" } else { "findings" };
    match &report.compare {
        Some(compare) => writeln!(
            out,
            "**{total} new {noun}** · {} fixed · {} already in {}",
            compare.fixed.len(),
            compare.existing,
            code(&compare.root)
        )?,
        None => writeln!(out, "**{total} {noun}**")?,
    }
    writeln!(out)
}

/// The per-type totals, one row per type with findings.
fn counts(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    let mut rows: Vec<(String, usize)> = GROUPS
        .iter()
        .map(|(kind, label)| {
            let count = report
                .findings
                .iter()
                .filter(|finding| finding.kind == *kind)
                .count();
            (format!("Unused {}", label.to_lowercase()), count)
        })
        .collect();
    let auto_date_time = report
        .auto_date_time
        .iter()
        .filter(|row| row.verdict != "in_use")
        .count();
    rows.push(("Auto date/time tables".to_owned(), auto_date_time));
    rows.push(("Broken visual bindings".to_owned(), report.broken.len()));
    rows.push(("Broken artifacts".to_owned(), report.broken_artifacts.len()));
    rows.retain(|(_, count)| *count > 0);
    if rows.is_empty() {
        return Ok(());
    }
    writeln!(out, "| Finding | Count |")?;
    writeln!(out, "|---|--:|")?;
    for (label, count) in rows {
        writeln!(out, "| {label} | {count} |")?;
    }
    writeln!(out)
}

/// Every reported finding as `(type, object, detail)`, in the human output's
/// section order: unused objects, auto date/time, broken artifacts, broken
/// bindings.
fn rows(report: &ScanOutput) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    for (kind, _) in GROUPS {
        for finding in report
            .findings
            .iter()
            .filter(|finding| finding.kind == *kind)
        {
            let detail = finding
                .storage
                .and_then(|stats| stats.bytes)
                .map(render::format_bytes)
                .unwrap_or_default();
            rows.push((kind.replace('_', " "), code(&finding.id), detail));
        }
    }
    for row in report
        .auto_date_time
        .iter()
        .filter(|row| row.verdict != "in_use")
    {
        rows.push((
            "auto date/time table".to_owned(),
            code(&row.id),
            row.verdict.replace('_', " "),
        ));
    }
    for artifact in &report.broken_artifacts {
        let references: Vec<String> = artifact
            .unresolved_references
            .iter()
            .map(|reference| code(reference))
            .collect();
        rows.push((
            format!("broken {}", artifact.kind.replace('_', " ")),
            code(&artifact.id),
            format!("unresolved: {}", references.join(", ")),
        ));
    }
    for binding in &report.broken {
        rows.push((
            "broken visual binding".to_owned(),
            code(&binding.target),
            format!(
                "{} — {}",
                cell(&render::reason_phrase(binding)),
                cell(&binding.provenance)
            ),
        ));
    }
    rows
}

fn findings(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    let rows = rows(report);
    if rows.is_empty() {
        let line = if report.compare.is_some() {
            "No new findings."
        } else {
            "No findings."
        };
        writeln!(out, "{line}")?;
        return writeln!(out);
    }
    let heading = if report.compare.is_some() {
        "New findings"
    } else {
        "Findings"
    };
    writeln!(out, "#### {heading} ({})", rows.len())?;
    writeln!(out)?;
    // The Detail column only when some row has one: sizes need a storage
    // catalog, and a TMDL scan of unused objects alone has none.
    if rows.iter().any(|(_, _, detail)| !detail.is_empty()) {
        writeln!(out, "| Type | Object | Detail |")?;
        writeln!(out, "|---|---|---|")?;
        for (kind, id, detail) in rows.iter().take(MAX_ROWS) {
            writeln!(out, "| {kind} | {id} | {detail} |")?;
        }
    } else {
        writeln!(out, "| Type | Object |")?;
        writeln!(out, "|---|---|")?;
        for (kind, id, _) in rows.iter().take(MAX_ROWS) {
            writeln!(out, "| {kind} | {id} |")?;
        }
    }
    more(out, rows.len())?;
    writeln!(out)
}

/// The `Fixed since` list: findings of the other checkout this scan no longer
/// detects.
fn fixed(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    let Some(compare) = report.compare.as_ref().filter(|c| !c.fixed.is_empty()) else {
        return Ok(());
    };
    writeln!(
        out,
        "#### Fixed since {} ({})",
        code(&compare.root),
        compare.fixed.len()
    )?;
    writeln!(out)?;
    writeln!(out, "| Type | Object |")?;
    writeln!(out, "|---|---|")?;
    for entry in compare.fixed.iter().take(MAX_ROWS) {
        writeln!(
            out,
            "| {} | {} |",
            entry.kind.replace('_', " "),
            code(&entry.id)
        )?;
    }
    more(out, compare.fixed.len())?;
    if render::fixed_uncertain(report) {
        writeln!(out)?;
        writeln!(
            out,
            "> This scan skipped model objects it could not parse, so some of these may be \
             parse damage rather than removals. The scan log lists the skips."
        )?;
    }
    writeln!(out)
}

/// The tables carrying the most unused findings, as `--summary` ranks them.
fn worst_tables(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    let worst = render::worst_tables(&report.findings);
    if worst.rows.is_empty() {
        return Ok(());
    }
    writeln!(out, "#### Worst tables")?;
    writeln!(out)?;
    writeln!(out, "| Table | Unused |")?;
    writeln!(out, "|---|--:|")?;
    for (table, count) in &worst.rows {
        writeln!(out, "| {} | {count} |", code(table))?;
    }
    if worst.more > 0 {
        writeln!(out)?;
        writeln!(out, "…and {} more tables with findings.", worst.more)?;
    }
    writeln!(out)
}

/// What the lists leave out: `[scan].ignore` suppressions and parser skips.
fn notes(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    let mut notes = Vec::new();
    if report.ignored > 0 {
        notes.push(format!(
            "{} findings suppressed by `[scan].ignore`.",
            report.ignored
        ));
    }
    if !report.skips.is_empty() {
        notes.push(format!(
            "{} source objects skipped by the parser; the scan log lists them.",
            report.skips.len()
        ));
    }
    for note in notes {
        writeln!(out, "<sub>{note}</sub>")?;
        writeln!(out)?;
    }
    Ok(())
}

fn more(out: &mut dyn io::Write, total: usize) -> io::Result<()> {
    if total > MAX_ROWS {
        writeln!(out)?;
        writeln!(
            out,
            "…and {} more. Run `rib scan` for the full list.",
            total - MAX_ROWS
        )?;
    }
    Ok(())
}

/// `text` as an inline code span that is safe inside a table cell: the fence
/// is one backtick longer than any run inside, `|` is escaped (GFM splits
/// cells on it even inside code), and line breaks become spaces.
fn code(text: &str) -> String {
    let text = text.replace(['\r', '\n'], " ").replace('|', "\\|");
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// Plain text that is safe inside a table cell.
fn cell(text: &str) -> String {
    text.replace(['\r', '\n'], " ").replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_spans_survive_backticks_and_pipes() {
        assert_eq!(code("'Sales'[Amount]"), "`'Sales'[Amount]`");
        assert_eq!(code("a|b"), "`a\\|b`");
        assert_eq!(code("x`y"), "``x`y``");
        assert_eq!(code("`edge"), "`` `edge ``");
        assert_eq!(code("two\nlines"), "`two lines`");
    }
}
