//! Rendering: everything a scan prints to stdout, in the three output modes
//! from `docs/cli-ux-guidelines.md` — human text, `--plain`, and `--json`.
//! The full contract (shapes, examples, schema) lives in `docs/scan/` beside
//! this crate: `human-output.md`, `text-formats.md`, and `json.md`.

use std::io;
use std::time::Duration;

use ripbi_core::SizeBasis;

use crate::compare::Coverage;
use crate::style::Palette;

pub use ripbi_app::json::write as json;
pub use ripbi_app::output::{
    AutoDateTimeRow, BrokenArtifactOut, BrokenOut, CompareOut, Finding, GROUPS, KEEP_HINT, KINDS,
    ScanOutput, SkipNoticeOut, UnusedStorage, UsedByOut, WorstTables, format_bytes, kind_of,
    power_query_labels, reason_phrase, verdict_of, worst_tables,
};

/// Writes the human-readable report: findings grouped by object type with `←`
/// chain annotations, the breakage, auto date/time, and `Fixed since`
/// sections, then the verdict — last, because the last line is the one a
/// terminal leaves in view. `show_power_query` adds the
/// `⭘ Power Query also names it` annotations (hidden by default; `--json`
/// always carries the underlying field). `broken_only` — a lone `--broken` —
/// swaps the verdict headline for its broken phrasing.
///
/// # Errors
/// Propagates stream write failures.
pub fn human(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
    show_power_query: bool,
    broken_only: bool,
) -> io::Result<()> {
    if !report.findings.is_empty() {
        for (kind, label) in GROUPS {
            let group: Vec<&Finding> = report
                .findings
                .iter()
                .filter(|finding| finding.kind == *kind)
                .collect();
            if group.is_empty() {
                continue;
            }
            writeln!(
                out,
                "{}",
                palette.bold(&format!("{label} ({})", group.len()))
            )?;
            for finding in group {
                writeln!(out, "  {}{}", finding.id, size_suffix(palette, finding))?;
                write_annotations(out, palette, finding, "    ", show_power_query)?;
            }
            writeln!(out)?;
        }
    }
    write_broken_artifacts(out, palette, report)?;
    write_broken(out, palette, report)?;
    write_auto_date_time(out, palette, report, show_power_query)?;
    write_fixed(out, palette, report)?;
    write_verdict(out, palette, report, broken_only)
}

/// The `Fixed since <root>` section: findings of the other checkout that are
/// gone here.
fn write_fixed(out: &mut dyn io::Write, palette: &Palette, report: &ScanOutput) -> io::Result<()> {
    let Some(compare) = &report.compare else {
        return Ok(());
    };
    if compare.fixed.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "{}",
        palette.ok(&format!(
            "Fixed since {} ({})",
            compare.label,
            compare.fixed.len()
        ))
    )?;
    for entry in &compare.fixed {
        writeln!(
            out,
            "  {}  {}",
            entry.id,
            palette.dim(&entry.kind.replace('_', " "))
        )?;
    }
    if report.fixed_uncertain() {
        writeln!(out, "  {}", palette.warn(FIXED_CAVEAT))?;
    }
    writeln!(out)
}

/// The caveat a `Fixed since` list carries when this scan's ingest skipped
/// objects: a finding that vanished may have been lost to the parse, not
/// removed (issue #141).
pub(crate) const FIXED_CAVEAT: &str = "(this scan skipped model objects it could not parse — some \
                            of these may be parse damage, not removals; see the notices below)";

/// The count of unused findings a skipped baseline could not judge, worded
/// as incomplete coverage rather than as findings.
pub(crate) fn unverified_note(compare: &CompareOut) -> String {
    format!(
        "({} unused unverified — no connected reports in {}, so their usage there is unknown)",
        compare.unverified.len(),
        compare.label
    )
}

/// The verdict's headline and whether it is clean. Under a lone `--broken`
/// the run's scope is the broken bindings — the empty reachability list is
/// the filter's doing, not a result — so the headline speaks for them. Every
/// other selection keeps the reachability phrasing.
fn headline(report: &ScanOutput, broken_only: bool) -> (String, bool) {
    if broken_only {
        let broken = report.broken.len() + report.broken_artifacts.len();
        return if broken == 0 {
            ("No broken reports or artifacts".to_string(), true)
        } else {
            (
                format!("{broken} broken {}", plural(broken, "binding")),
                false,
            )
        };
    }
    match report.findings.len() {
        0 => ("No unused objects".to_string(), true),
        count => (format!("{count} unused {}", plural(count, "object")), false),
    }
}

/// `object` or `objects`.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_string()
    } else {
        format!("{noun}s")
    }
}

fn write_broken_artifacts(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
) -> io::Result<()> {
    if report.broken_artifacts.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "{}",
        palette.alert(&format!(
            "Broken artifacts ({})",
            report.broken_artifacts.len()
        ))
    )?;
    for artifact in &report.broken_artifacts {
        match &artifact.report {
            Some(path) => writeln!(out, "  {} — in report {path}", artifact.id)?,
            None => writeln!(out, "  {}", artifact.id)?,
        }
        for reference in &artifact.unresolved_references {
            writeln!(
                out,
                "{}",
                palette.dim(&format!("    ← {reference} not found"))
            )?;
        }
    }
    writeln!(out)
}

/// The broken-visual section (issue #60), rendered in both human modes after
/// the reachability findings: like the auto date/time section, a different
/// verdict (written references, not reachability) — and advisory, so it
/// survives even a clean "No unused objects" scan. The `--broken` flag is
/// what turns it into a gate.
fn write_broken(out: &mut dyn io::Write, palette: &Palette, report: &ScanOutput) -> io::Result<()> {
    if report.broken.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "{}",
        palette.alert(&format!("Broken visual bindings ({})", report.broken.len()))
    )?;
    for binding in &report.broken {
        writeln!(out, "  {}", binding.target)?;
        writeln!(
            out,
            "{}",
            palette.dim(&format!(
                "    ← {} — {}",
                reason_phrase(binding),
                binding.location
            ))
        )?;
    }
    writeln!(out)
}

/// The auto date/time section, rendered in both human modes after the
/// reachability findings: a different verdict (report bindings, not
/// reachability), so it survives even a clean "No unused objects" scan.
fn write_auto_date_time(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
    show_power_query: bool,
) -> io::Result<()> {
    if report.auto_date_time.is_empty() {
        return Ok(());
    }
    const SECTIONS: &[(&str, &str)] = &[
        ("in_use", "in use — replace with a real date table"),
        (
            "unused_by_reports",
            "unused by reports — disable auto date/time",
        ),
        ("dead", "dead"),
    ];
    let actionable = report
        .auto_date_time
        .iter()
        .any(|row| row.verdict != "in_use");
    let header = format!("Auto date/time ({})", report.auto_date_time.len());
    writeln!(
        out,
        "{}",
        if actionable {
            palette.warn(&header)
        } else {
            palette.bold(&header)
        }
    )?;
    for (verdict, label) in SECTIONS {
        let group: Vec<&AutoDateTimeRow> = report
            .auto_date_time
            .iter()
            .filter(|row| row.verdict == *verdict)
            .collect();
        if group.is_empty() {
            continue;
        }
        writeln!(out, "  {label}:")?;
        for row in group {
            let source = row
                .source_column
                .as_deref()
                .map(|column| format!(" — for {column}"))
                .unwrap_or_default();
            let size = row
                .finding
                .as_ref()
                .map(|finding| size_suffix(palette, finding))
                .unwrap_or_default();
            writeln!(out, "    {}{source}{size}", row.id)?;
            if let Some(finding) = &row.finding {
                write_annotations(out, palette, finding, "      ", show_power_query)?;
            }
        }
    }
    writeln!(out)
}

/// Writes the `Worst tables:` block of `--summary` (issue #38): a bounded list of
/// the tables carrying the most surviving findings. Nothing prints when every
/// surviving finding is table-less.
fn write_worst_tables(
    out: &mut dyn io::Write,
    palette: &Palette,
    findings: &[Finding],
) -> io::Result<()> {
    let worst = worst_tables(findings);
    if worst.rows.is_empty() {
        return Ok(());
    }
    let name_width = worst
        .rows
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let count_width = worst
        .rows
        .iter()
        .map(|(_, count)| count.to_string().len())
        .max()
        .unwrap_or(0);
    writeln!(out)?;
    writeln!(out, "{}", palette.bold("Worst tables:"))?;
    for (name, count) in &worst.rows {
        writeln!(out, "  {name:<name_width$}  {count:>count_width$}")?;
    }
    if worst.more > 0 {
        writeln!(out, "  ... and {} more tables with findings", worst.more)?;
    }
    Ok(())
}

/// Writes `--summary` output: the summary line and per-type totals, without
/// the findings list — the shape for models with thousands of findings.
/// `broken_only` swaps the empty-findings placeholder the way `human` does.
///
/// # Errors
/// Propagates stream write failures.
pub fn human_summary(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
    broken_only: bool,
) -> io::Result<()> {
    if !report.findings.is_empty() {
        for (kind, label) in GROUPS {
            let count = report
                .findings
                .iter()
                .filter(|finding| finding.kind == *kind)
                .count();
            if count > 0 {
                writeln!(out, "{}: {}", palette.bold(label), count)?;
            }
        }
        write_worst_tables(out, palette, &report.findings)?;
    }
    if !report.broken.is_empty() {
        writeln!(
            out,
            "{}: {}",
            palette.alert("Broken visual bindings"),
            report.broken.len()
        )?;
    }
    if !report.broken_artifacts.is_empty() {
        writeln!(
            out,
            "{}: {}",
            palette.alert("Broken artifacts"),
            report.broken_artifacts.len()
        )?;
    }
    if let Some(compare) = report.compare.as_ref().filter(|c| !c.fixed.is_empty()) {
        writeln!(
            out,
            "{}: {}",
            palette.ok(&format!("Fixed since {}", compare.label)),
            compare.fixed.len()
        )?;
        if report.fixed_uncertain() {
            writeln!(out, "  {}", palette.warn(FIXED_CAVEAT))?;
        }
    }
    if !report.auto_date_time.is_empty() {
        write_auto_date_time_summary(out, palette, report)?;
    }
    // The blank line separates the counts from the verdict; with no counts
    // the one stderr already wrote under the notes is the separator.
    let has_counts = !report.findings.is_empty()
        || !report.broken.is_empty()
        || !report.broken_artifacts.is_empty()
        || report.compare.as_ref().is_some_and(|c| !c.fixed.is_empty())
        || !report.auto_date_time.is_empty();
    if has_counts {
        writeln!(out)?;
    }
    write_verdict(out, palette, report, broken_only)
}

/// The `Auto date/time: …` line of `--summary`.
fn write_auto_date_time_summary(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
) -> io::Result<()> {
    let mut counts = Vec::new();
    for (verdict, label) in [
        ("in_use", "in use"),
        ("unused_by_reports", "unused by reports"),
        ("dead", "dead"),
    ] {
        let count = report
            .auto_date_time
            .iter()
            .filter(|row| row.verdict == verdict)
            .count();
        if count > 0 {
            counts.push(format!("{count} {label}"));
        }
    }
    // Issue #47: the aggregation is the machinery and the date columns it
    // serves, with the verdicts as the breakdown. The shared template table
    // pairs with no column, so the scope clause only makes sense when at
    // least one column resolves.
    let hidden_tables = report.auto_date_time.len();
    let date_columns = report.date_columns();
    let scope = if date_columns > 0 {
        format!("{hidden_tables} hidden tables over {date_columns} date columns")
    } else {
        format!("{hidden_tables} hidden tables")
    };
    writeln!(
        out,
        "{}: {} ({})",
        palette.bold("Auto date/time"),
        scope,
        counts.join(", ")
    )
}

/// The verdict both human modes close with: `✓ No unused objects` or
/// `✗ 12 unused objects`, the reachability counts beside it, then — indented
/// under it — the unused storage and the suppression notes that explain the
/// arithmetic.
fn write_verdict(
    out: &mut dyn io::Write,
    palette: &Palette,
    report: &ScanOutput,
    broken_only: bool,
) -> io::Result<()> {
    let (headline, clean) = headline(report, broken_only);
    let mark = if clean {
        palette.ok(&format!("\u{2713} {headline}"))
    } else {
        palette.warn(&format!("\u{2717} {headline}"))
    };
    let elapsed = report
        .elapsed
        .map(|elapsed| format!("  {}", palette.dim(&format_elapsed(elapsed))))
        .unwrap_or_default();
    writeln!(
        out,
        "{mark}   {} of {} reachable from {} roots{elapsed}",
        report.reachable, report.objects, report.roots,
    )?;
    if let Some(storage) = &report.unused_storage {
        let of = report
            .model_bytes
            .map(|total| format!(" of {}", format_bytes(total)))
            .unwrap_or_default();
        let (approx, note) = if storage.lower_bound {
            (
                "at least",
                "; a lower bound: some files are missing from the backup log",
            )
        } else {
            ("\u{2248}", "")
        };
        let place = if storage.in_memory {
            "in memory"
        } else {
            "on disk"
        };
        writeln!(
            out,
            "  Unused storage: {approx} {}{of} {place} ({} objects with size data{note})",
            palette.bold(&format_bytes(storage.bytes)),
            storage.objects,
        )?;
    }
    let mut notes = Vec::new();
    if let Some(compare) = &report.compare {
        notes.push(format!(
            "({} findings already in {})",
            compare.existing, compare.label
        ));
        if compare.coverage == Coverage::Skipped {
            notes.push(unverified_note(compare));
        }
    }
    if report.ignored > 0 {
        notes.push(format!(
            "({} findings suppressed by [scan].ignore)",
            report.ignored
        ));
    }
    if report.filtered_out > 0 {
        notes.push(format!(
            "({} unused hidden by type filters)",
            report.filtered_out
        ));
    }
    if report.broken_hidden > 0 {
        notes.push(format!(
            "({} broken-visual bindings hidden by type filters)",
            report.broken_hidden
        ));
    }
    if report.broken_artifacts_hidden > 0 {
        notes.push(format!(
            "({} broken artifacts hidden by type filters)",
            report.broken_artifacts_hidden
        ));
    }
    if report.broken_suppressed > 0 {
        notes.push(format!(
            "({} possible broken-visual bindings suppressed — the model ingest \
             reported skips, listed on stderr; --strict fails on those skips)",
            report.broken_suppressed
        ));
    }
    if report.broken_artifacts_suppressed > 0 {
        notes.push(format!(
            "({} possible broken artifacts suppressed — the model ingest reported skips, \
             listed on stderr; --strict fails on those skips)",
            report.broken_artifacts_suppressed
        ));
    }
    if report.machinery_members > 0 {
        notes.push(format!(
            "({} unused auto date/time members covered by their tables' verdicts)",
            report.machinery_members
        ));
    }
    for note in &notes {
        writeln!(out, "  {}", palette.muted(note))?;
    }
    Ok(())
}

/// `0.8s`, `12.4s`, `2m 05s`.
pub fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let whole = elapsed.as_secs();
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}

/// `  (1.2 MB)` after a finding's id, when its size is known.
fn size_suffix(palette: &Palette, finding: &Finding) -> String {
    let Some(stats) = finding.storage else {
        return String::new();
    };
    let Some(bytes) = stats.bytes else {
        return String::new();
    };
    let bound = if stats.basis == SizeBasis::LowerBound {
        "\u{2265} "
    } else {
        ""
    };
    format!(
        "  {}",
        palette.dim(&format!("({bound}{})", format_bytes(bytes)))
    )
}

fn write_annotations(
    out: &mut dyn io::Write,
    palette: &Palette,
    finding: &Finding,
    indent: &str,
    show_power_query: bool,
) -> io::Result<()> {
    if finding.used_by.is_empty() {
        writeln!(
            out,
            "{}",
            palette.dim(&format!("{indent}← nothing references it"))
        )?;
    } else {
        let only = finding.used_by.len() == 1;
        for used in &finding.used_by {
            let prefix = if only { "only " } else { "" };
            let also = if used.also_unused {
                " (also unused)"
            } else {
                ""
            };
            writeln!(
                out,
                "{}",
                palette.dim(&format!(
                    "{indent}← {prefix}used by {} — {}{also}",
                    used.id, used.provenance
                ))
            )?;
        }
    }
    // Cleanup-time supply-chain guidance, not verdict information: hidden
    // unless asked for (--power-query). The JSON field is unconditional.
    if show_power_query && !finding.named_in_power_query.is_empty() {
        let named = finding.named_in_power_query.join(", ");
        writeln!(
            out,
            "{}",
            palette.dim(&format!(
                "{indent}⭘ Power Query also names it ({named}) — safe to stop loading; \
                 removing it from the script means editing those steps too"
            ))
        )?;
    }
    Ok(())
}

/// Writes `--plain` output: one `<type>\t<id>` record per finding, one
/// `broken_visual:<reason>\t<target>` record per reported broken binding, a
/// `broken_artifact\t<id>` record per DAX owner (with a report-path field for
/// report measures), then
/// one `auto_date_time:<verdict>\t<id>` record per auto date/time table.
///
/// # Errors
/// Propagates stream write failures.
pub fn plain(out: &mut dyn io::Write, report: &ScanOutput) -> io::Result<()> {
    for finding in &report.findings {
        match finding.storage.and_then(|stats| stats.bytes) {
            Some(bytes) => writeln!(out, "{}\t{}\t{bytes}", finding.kind, finding.id)?,
            None => writeln!(out, "{}\t{}", finding.kind, finding.id)?,
        }
    }
    for binding in &report.broken {
        writeln!(out, "broken_visual:{}\t{}", binding.reason, binding.target)?;
    }
    for artifact in &report.broken_artifacts {
        match &artifact.report {
            Some(path) => writeln!(out, "broken_artifact\t{}\t{path}", artifact.id)?,
            None => writeln!(out, "broken_artifact\t{}", artifact.id)?,
        }
    }
    for row in &report.auto_date_time {
        writeln!(out, "auto_date_time:{}\t{}", row.verdict, row.id)?;
    }
    if let Some(compare) = &report.compare {
        for entry in &compare.fixed {
            writeln!(out, "fixed:{}\t{}", entry.kind, entry.id)?;
        }
        for entry in &compare.unverified {
            writeln!(out, "unverified:{}\t{}", entry.kind, entry.id)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ripbi_core::StorageStats;

    use super::*;

    fn finding(table: Option<&str>) -> Finding {
        Finding {
            kind: "measure",
            id: String::new(),
            table: table.map(ripbi_core::NameKey::new),
            used_by: Vec::new(),
            named_in_power_query: Vec::new(),
            storage: None,
        }
    }

    #[test]
    fn orders_by_count_then_table_name() {
        let findings = [
            finding(Some("B")),
            finding(Some("A")),
            finding(Some("A")),
            finding(Some("C")),
            finding(Some("C")),
            finding(Some("C")),
        ];

        let worst = worst_tables(&findings);

        assert_eq!(
            worst.rows,
            vec![
                ("'C'".to_string(), 3),
                ("'A'".to_string(), 2),
                ("'B'".to_string(), 1),
            ]
        );
        assert_eq!(worst.more, 0);
    }

    /// The tie-break follows the model's case-insensitive identity ordering, not
    /// the display bytes: `apple` precedes `Zebra` at equal counts.
    #[test]
    fn tie_breaks_on_the_folded_table_name() {
        let findings = [
            finding(Some("Zebra")),
            finding(Some("Zebra")),
            finding(Some("apple")),
            finding(Some("apple")),
        ];

        let worst = worst_tables(&findings);

        assert_eq!(
            worst.rows,
            vec![("'apple'".to_string(), 2), ("'Zebra'".to_string(), 2)]
        );
    }

    #[test]
    fn skips_table_less_findings() {
        let findings = [finding(None), finding(None), finding(Some("Sales"))];

        let worst = worst_tables(&findings);

        assert_eq!(worst.rows, vec![("'Sales'".to_string(), 1)]);
        assert_eq!(worst.more, 0);
    }

    #[test]
    fn is_empty_when_every_finding_is_table_less() {
        let findings = [finding(None), finding(None)];

        let worst = worst_tables(&findings);

        assert!(worst.rows.is_empty());
        assert_eq!(worst.more, 0);
    }

    #[test]
    fn caps_the_rows_and_counts_the_rest() {
        let findings: Vec<Finding> = (0..ripbi_app::output::WORST_TABLES_LIMIT + 3)
            .map(|i| finding(Some(&format!("T{i:02}"))))
            .collect();

        let worst = worst_tables(&findings);

        assert_eq!(worst.rows.len(), ripbi_app::output::WORST_TABLES_LIMIT);
        assert_eq!(worst.more, 3);
    }

    fn scan_output(findings: Vec<Finding>) -> ScanOutput {
        ScanOutput {
            target: String::new(),
            reports: Vec::new(),
            objects: 0,
            reachable: 0,
            roots: 0,
            unused_raw: findings.len(),
            ignored: 0,
            kept: 0,
            queried: 0,
            filtered_out: 0,
            machinery_members: 0,
            broken: Vec::new(),
            broken_hidden: 0,
            broken_suppressed: 0,
            broken_raw: 0,
            broken_artifacts: Vec::new(),
            broken_artifacts_hidden: 0,
            broken_artifacts_suppressed: 0,
            broken_artifacts_raw: 0,
            findings,
            auto_date_time: Vec::new(),
            skips: Vec::new(),
            model_bytes: None,
            unused_storage: None,
            stats_source: None,
            compare: None,
            elapsed: None,
        }
    }

    #[test]
    fn the_summary_prints_a_padded_worst_tables_block() {
        let findings = vec![
            finding(Some("Sales")),
            finding(Some("Sales")),
            finding(Some("Sales")),
            finding(Some("Customer")),
            finding(Some("Customer")),
            finding(None),
        ];

        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &scan_output(findings), false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("\nWorst tables:\n  'Sales'     3\n  'Customer'  2\n"),
            "the block is blank-line separated, count descending, columns aligned:\n{text}"
        );
    }

    #[test]
    fn the_summary_announces_tables_beyond_the_top() {
        let findings: Vec<Finding> = (0..ripbi_app::output::WORST_TABLES_LIMIT + 2)
            .map(|i| finding(Some(&format!("T{i:02}"))))
            .collect();

        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &scan_output(findings), false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("  ... and 2 more tables with findings\n"),
            "the cut is announced:\n{text}"
        );
    }

    #[test]
    fn no_worst_tables_block_when_nothing_has_a_table() {
        let mut out = Vec::new();
        human_summary(
            &mut out,
            &Palette::plain(),
            &scan_output(vec![finding(None)]),
            false,
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains("Worst tables"),
            "table-less findings belong to no table:\n{text}"
        );
    }

    #[test]
    fn json_carries_the_quoted_table_and_nulls_table_less_kinds() {
        let mut tabled = finding(Some("Sales"));
        tabled.id = "'Sales'[Legacy Total]".to_string();

        let mut out = Vec::new();
        json(&mut out, &scan_output(vec![tabled, finding(None)])).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["unused"][0]["table"], "'Sales'");
        assert!(value["unused"][1]["table"].is_null());
    }

    fn auto_row(verdict: &'static str, id: &str, source_column: Option<&str>) -> AutoDateTimeRow {
        AutoDateTimeRow {
            verdict,
            id: id.to_string(),
            source_column: source_column.map(str::to_string),
            finding: None,
        }
    }

    fn scan_output_with_auto_date_time(
        findings: Vec<Finding>,
        auto_date_time: Vec<AutoDateTimeRow>,
    ) -> ScanOutput {
        let mut output = scan_output(findings);
        output.auto_date_time = auto_date_time;
        output
    }

    /// Issue #47: the summary aggregates the machinery and the date columns it
    /// serves, with the verdicts as the breakdown. Columns count distinctly —
    /// two tables over one column — and the shared template table, which
    /// serves no column, still counts toward the tables.
    #[test]
    fn the_summary_counts_hidden_tables_over_distinct_date_columns() {
        let rows = vec![
            auto_row(
                "in_use",
                "table 'LocalDateTable_a'",
                Some("'Opportunity Calendar'[Date]"),
            ),
            auto_row(
                "dead",
                "table 'LocalDateTable_b'",
                Some("'Opportunity'[Date]"),
            ),
            auto_row(
                "dead",
                "table 'LocalDateTable_c'",
                Some("'Opportunity'[Date]"),
            ),
            auto_row("dead", "table 'DateTableTemplate_d'", None),
        ];

        let mut out = Vec::new();
        human_summary(
            &mut out,
            &Palette::plain(),
            &scan_output_with_auto_date_time(vec![], rows),
            false,
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(
                "Auto date/time: 4 hidden tables over 2 date columns (1 in use, 3 dead)\n"
            ),
            "the aggregation names the machinery, the columns, and the verdicts:\n{text}"
        );
    }

    /// A model whose machinery pairs with no column at all (only the template,
    /// or unresolvable pairings) still aggregates — without a scope clause.
    #[test]
    fn the_summary_drops_the_column_clause_when_no_column_resolves() {
        let rows = vec![
            auto_row("dead", "table 'LocalDateTable_a'", None),
            auto_row("dead", "table 'DateTableTemplate_b'", None),
        ];

        let mut out = Vec::new();
        human_summary(
            &mut out,
            &Palette::plain(),
            &scan_output_with_auto_date_time(vec![], rows),
            false,
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("Auto date/time: 2 hidden tables (2 dead)\n"),
            "no column clause without a resolvable column:\n{text}"
        );
    }

    /// The members the section covers leave a gap in the summary line's
    /// arithmetic (objects ≠ reachable + unused), so the note explains it —
    /// the same convention as the `[scan].ignore` and type-filter notes.
    #[test]
    fn the_summary_explains_the_machinery_members_gap() {
        let mut output = scan_output_with_auto_date_time(
            vec![],
            vec![auto_row(
                "dead",
                "table 'LocalDateTable_a'",
                Some("'Opportunity'[Date]"),
            )],
        );
        output.machinery_members = 8;

        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &output, false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("(8 unused auto date/time members covered by their tables' verdicts)\n"),
            "the covered members are accounted for:\n{text}"
        );
    }

    #[test]
    fn json_counts_the_machinery_and_its_date_columns() {
        let rows = vec![
            auto_row(
                "dead",
                "table 'LocalDateTable_a'",
                Some("'Opportunity'[Date]"),
            ),
            auto_row(
                "dead",
                "table 'LocalDateTable_b'",
                Some("'Opportunity'[Date]"),
            ),
            auto_row("dead", "table 'DateTableTemplate_c'", None),
        ];
        let mut output = scan_output_with_auto_date_time(vec![], rows);
        output.machinery_members = 12;

        let mut out = Vec::new();
        json(&mut out, &output).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let counts = &value["summary"]["auto_date_time"];
        assert_eq!(counts["hidden_tables"], 3);
        assert_eq!(counts["date_columns"], 1);
        assert_eq!(counts["member_findings"], 12);
        assert_eq!(counts["dead"], 3);
    }

    fn broken(target: &str, reason: &'static str, artifact: Option<&str>) -> BrokenOut {
        BrokenOut {
            target: target.to_string(),
            reason,
            bound_artifact: artifact.map(str::to_string),
            bound_artifact_report: None,
            provenance: "field well 'Values' — visual 'V1' on page 'P1' in report 'Mini'"
                .to_string(),
            location: "field well 'Values' — card visual 'Revenue' (id 'V1') on page \
                       'Overview' (id 'P1') in report 'Mini'"
                .to_string(),
        }
    }

    /// Issue #60: the section renders — with the reason phrase and the
    /// binding site — even when the scan is otherwise clean, because it is a
    /// different verdict than reachability's.
    #[test]
    fn the_broken_section_renders_on_an_otherwise_clean_scan() {
        let mut output = scan_output(vec![]);
        output.broken = vec![broken("'Sales'[Color]", "field_not_found", None)];

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("Broken visual bindings (1)\n  'Sales'[Color]\n    ← field not found in the model — field well 'Values' — card visual 'Revenue' (id 'V1') on page 'Overview' (id 'P1') in report 'Mini'\n"),
            "the section names the field, the reason, and the site:\n{text}"
        );
        assert!(
            text.ends_with("\u{2713} No unused objects   0 of 0 reachable from 0 roots\n"),
            "the verdict is the last line:\n{text}"
        );
    }

    /// The verdict closes the report, after every section, so a terminal
    /// leaves it in view; with findings it counts them.
    #[test]
    fn the_verdict_is_the_last_line_and_counts_the_findings() {
        let mut output = scan_output(vec![sized(4096, SizeBasis::Files)]);
        output.objects = 7;
        output.reachable = 6;
        output.roots = 2;

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, false).unwrap();

        let text = String::from_utf8(out).unwrap();
        let verdict = text
            .lines()
            .position(|line| {
                line.starts_with("\u{2717} 1 unused object   6 of 7 reachable from 2 roots")
            })
            .unwrap_or_else(|| panic!("no verdict line:\n{text}"));
        let rest: Vec<&str> = text.lines().skip(verdict + 1).collect();
        assert!(
            rest.iter().all(|line| line.starts_with("  ")),
            "only the verdict's own indented details follow it:\n{text}"
        );
    }

    /// Under a lone `--broken` the verdict speaks for the scope the flag
    /// asked about: nothing flags reads "No broken reports or artifacts", never the
    /// reachability phrasing — the empty findings list is the filter's doing.
    #[test]
    fn a_broken_only_clean_scan_places_the_no_broken_reports_line() {
        let mut out = Vec::new();
        human(
            &mut out,
            &Palette::plain(),
            &scan_output(vec![]),
            false,
            true,
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("\u{2713} No broken reports or artifacts"),
            "the verdict names the scope:\n{text}"
        );
        assert!(
            !text.contains("No unused objects"),
            "the run never scoped reachability:\n{text}"
        );
    }

    /// With rows in the section, the verdict counts them — "No unused
    /// objects" under the bindings would read like a verdict the flag never
    /// asked for.
    #[test]
    fn a_broken_only_scan_with_rows_counts_them_in_the_verdict() {
        let mut output = scan_output(vec![]);
        output.broken = vec![broken("'Sales'[Color]", "field_not_found", None)];

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, true).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Broken visual bindings (1)"), ":\n{text}");
        assert!(text.contains("\u{2717} 1 broken binding"), ":\n{text}");
        assert!(!text.contains("No unused objects"), ":\n{text}");
        assert!(
            !text.contains("No broken reports or artifacts"),
            ":\n{text}"
        );
    }

    /// `--summary` shares the verdict: one phrasing per scope, both human
    /// modes.
    #[test]
    fn the_summary_shares_the_broken_only_placeholder() {
        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &scan_output(vec![]), true).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("No broken reports or artifacts"), ":\n{text}");
    }

    /// A clean `--summary` is the verdict alone: the blank line stderr writes
    /// under the notes already separates it, so stdout adds no second one.
    /// With counts, one blank line still separates them from the verdict.
    #[test]
    fn the_summary_separates_the_verdict_once() {
        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &scan_output(vec![]), false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\u{2713} No unused objects"), ":\n{text}");

        let mut out = Vec::new();
        human_summary(
            &mut out,
            &Palette::plain(),
            &scan_output(vec![finding(None)]),
            false,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\n\n\u{2717} 1 unused object"), ":\n{text}");
        assert!(!text.contains("\n\n\n"), ":\n{text}");
    }

    /// The propagated reason names the artifact: the visual is what a user
    /// sees, but the measure is what they fix.
    #[test]
    fn a_broken_artifact_reason_names_the_artifact() {
        let mut output = scan_output(vec![]);
        output.broken = vec![broken(
            "'Sales'[Broken]",
            "bound_artifact_broken",
            Some("'Sales'[Broken Total]"),
        )];

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("← bound artifact 'Sales'[Broken Total] has unresolvable references"),
            "the artifact is named:\n{text}"
        );
    }

    #[test]
    fn plain_carries_broken_visual_records_with_their_reason() {
        let mut output = scan_output(vec![]);
        output.broken = vec![
            broken("'Sales'[Color]", "field_not_found", None),
            broken(
                "'Sales'[Broken]",
                "bound_artifact_broken",
                Some("'Sales'[T]"),
            ),
        ];

        let mut out = Vec::new();
        plain(&mut out, &output).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("broken_visual:field_not_found\t'Sales'[Color]\n"),
            "one record per binding, reason in the kind prefix:\n{text}"
        );
        assert!(
            text.contains("broken_visual:bound_artifact_broken\t'Sales'[Broken]\n"),
            "{text}"
        );
    }

    #[test]
    fn json_carries_broken_bindings_and_their_totals() {
        let mut output = scan_output(vec![]);
        output.broken = vec![broken("'Sales'[Color]", "field_not_found", None)];
        output.broken_raw = 3;

        let mut out = Vec::new();
        json(&mut out, &output).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["summary"]["broken"], 1);
        assert_eq!(value["summary"]["broken_total"], 3);
        assert_eq!(value["broken"][0]["target"], "'Sales'[Color]");
        assert_eq!(value["broken"][0]["reason"], "field_not_found");
        assert!(value["broken"][0]["bound_artifact"].is_null());
        assert_eq!(
            value["broken"][0]["provenance"],
            "field well 'Values' — visual 'V1' on page 'P1' in report 'Mini'"
        );
        assert_eq!(
            value["broken"][0]["location"],
            "field well 'Values' — card visual 'Revenue' (id 'V1') on page 'Overview' (id 'P1') \
             in report 'Mini'"
        );
    }

    /// The suppression note explains findings the reader cannot see — the
    /// issue #60 precision bar — and survives a clean scan.
    #[test]
    fn the_summary_explains_suppressed_broken_bindings() {
        let mut output = scan_output(vec![]);
        output.broken_suppressed = 2;

        let mut out = Vec::new();
        human_summary(&mut out, &Palette::plain(), &output, false).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("(2 possible broken-visual bindings suppressed — the model ingest reported skips, listed on stderr; --strict fails on those skips)\n"),
            "the suppression is accounted for:\n{text}"
        );
    }

    #[test]
    fn bytes_format_in_1024_based_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(326_135), "318.5 KB");
        assert_eq!(format_bytes(4_404_019), "4.2 MB");
        assert_eq!(format_bytes(3 << 30), "3.0 GB");
    }

    fn sized(bytes: u64, basis: SizeBasis) -> Finding {
        Finding {
            kind: "column",
            id: "'Sales'[Comment]".to_string(),
            storage: Some(StorageStats {
                bytes: Some(bytes),
                basis,
                rows: Some(10),
                cardinality: Some(3),
            }),
            ..finding(Some("Sales"))
        }
    }

    /// Issue #122: sizes print beside findings and as a total line; a lower
    /// bound says so in both places.
    #[test]
    fn sizes_print_beside_findings_and_as_a_total() {
        let mut output = scan_output(vec![sized(2048, SizeBasis::Files)]);
        output.model_bytes = Some(1 << 20);
        output.unused_storage = Some(UnusedStorage {
            bytes: 2048,
            objects: 1,
            lower_bound: false,
            in_memory: false,
        });
        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(
                "Unused storage: \u{2248} 2.0 KB of 1.0 MB on disk (1 objects with size data)\n"
            ),
            "{text}"
        );
        assert!(text.contains("  'Sales'[Comment]  (2.0 KB)\n"), "{text}");

        output.findings = vec![sized(2048, SizeBasis::LowerBound)];
        output.unused_storage = output.unused_storage.map(|storage| UnusedStorage {
            lower_bound: true,
            ..storage
        });
        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("Unused storage: at least 2.0 KB of 1.0 MB"),
            "{text}"
        );
        assert!(
            text.contains("a lower bound: some files are missing"),
            "{text}"
        );
        assert!(
            text.contains("  'Sales'[Comment]  (\u{2265} 2.0 KB)\n"),
            "{text}"
        );

        let mut out = Vec::new();
        json(&mut out, &output).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["summary"]["unused_bytes"], 2048);
        assert_eq!(value["summary"]["unused_bytes_lower_bound"], true);
        assert_eq!(value["unused"][0]["size_basis"], "lower_bound");
        assert_eq!(value["unused"][0]["cardinality"], 3);

        let mut out = Vec::new();
        plain(&mut out, &output).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "column\t'Sales'[Comment]\t2048\n"
        );

        // Issue #108: `.vpax` sizes are the engine's, not files on disk.
        output.findings = vec![sized(2048, SizeBasis::Engine)];
        output.unused_storage = output.unused_storage.map(|storage| UnusedStorage {
            lower_bound: false,
            in_memory: true,
            ..storage
        });
        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("Unused storage: \u{2248} 2.0 KB of 1.0 MB in memory (1 objects"),
            "{text}"
        );
        assert!(text.contains("  'Sales'[Comment]  (2.0 KB)\n"), "{text}");
    }

    /// Without storage data nothing new prints: the pre-#122 shapes.
    #[test]
    fn no_storage_adds_nothing() {
        let output = scan_output(vec![finding(Some("Sales"))]);
        let mut out = Vec::new();
        json(&mut out, &output).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        for key in ["unused_bytes", "unused_bytes_lower_bound", "model_bytes"] {
            assert!(value["summary"].get(key).is_none(), "{key}");
        }
        for key in ["bytes", "size_basis", "rows", "cardinality"] {
            assert!(value["unused"][0].get(key).is_none(), "{key}");
        }
        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &output, false, false).unwrap();
        assert!(!String::from_utf8(out).unwrap().contains("storage"));
    }
}
