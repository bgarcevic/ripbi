//! Rendering for the `report` command: everything it prints to stdout — the
//! human tree, the list views (`--pages`, `--visuals`, `--fields`, `--used`),
//! `--plain` records, and `--json`. The full contract (shapes, examples,
//! schema) lives in `docs/report.md` beside this crate.

use std::io;

use serde::Serialize;

use crate::render::SkipNoticeOut;
use crate::style::Palette;

/// Everything the `report` command wants to say on stdout, as
/// presentation-ready data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Inventory {
    /// The model the reports were paired with, as given.
    pub target: String,
    /// One node per ingested report, in ingestion order.
    pub reports: Vec<ReportNode>,
    /// Every skip notice ingestion recorded.
    pub skips: Vec<SkipNoticeOut>,
    /// Every model object reachability keeps alive, sorted by kind then id.
    /// Populated only when the `--used` view runs.
    pub used: Vec<UsedRow>,
}

/// One model object the report keeps alive — the complement of scan's unused
/// findings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsedRow {
    /// The object kind, in scan's vocabulary (`measure`, `column`, …).
    pub kind: &'static str,
    /// The display id, e.g. `'Sales'[Total]`.
    pub id: String,
}

/// One report's inventory: its filters, pages, bookmarks, and report measures.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ReportNode {
    /// The report item's path, as given.
    pub path: String,
    /// The report's display name (`ReportModel::name`), or its folder name.
    pub name: String,
    /// Report-level filters.
    pub filters: Vec<FilterNode>,
    /// Broken live bindings at report level (report filters, report-measure
    /// references). Bookmark-saved breakage is out of scope.
    pub unresolved: Vec<UnresolvedNode>,
    /// Desktop pages first, then phone-layout pages tagged `mobile`.
    pub pages: Vec<PageNode>,
    /// How many bookmarks the report carries (v1 lists the count only).
    pub bookmarks: usize,
    /// Report-level measure names (`reportExtensions.json`).
    pub report_measures: Vec<String>,
    /// The itemized unresolved bindings: every visual-, page-, and
    /// report-level row this report carries.
    pub unresolved_count: usize,
}

/// One page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PageNode {
    /// The page's object name (the PBIR folder name).
    pub name: String,
    /// The author-facing display name, when the page carries one.
    pub display_name: Option<String>,
    /// Display-only, never liveness.
    pub hidden: bool,
    /// True for phone-layout pages (`definition.mobile/`, issue #49).
    pub mobile: bool,
    /// Page-level filters.
    pub filters: Vec<FilterNode>,
    /// Broken live bindings at page level (page filters, drillthrough
    /// parameters).
    pub unresolved: Vec<UnresolvedNode>,
    /// The page's visuals, in source order.
    pub visuals: Vec<VisualNode>,
}

/// One visual: every model object it references, and where each binds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct VisualNode {
    /// The visual's object name (the PBIR folder name).
    pub name: String,
    /// The visual type as written, e.g. `"card"`, `"donutChart"`.
    pub visual_type: String,
    /// Field wells, sorts, conditional formatting, and alt text — the visual's
    /// references outside its filters.
    pub fields: Vec<FieldNode>,
    /// Visual-level filters, persisted automatic filters included.
    pub filters: Vec<FilterNode>,
    /// References that resolve to nothing in the model, in `scan`'s
    /// broken-binding vocabulary.
    pub unresolved: Vec<UnresolvedNode>,
}

/// One field reference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct FieldNode {
    /// `column`, `measure`, `hierarchy_level`, `aggregation`, or `written` —
    /// the binding's own claim, not a resolution verdict.
    pub kind: &'static str,
    /// The written reference, e.g. `'Sales'[Total]`.
    pub target: String,
    /// Where the reference binds: `field_well:<role>`, `sort`,
    /// `conditional_formatting`, or `alt_text`.
    pub binding_site: String,
    /// Whether a field-well projection is active. Inactive projections still
    /// bind. `None` outside field wells.
    pub active: Option<bool>,
}

/// One filter at report, page, or visual level.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct FilterNode {
    /// The filter's in-scope name, e.g. `Filter5`.
    pub name: Option<String>,
    /// The author-facing name, when the filter carries one.
    pub display_name: Option<String>,
    /// The filter kind as written, e.g. `Categorical` or `Advanced`.
    pub filter_type: Option<String>,
    /// The declared filtered field, when the filter states one.
    pub target: Option<String>,
    /// Further fields the condition tree references.
    pub references: Vec<String>,
}

/// One reference that resolves to nothing in the model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct UnresolvedNode {
    /// The written reference, e.g. `'Sales'[Color]`.
    pub target: String,
    /// Why it does not resolve — snake_case, the same vocabulary as `scan`'s
    /// broken bindings.
    pub reason: &'static str,
    /// The broken artifact the binding lands on, for the
    /// `bound_artifact_broken` reason; `None` otherwise.
    pub artifact: Option<String>,
    /// Where the reference binds — the same vocabulary as
    /// [`FieldNode::binding_site`], plus `filter` and `drillthrough`.
    pub binding_site: String,
}

/// Box-drawing cells for the tree's connector lines — each four columns wide.
const TEE: &str = "├── ";
const ELBOW: &str = "└── ";
const STALK: &str = "│   ";
const BLANK: &str = "    ";

/// Filter and unresolved rows share every property block, and `unresolved`
/// is the longer label — so it sets the column width for their blocks.
const FILTER_ROW_WIDTH: usize = "unresolved".len();

/// Prints the human tree: report → pages → visuals → property rows.
pub(crate) fn human(
    out: &mut dyn io::Write,
    palette: &Palette,
    inventory: &Inventory,
) -> io::Result<()> {
    for report in &inventory.reports {
        let visuals: usize = report.pages.iter().map(|page| page.visuals.len()).sum();
        let desktop = report.pages.iter().filter(|page| !page.mobile).count();
        let mut header = format!(
            "{} — {}, {}",
            report.name,
            plural(desktop, "page"),
            plural(visuals, "visual")
        );
        if report.unresolved_count > 0 {
            header.push_str(&format!(
                ", {}",
                plural(report.unresolved_count, "unresolved binding")
            ));
        }
        writeln!(out, "{}", palette.bold(&header))?;
        if !report.filters.is_empty() || !report.unresolved.is_empty() {
            writeln!(out, "  Report filters")?;
            for filter in &report.filters {
                write_filter(out, palette, filter, "    ", FILTER_ROW_WIDTH)?;
            }
            for unresolved in &report.unresolved {
                write_unresolved(out, palette, unresolved, "    ", FILTER_ROW_WIDTH)?;
            }
        }
        let (desktop_pages, mobile_pages): (Vec<_>, Vec<_>) =
            report.pages.iter().partition(|page| !page.mobile);
        if !desktop_pages.is_empty() {
            writeln!(out, "  Pages")?;
            write_pages(out, palette, &desktop_pages)?;
        }
        if !mobile_pages.is_empty() {
            writeln!(out, "  Mobile pages")?;
            write_pages(out, palette, &mobile_pages)?;
        }
        if report.bookmarks > 0 {
            writeln!(
                out,
                "  {}",
                palette.dim(&plural(report.bookmarks, "bookmark"))
            )?;
        }
        if !report.report_measures.is_empty() {
            writeln!(
                out,
                "  Report measures: {}",
                report.report_measures.join(", ")
            )?;
        }
    }
    Ok(())
}

/// One section's pages as connected subtrees.
fn write_pages(out: &mut dyn io::Write, palette: &Palette, pages: &[&PageNode]) -> io::Result<()> {
    for (index, page) in pages.iter().enumerate() {
        write_page(out, palette, page, index + 1 == pages.len())?;
    }
    Ok(())
}

/// One page's subtree: the connected page line, then its children in order —
/// a `filters (n)` group for its filters, an `unresolved (n)` group for its
/// broken bindings, then its visuals — each connected into the child chain.
fn write_page(
    out: &mut dyn io::Write,
    palette: &Palette,
    page: &PageNode,
    last: bool,
) -> io::Result<()> {
    let prefix = "    ";
    let connector = if last { ELBOW } else { TEE };
    let stalk = format!("{prefix}{}", if last { BLANK } else { STALK });
    let label = page.display_name.as_deref().unwrap_or(&page.name);
    let mut line = format!("{prefix}{connector}{label}");
    if label != page.name {
        line.push_str(&format!(" {}", palette.dim(&format!("({})", page.name))));
    }
    if page.hidden {
        line.push_str(&format!(" {}", palette.dim("(hidden)")));
    }
    line.push_str(&format!(" — {}", plural(page.visuals.len(), "visual")));
    writeln!(out, "{line}")?;
    let mut remaining = page.visuals.len()
        + usize::from(!page.filters.is_empty())
        + usize::from(!page.unresolved.is_empty());
    if !page.filters.is_empty() {
        remaining -= 1;
        let inner = write_group(out, "filters", page.filters.len(), &stalk, remaining == 0)?;
        let row_prefix = format!("{inner}  ");
        for filter in &page.filters {
            write_filter(out, palette, filter, &row_prefix, "filter".len())?;
        }
    }
    if !page.unresolved.is_empty() {
        remaining -= 1;
        let inner = write_group(
            out,
            "unresolved",
            page.unresolved.len(),
            &stalk,
            remaining == 0,
        )?;
        let row_prefix = format!("{inner}  ");
        for unresolved in &page.unresolved {
            write_unresolved(out, palette, unresolved, &row_prefix, FILTER_ROW_WIDTH)?;
        }
    }
    for visual in &page.visuals {
        remaining -= 1;
        write_visual(out, palette, visual, &stalk, remaining == 0)?;
    }
    Ok(())
}

/// One named group node — `filters (9)`, `unresolved (2)` — collecting a
/// page's property rows into the child chain: the node connects like a
/// visual, and its rows print inside its subtree. Returns the group's stalk
/// for its rows.
fn write_group(
    out: &mut dyn io::Write,
    label: &str,
    count: usize,
    prefix: &str,
    last: bool,
) -> io::Result<String> {
    let connector = if last { ELBOW } else { TEE };
    writeln!(out, "{prefix}{connector}{label} ({count})")?;
    Ok(format!("{prefix}{}", if last { BLANK } else { STALK }))
}

/// One visual's subtree: the connected visual line — dim object name, bold
/// type — then its property rows aligned on a label column computed from the
/// visual's own rows.
fn write_visual(
    out: &mut dyn io::Write,
    palette: &Palette,
    visual: &VisualNode,
    prefix: &str,
    last: bool,
) -> io::Result<()> {
    let connector = if last { ELBOW } else { TEE };
    let stalk = format!("{prefix}{}", if last { BLANK } else { STALK });
    writeln!(
        out,
        "{prefix}{connector}{} {} {}",
        palette.dim(&visual.name),
        palette.dim("—"),
        palette.bold(&visual.visual_type)
    )?;
    let row_prefix = format!("{stalk}  ");
    let mut width = visual
        .fields
        .iter()
        .map(|field| human_site(&field.binding_site).chars().count())
        .max()
        .unwrap_or(0);
    if !visual.filters.is_empty() || !visual.unresolved.is_empty() {
        width = width.max(FILTER_ROW_WIDTH);
    }
    for field in &visual.fields {
        let mut content = String::new();
        if let Some(kind) = tree_kind(field.kind) {
            content.push_str(kind);
            content.push(' ');
        }
        content.push_str(&field.target);
        if field.active == Some(false) {
            content.push_str(&format!(" {}", palette.dim("(inactive)")));
        }
        let label = human_site(&field.binding_site);
        writeln!(out, "{row_prefix}{label:<width$} {content}")?;
    }
    for filter in &visual.filters {
        write_filter(out, palette, filter, &row_prefix, width)?;
    }
    for unresolved in &visual.unresolved {
        write_unresolved(out, palette, unresolved, &row_prefix, width)?;
    }
    Ok(())
}

/// The kind word the tree keeps: `column` and `measure` only, the two a bare
/// `'Table'[Name]` cannot tell apart. Hierarchy levels, aggregations, and
/// written expressions self-identify in their target.
fn tree_kind(kind: &'static str) -> Option<&'static str> {
    match kind {
        "column" | "measure" => Some(kind),
        _ => None,
    }
}

/// One unresolved property row: the red marker in the label column, then the
/// target, the reason in parentheses, and `via <artifact>` when the binding
/// lands on an artifact whose own DAX is broken.
fn write_unresolved(
    out: &mut dyn io::Write,
    palette: &Palette,
    unresolved: &UnresolvedNode,
    prefix: &str,
    width: usize,
) -> io::Result<()> {
    let mut line = format!(
        "{prefix}{}",
        palette.red(&format!("{:<width$}", "unresolved"))
    );
    line.push(' ');
    line.push_str(&unresolved.target);
    line.push(' ');
    line.push_str(&palette.red(&format!("({})", reason_phrase(unresolved))));
    if let Some(artifact) = &unresolved.artifact {
        line.push_str(&format!(" {}", palette.dim(&format!("via {artifact}"))));
    }
    writeln!(out, "{line}")
}

/// One filter property row: `filter` in the label column, then the name, the
/// persisted metadata, and the declared target — one `also references` line
/// per additional field the condition tree touches, aligned under the
/// target.
fn write_filter(
    out: &mut dyn io::Write,
    palette: &Palette,
    filter: &FilterNode,
    prefix: &str,
    width: usize,
) -> io::Result<()> {
    let mut content = String::new();
    if let Some(name) = &filter.name {
        content.push_str(name);
    }
    let mut metadata = Vec::new();
    if let Some(filter_type) = &filter.filter_type {
        metadata.push(filter_type.clone());
    }
    if let Some(display_name) = &filter.display_name {
        metadata.push(display_name.clone());
    }
    if !metadata.is_empty() {
        if !content.is_empty() {
            content.push(' ');
        }
        content.push_str(&format!("({})", metadata.join(", ")));
    }
    if let Some(target) = &filter.target {
        if !content.is_empty() {
            content.push_str(" — ");
        }
        content.push_str(target);
    }
    writeln!(out, "{prefix}{:<width$} {content}", "filter")?;
    for reference in &filter.references {
        writeln!(
            out,
            "{prefix}{} {} {reference}",
            " ".repeat(width),
            palette.dim("also references")
        )?;
    }
    Ok(())
}

/// The binding site as a person reads it: the well's bare role, the rest
/// with spaces — except `conditional formatting`, which the tree shortens to
/// `formatting`, the one label long enough to stretch a formatting-heavy
/// visual's whole column.
fn human_site(site: &str) -> String {
    match site.strip_prefix("field_well:") {
        Some(role) => role.to_string(),
        None if site == "conditional_formatting" => "formatting".to_string(),
        None => site.replace('_', " "),
    }
}

/// The unresolved reason as a person reads it.
fn reason_phrase(unresolved: &UnresolvedNode) -> String {
    unresolved.reason.replace('_', " ")
}

/// `n` followed by the singular or plural noun.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// The list views: aligned tables, one row per page / visual / binding / used
/// model object. Hand-rolled through [`write_table`] — computed column
/// widths, header, dash separator, no color. An empty view prints nothing;
/// the stderr note explains why.
pub(crate) fn pages_view(out: &mut dyn io::Write, inventory: &Inventory) -> io::Result<()> {
    let mut rows = Vec::new();
    for report in &inventory.reports {
        for page in &report.pages {
            rows.push(vec![
                page.name.clone(),
                page.display_name.clone().unwrap_or_else(|| "-".to_string()),
                yes_or_dash(page.hidden),
                yes_or_dash(page.mobile),
                page.visuals.len().to_string(),
            ]);
        }
    }
    if rows.is_empty() {
        return Ok(());
    }
    write_table(
        out,
        &["page", "display", "hidden", "mobile", "visuals"],
        &rows,
    )
}

/// One roll-up row per visual: where it lives, its type, and how much it
/// references and fails to resolve.
pub(crate) fn visuals_view(out: &mut dyn io::Write, inventory: &Inventory) -> io::Result<()> {
    let mut rows = Vec::new();
    for report in &inventory.reports {
        for page in &report.pages {
            let page_label = page.display_name.as_deref().unwrap_or(&page.name);
            for visual in &page.visuals {
                rows.push(vec![
                    page_label.to_string(),
                    visual.name.clone(),
                    visual.visual_type.clone(),
                    visual.fields.len().to_string(),
                    visual.unresolved.len().to_string(),
                ]);
            }
        }
    }
    if rows.is_empty() {
        return Ok(());
    }
    write_table(
        out,
        &["page", "visual", "type", "fields", "unresolved"],
        &rows,
    )
}

/// One row per binding: field wells, sorts, conditional formatting, alt text,
/// filter targets and their condition-tree references, and the unresolved
/// bindings — at report, page, and visual level. Report- and page-level rows
/// use `-` for the segments above them.
pub(crate) fn fields_view(out: &mut dyn io::Write, inventory: &Inventory) -> io::Result<()> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for report in &inventory.reports {
        extend_filter_rows(&mut rows, "-", "-", &report.filters);
        extend_unresolved_rows(&mut rows, "-", "-", &report.unresolved);
        for page in &report.pages {
            let page_label = page.display_name.as_deref().unwrap_or(&page.name);
            extend_filter_rows(&mut rows, page_label, "-", &page.filters);
            extend_unresolved_rows(&mut rows, page_label, "-", &page.unresolved);
            for visual in &page.visuals {
                for field in &visual.fields {
                    let note = match field.active {
                        Some(false) => "(inactive)".to_string(),
                        _ => String::new(),
                    };
                    rows.push(vec![
                        page_label.to_string(),
                        visual.name.clone(),
                        field.binding_site.clone(),
                        field.kind.to_string(),
                        field.target.clone(),
                        note,
                    ]);
                }
                extend_filter_rows(&mut rows, page_label, &visual.name, &visual.filters);
                extend_unresolved_rows(&mut rows, page_label, &visual.name, &visual.unresolved);
            }
        }
    }
    if rows.is_empty() {
        return Ok(());
    }
    write_table(
        out,
        &["page", "visual", "site", "kind", "target", "note"],
        &rows,
    )
}

/// One row per model object the report keeps alive.
pub(crate) fn used_view(out: &mut dyn io::Write, inventory: &Inventory) -> io::Result<()> {
    let rows: Vec<Vec<String>> = inventory
        .used
        .iter()
        .map(|used| vec![used.kind.to_string(), used.id.clone()])
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    write_table(out, &["kind", "id"], &rows)
}

/// One row per filter: the declared target, then one row per condition-tree
/// reference, both labeled with the filter's name and type.
fn extend_filter_rows(
    rows: &mut Vec<Vec<String>>,
    page: &str,
    visual: &str,
    filters: &[FilterNode],
) {
    for filter in filters {
        let label = filter_label(filter);
        if let Some(target) = &filter.target {
            rows.push(vec![
                page.to_string(),
                visual.to_string(),
                "filter".to_string(),
                "filter".to_string(),
                target.clone(),
                label.clone(),
            ]);
        }
        for reference in &filter.references {
            rows.push(vec![
                page.to_string(),
                visual.to_string(),
                "filter".to_string(),
                "filter_ref".to_string(),
                reference.clone(),
                label.clone(),
            ]);
        }
    }
}

/// One row per unresolved binding.
fn extend_unresolved_rows(
    rows: &mut Vec<Vec<String>>,
    page: &str,
    visual: &str,
    unresolved: &[UnresolvedNode],
) {
    for node in unresolved {
        let mut note = reason_phrase(node);
        if let Some(artifact) = &node.artifact {
            note.push_str(&format!(" via {artifact}"));
        }
        rows.push(vec![
            page.to_string(),
            visual.to_string(),
            node.binding_site.clone(),
            "unresolved".to_string(),
            node.target.clone(),
            note,
        ]);
    }
}

/// The filter's name and kind as written, e.g. `V1Filter (Advanced)`.
fn filter_label(filter: &FilterNode) -> String {
    let mut parts = Vec::new();
    if let Some(name) = &filter.name {
        parts.push(name.clone());
    }
    if let Some(filter_type) = &filter.filter_type {
        parts.push(format!("({filter_type})"));
    }
    parts.join(" ")
}

fn yes_or_dash(flag: bool) -> String {
    if flag {
        "yes".to_string()
    } else {
        "-".to_string()
    }
}

/// One aligned table: computed column widths, header, dash separator, rows.
/// Cells never wrap — the terminal does; widths are char counts.
fn write_table(out: &mut dyn io::Write, header: &[&str], rows: &[Vec<String>]) -> io::Result<()> {
    let widths: Vec<usize> = (0..header.len())
        .map(|column| {
            header[column].chars().count().max(
                rows.iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| cell.chars().count())
                    .max()
                    .unwrap_or(0),
            )
        })
        .collect();
    let line = |cells: &[String]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(column, cell)| format!("{cell:<width$}", width = widths[column]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let head: Vec<String> = header.iter().map(|cell| cell.to_string()).collect();
    writeln!(out, "{}", line(&head))?;
    let dashes: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
    writeln!(out, "{}", dashes.join("  "))?;
    for row in rows {
        writeln!(out, "{}", line(row))?;
    }
    Ok(())
}

/// Prints one tab-separated record per line, each starting with its record
/// type: `report`, `report-measure`, `page`, `visual`, `field`, `filter`,
/// `filter-ref`, `unresolved`. Missing segments are `-`.
pub(crate) fn plain(out: &mut dyn io::Write, inventory: &Inventory) -> io::Result<()> {
    for report in &inventory.reports {
        writeln!(out, "report\t{}", report.name)?;
        for measure in &report.report_measures {
            writeln!(out, "report-measure\t{}\t{}", report.name, measure)?;
        }
        for filter in &report.filters {
            write_plain_filter(out, &report.name, "-", "-", filter)?;
        }
        for unresolved in &report.unresolved {
            writeln!(
                out,
                "unresolved\t{}\t-\t-\t{}\t{}",
                report.name, unresolved.target, unresolved.reason
            )?;
        }
        for page in &report.pages {
            writeln!(
                out,
                "page\t{}\t{}\t{}",
                report.name,
                page.name,
                if page.mobile { "mobile" } else { "desktop" }
            )?;
            for filter in &page.filters {
                write_plain_filter(out, &report.name, &page.name, "-", filter)?;
            }
            for unresolved in &page.unresolved {
                writeln!(
                    out,
                    "unresolved\t{}\t{}\t-\t{}\t{}",
                    report.name, page.name, unresolved.target, unresolved.reason
                )?;
            }
            for visual in &page.visuals {
                writeln!(
                    out,
                    "visual\t{}\t{}\t{}\t{}",
                    report.name, page.name, visual.name, visual.visual_type
                )?;
                for field in &visual.fields {
                    writeln!(
                        out,
                        "field\t{}\t{}\t{}\t{}\t{}\t{}",
                        report.name,
                        page.name,
                        visual.name,
                        field.binding_site,
                        field.kind,
                        field.target
                    )?;
                }
                for filter in &visual.filters {
                    write_plain_filter(out, &report.name, &page.name, &visual.name, filter)?;
                }
                for unresolved in &visual.unresolved {
                    writeln!(
                        out,
                        "unresolved\t{}\t{}\t{}\t{}\t{}",
                        report.name, page.name, visual.name, unresolved.target, unresolved.reason
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// `filter <report> <page> <visual> <name> <type> <target>` — `-` for the
/// segments the filter does not carry — and one `filter-ref` record per
/// condition-tree reference.
fn write_plain_filter(
    out: &mut dyn io::Write,
    report: &str,
    page: &str,
    visual: &str,
    filter: &FilterNode,
) -> io::Result<()> {
    writeln!(
        out,
        "filter\t{}\t{}\t{}\t{}\t{}\t{}",
        report,
        page,
        visual,
        filter.name.as_deref().unwrap_or("-"),
        filter.filter_type.as_deref().unwrap_or("-"),
        filter.target.as_deref().unwrap_or("-")
    )?;
    for reference in &filter.references {
        writeln!(
            out,
            "filter-ref\t{}\t{}\t{}\t{}",
            report, page, visual, reference
        )?;
    }
    Ok(())
}

/// Prints the JSON inventory (schema_version 1 — `docs/report.md`).
pub(crate) fn json(out: &mut dyn io::Write, inventory: &Inventory) -> io::Result<()> {
    let payload = JsonInventory {
        schema_version: 1,
        target: inventory.target.clone(),
        reports: inventory
            .reports
            .iter()
            .map(|report| JsonReport {
                path: report.path.clone(),
                name: report.name.clone(),
                unresolved: report.unresolved.iter().map(json_unresolved).collect(),
                pages: report
                    .pages
                    .iter()
                    .map(|page| JsonPage {
                        name: page.name.clone(),
                        display_name: page.display_name.clone(),
                        hidden: page.hidden,
                        mobile: page.mobile,
                        filters: page.filters.iter().map(json_filter).collect(),
                        unresolved: page.unresolved.iter().map(json_unresolved).collect(),
                        visuals: report_visuals(page),
                    })
                    .collect(),
                bookmarks: report.bookmarks,
                report_measures: report.report_measures.clone(),
                unresolved_count: report.unresolved_count,
            })
            .collect(),
        skips: JsonSkips {
            count: inventory.skips.len(),
            notices: inventory
                .skips
                .iter()
                .map(|skip| JsonSkip {
                    path: skip.path.clone(),
                    location: skip.location.clone(),
                    kind: skip.kind,
                    detail: skip.detail.clone(),
                })
                .collect(),
        },
    };
    serde_json::to_writer_pretty(&mut *out, &payload)?;
    writeln!(out)
}

/// A page's visuals as JSON DTOs.
fn report_visuals(page: &PageNode) -> Vec<JsonVisual> {
    page.visuals
        .iter()
        .map(|visual| JsonVisual {
            name: visual.name.clone(),
            visual_type: visual.visual_type.clone(),
            fields: visual
                .fields
                .iter()
                .map(|field| JsonField {
                    kind: field.kind,
                    target: field.target.clone(),
                    binding_site: field.binding_site.clone(),
                    active: field.active,
                })
                .collect(),
            filters: visual.filters.iter().map(json_filter).collect(),
            unresolved: visual.unresolved.iter().map(json_unresolved).collect(),
        })
        .collect()
}

/// The filter DTO report/page/visual filters share.
fn json_filter(filter: &FilterNode) -> JsonFilter {
    JsonFilter {
        name: filter.name.clone(),
        display_name: filter.display_name.clone(),
        filter_type: filter.filter_type.clone(),
        target: filter.target.clone(),
        references: filter.references.clone(),
    }
}

/// The unresolved DTO every level shares.
fn json_unresolved(unresolved: &UnresolvedNode) -> JsonUnresolved {
    JsonUnresolved {
        target: unresolved.target.clone(),
        reason: unresolved.reason,
        bound_artifact: unresolved.artifact.clone(),
        binding_site: unresolved.binding_site.clone(),
    }
}

#[derive(Serialize)]
struct JsonInventory {
    schema_version: u8,
    target: String,
    reports: Vec<JsonReport>,
    skips: JsonSkips,
}

#[derive(Serialize)]
struct JsonReport {
    path: String,
    name: String,
    /// Broken live bindings at report level.
    unresolved: Vec<JsonUnresolved>,
    pages: Vec<JsonPage>,
    /// v1 lists the count only — saved bookmark state is not itemized.
    bookmarks: usize,
    report_measures: Vec<String>,
    /// The itemized unresolved bindings: every visual-, page-, and
    /// report-level row this report carries.
    unresolved_count: usize,
}

#[derive(Serialize)]
struct JsonPage {
    name: String,
    display_name: Option<String>,
    hidden: bool,
    mobile: bool,
    filters: Vec<JsonFilter>,
    /// Broken live bindings at page level.
    unresolved: Vec<JsonUnresolved>,
    visuals: Vec<JsonVisual>,
}

#[derive(Serialize)]
struct JsonVisual {
    name: String,
    #[serde(rename = "type")]
    visual_type: String,
    fields: Vec<JsonField>,
    filters: Vec<JsonFilter>,
    unresolved: Vec<JsonUnresolved>,
}

#[derive(Serialize)]
struct JsonField {
    /// `column`, `measure`, `hierarchy_level`, `aggregation`, or `written`.
    kind: &'static str,
    target: String,
    /// `field_well:<role>`, `sort`, `conditional_formatting`, or `alt_text`.
    binding_site: String,
    /// Present on field-well projections only; inactive projections still
    /// bind.
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<bool>,
}

#[derive(Serialize)]
struct JsonFilter {
    name: Option<String>,
    display_name: Option<String>,
    /// As written, e.g. `Categorical` or `Advanced`.
    filter_type: Option<String>,
    target: Option<String>,
    references: Vec<String>,
}

#[derive(Serialize)]
struct JsonUnresolved {
    target: String,
    /// The same vocabulary as `scan`'s broken bindings.
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bound_artifact: Option<String>,
    binding_site: String,
}

#[derive(Serialize)]
struct JsonSkips {
    count: usize,
    notices: Vec<JsonSkip>,
}

#[derive(Serialize)]
struct JsonSkip {
    path: String,
    location: Option<String>,
    kind: &'static str,
    detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Inventory {
        Inventory {
            target: "models/Sales.SemanticModel".to_string(),
            reports: vec![ReportNode {
                path: "reports/Sales.Report".to_string(),
                name: "Sales".to_string(),
                filters: Vec::new(),
                unresolved: Vec::new(),
                pages: vec![PageNode {
                    name: "P1".to_string(),
                    display_name: Some("Overview".to_string()),
                    hidden: false,
                    mobile: false,
                    filters: Vec::new(),
                    unresolved: Vec::new(),
                    visuals: vec![VisualNode {
                        name: "V1".to_string(),
                        visual_type: "card".to_string(),
                        fields: vec![FieldNode {
                            kind: "measure",
                            target: "'Sales'[Total]".to_string(),
                            binding_site: "field_well:Values".to_string(),
                            active: Some(true),
                        }],
                        filters: vec![FilterNode {
                            name: Some("Filter5".to_string()),
                            display_name: None,
                            filter_type: Some("Categorical".to_string()),
                            target: Some("'Product'[Color]".to_string()),
                            references: Vec::new(),
                        }],
                        unresolved: vec![UnresolvedNode {
                            target: "'Sales'[Gone]".to_string(),
                            reason: "field_not_found",
                            artifact: None,
                            binding_site: "field_well:Values".to_string(),
                        }],
                    }],
                }],
                bookmarks: 1,
                report_measures: vec!["Budget %".to_string()],
                unresolved_count: 1,
            }],
            skips: Vec::new(),
            used: vec![
                UsedRow {
                    kind: "column",
                    id: "'Sales'[Amount]".to_string(),
                },
                UsedRow {
                    kind: "measure",
                    id: "'Sales'[Total]".to_string(),
                },
            ],
        }
    }

    /// The human tree shows every level: report, page, visual, and the
    /// property rows — field, filter, and the unresolved marker — aligned on
    /// a label column.
    #[test]
    fn human_prints_the_full_tree() {
        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &sample()).expect("human renders");

        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("Sales — 1 page, 1 visual, 1 unresolved binding"));
        assert!(text.contains("  Pages"));
        assert!(text.contains("    └── Overview (P1) — 1 visual"));
        assert!(text.contains("        └── V1 — card"));
        assert!(text.contains("              Values     measure 'Sales'[Total]"));
        assert!(
            text.contains("              filter     Filter5 (Categorical) — 'Product'[Color]"),
            "{text}"
        );
        assert!(text.contains("              unresolved 'Sales'[Gone] (field not found)"));
        assert!(text.contains("Report measures: Budget %"));
    }

    /// Report- and page-level unresolved bindings render under their level,
    /// with the same property-row shape the visual level uses.
    #[test]
    fn human_shows_unresolved_at_every_level() {
        let mut inventory = sample();
        let report = &mut inventory.reports[0];
        report.unresolved.push(UnresolvedNode {
            target: "'Product'[Category]".to_string(),
            reason: "table_not_found",
            artifact: None,
            binding_site: "filter".to_string(),
        });
        report.pages[0].unresolved.push(UnresolvedNode {
            target: "'Date'[Calendar Year]".to_string(),
            reason: "table_not_found",
            artifact: None,
            binding_site: "filter".to_string(),
        });

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &inventory).expect("human renders");

        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            text.contains("    unresolved 'Product'[Category] (table not found)"),
            "report level, under Report filters:\n{text}"
        );
        assert!(
            text.contains("        ├── unresolved (1)"),
            "page level, a connected group node:\n{text}"
        );
        assert!(
            text.contains("        │     unresolved 'Date'[Calendar Year] (table not found)"),
            "page level, inside the group:\n{text}"
        );
    }

    /// A page's filters collect under a `filters (n)` group node that
    /// connects into the page's child chain ahead of its visuals — the
    /// guides stay unbroken from the page line down to the last visual.
    #[test]
    fn page_filters_group_under_a_connected_node() {
        let mut inventory = sample();
        inventory.reports[0].pages[0].filters.push(FilterNode {
            name: Some("Filter1".to_string()),
            display_name: None,
            filter_type: Some("Categorical".to_string()),
            target: Some("'Product'[Color]".to_string()),
            references: vec!["'Product'[Model]".to_string()],
        });

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &inventory).expect("human renders");

        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("        ├── filters (1)"), "{text}");
        assert!(
            text.contains("│     filter Filter1 (Categorical) — 'Product'[Color]"),
            "{text}"
        );
        assert!(
            text.contains("│            also references 'Product'[Model]"),
            "{text}"
        );
        assert!(
            text.contains("        └── V1 — card"),
            "the visual stays connected after the group:\n{text}"
        );
    }

    /// Pages and visuals connect with box drawing: `├──` for every child but
    /// the last, `└──` for the last, and the stalk carries down through open
    /// subtrees.
    #[test]
    fn the_tree_connects_children_with_box_drawing() {
        let mut inventory = sample();
        let report = &mut inventory.reports[0];
        report.pages[0].visuals.push(VisualNode {
            name: "V2".to_string(),
            visual_type: "card".to_string(),
            ..VisualNode::default()
        });
        report.pages.push(PageNode {
            name: "P2".to_string(),
            display_name: None,
            hidden: false,
            mobile: false,
            filters: Vec::new(),
            unresolved: Vec::new(),
            visuals: vec![VisualNode {
                name: "V3".to_string(),
                visual_type: "card".to_string(),
                ..VisualNode::default()
            }],
        });

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &inventory).expect("human renders");

        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("    ├── Overview (P1) — 2 visuals"), "{text}");
        assert!(text.contains("    └── P2 — 1 visual"), "{text}");
        assert!(
            text.contains("    │   ├── V1 — card"),
            "V1 hangs off P1's open stalk:\n{text}"
        );
        assert!(text.contains("    │   └── V2 — card"), "{text}");
        assert!(text.contains("        └── V3 — card"), "{text}");
        assert!(
            text.contains("    │   │     Values     measure 'Sales'[Total]"),
            "the open pages' stalk carries through V1's rows:\n{text}"
        );
    }

    /// The tree keeps `column` and `measure` — the two a bare
    /// `'Table'[Name]` cannot tell apart — and drops the kinds whose targets
    /// self-identify.
    #[test]
    fn the_tree_keeps_kind_words_only_where_ambiguous() {
        let mut inventory = sample();
        inventory.reports[0].pages[0].visuals[0].fields = vec![
            FieldNode {
                kind: "column",
                target: "'Product'[Color]".to_string(),
                binding_site: "field_well:Values".to_string(),
                active: Some(true),
            },
            FieldNode {
                kind: "measure",
                target: "'Sales'[Total]".to_string(),
                binding_site: "field_well:Values".to_string(),
                active: Some(true),
            },
            FieldNode {
                kind: "hierarchy_level",
                target: "hierarchy 'Product'[Products] level 'Category'".to_string(),
                binding_site: "field_well:Rows".to_string(),
                active: Some(true),
            },
            FieldNode {
                kind: "aggregation",
                target: "Max('Sales'[Products])".to_string(),
                binding_site: "conditional_formatting".to_string(),
                active: Some(true),
            },
        ];

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &inventory).expect("human renders");

        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("column 'Product'[Color]"), "{text}");
        assert!(text.contains("measure 'Sales'[Total]"), "{text}");
        assert!(
            text.contains("Rows       hierarchy 'Product'[Products] level 'Category'"),
            "{text}"
        );
        assert!(text.contains("Max('Sales'[Products])"), "{text}");
        assert!(
            !text.contains("hierarchy level "),
            "the target carries the hierarchy shape; no kind word:\n{text}"
        );
        assert!(!text.contains("aggregation "), "{text}");
    }

    /// `conditional formatting` shortens to `formatting` in the tree, and the
    /// label column pads to the block's longest label.
    #[test]
    fn the_tree_abbreviates_conditional_formatting() {
        let mut inventory = sample();
        let visual = &mut inventory.reports[0].pages[0].visuals[0];
        visual.fields = vec![
            FieldNode {
                kind: "measure",
                target: "'Sales'[Total]".to_string(),
                binding_site: "field_well:Values".to_string(),
                active: Some(true),
            },
            FieldNode {
                kind: "column",
                target: "'Product'[Color]".to_string(),
                binding_site: "conditional_formatting".to_string(),
                active: Some(true),
            },
        ];
        visual.filters.clear();
        visual.unresolved.clear();

        let mut out = Vec::new();
        human(&mut out, &Palette::plain(), &inventory).expect("human renders");

        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            text.contains("formatting column 'Product'[Color]"),
            "{text}"
        );
        assert!(!text.contains("conditional formatting"), "{text}");
        assert!(
            text.contains("Values     measure 'Sales'[Total]"),
            "`Values` pads to the block's width, `formatting`:\n{text}"
        );
    }

    /// `--plain` records are tab-separated and self-contained: each carries
    /// its report, page, and visual; `-` stands in for missing segments.
    #[test]
    fn plain_records_are_tab_separated() {
        let mut out = Vec::new();
        plain(&mut out, &sample()).expect("plain renders");

        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("report\tSales\n"));
        assert!(text.contains("report-measure\tSales\tBudget %\n"));
        assert!(text.contains("page\tSales\tP1\tdesktop\n"));
        assert!(text.contains("visual\tSales\tP1\tV1\tcard\n"));
        assert!(
            text.contains("field\tSales\tP1\tV1\tfield_well:Values\tmeasure\t'Sales'[Total]\n")
        );
        assert!(text.contains("filter\tSales\tP1\tV1\tFilter5\tCategorical\t'Product'[Color]\n"));
        assert!(text.contains("unresolved\tSales\tP1\tV1\t'Sales'[Gone]\tfield_not_found\n"));
    }

    /// `--json` pins schema_version 1 and the field/vocabulary shapes.
    #[test]
    fn json_pins_the_v1_schema() {
        let mut out = Vec::new();
        json(&mut out, &sample()).expect("json renders");

        let payload: serde_json::Value = serde_json::from_slice(&out).expect("valid json");
        assert_eq!(payload["schema_version"], 1);
        let report = &payload["reports"][0];
        assert_eq!(report["name"], "Sales");
        assert_eq!(report["unresolved_count"], 1);
        let visual = &report["pages"][0]["visuals"][0];
        assert_eq!(visual["type"], "card");
        assert_eq!(visual["fields"][0]["binding_site"], "field_well:Values");
        assert_eq!(visual["fields"][0]["active"], true);
        assert_eq!(visual["unresolved"][0]["reason"], "field_not_found");
    }

    /// The table writer computes one width per column from the widest cell;
    /// the last column carries no trailing padding.
    #[test]
    fn write_table_computes_column_widths() {
        let mut out = Vec::new();
        write_table(
            &mut out,
            &["page", "visual"],
            &[
                vec!["Overview".to_string(), "V1".to_string()],
                vec!["P2".to_string(), "pivotTable".to_string()],
            ],
        )
        .expect("table renders");

        let text = String::from_utf8(out).expect("utf-8");
        assert_eq!(
            text,
            "page      visual\n--------  ----------\nOverview  V1\nP2        pivotTable\n"
        );
    }

    /// The list views render their fixed columns; the pages view uses `-`
    /// for a page without a display name, the fields view labels filter and
    /// unresolved rows.
    #[test]
    fn the_views_render_their_tables() {
        let mut out = Vec::new();
        pages_view(&mut out, &sample()).expect("pages renders");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            text.contains(
                "page  display   hidden  mobile  visuals\n\
                           ----  --------  ------  ------  -------\n\
                           P1    Overview  -       -       1\n"
            ),
            "{text}"
        );

        let mut out = Vec::new();
        visuals_view(&mut out, &sample()).expect("visuals renders");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            text.contains("Overview  V1      card  1       1\n"),
            "{text}"
        );

        let mut out = Vec::new();
        fields_view(&mut out, &sample()).expect("fields renders");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            text.contains(
                "page      visual  site               kind        target            note\n"
            ),
            "{text}"
        );
        assert!(text.contains("measure     'Sales'[Total]"), "{text}");
        assert!(text.contains("Filter5 (Categorical)"), "{text}");
        assert!(text.contains("'Sales'[Gone]"), "{text}");
        assert!(text.contains("field not found"), "{text}");

        let mut out = Vec::new();
        used_view(&mut out, &sample()).expect("used renders");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("kind     id\n"), "{text}");
        assert!(text.contains("measure  'Sales'[Total]"), "{text}");
    }
}
