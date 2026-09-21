//! The `deps` presentation layer: the focused object view (Dependencies /
//! Impact trees), the graph overview, and — with the orchestration module —
//! the whole user-visible surface. Machine modes (`--plain`, `--json`) render
//! the same data; change them together with this module and `docs/deps.md`.

use std::io::{self};

use super::tree::Node;
use crate::style::Palette;

/// What `deps` prints: the whole-graph overview, or one object's view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DepsOutput {
    /// The compact overview printed with no object and no selectors — never
    /// the whole graph.
    Overview(OverviewOut),
    /// One object's dependencies and/or impact.
    Focused(FocusedOut),
}

/// The overview's counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OverviewOut {
    /// Every model and report object in the graph.
    pub objects: usize,
    /// Every dependency edge between them.
    pub edges: usize,
    /// Every report binding (the graph's roots).
    pub bindings: usize,
    /// `(label, count)` by object type, in the fixed bucket order.
    pub by_type: Vec<(&'static str, usize)>,
}

/// One object's focused view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FocusedOut {
    /// The object, in its display form.
    pub root: String,
    /// The object's kind label.
    pub kind: &'static str,
    /// The upstream section; `None` when only `--impact` was asked for.
    pub dependencies: Option<Section>,
    /// The downstream sections; `None` when only `--dependencies` was asked
    /// for.
    pub impact: Option<ImpactOut>,
}

/// One titled tree section: Dependencies, or the Model half of Impact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section {
    /// The slice's tree projection, rooted at the selected object; the
    /// renderer draws its children under the section header.
    pub tree: Node,
    /// True when the object has no neighbors in this direction — the section
    /// prints `└─ nothing`.
    pub empty: bool,
    /// True when the budget cut branches — the section prints the
    /// `nodes are reachable` footer.
    pub suppressed: bool,
    /// The slice's node count, for the footer.
    pub reachable: usize,
}

/// The Impact section's two halves: model objects, and the report bindings
/// that ride along on the impact slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImpactOut {
    /// The model-consumer tree; `None` when nothing in the model uses the
    /// object.
    pub model: Option<Section>,
    /// The report-binding tries, one root per report (or per object, when
    /// bindings land on more than the selected object).
    pub reports: Vec<Node>,
}

/// The footer a budget-truncated section prints — the one place that points
/// at the other output modes instead of drowning the terminal.
pub(crate) const TRUNCATION_FOOTER: &str = "Use --depth 2 for a smaller view, --graph \
 to inspect topology, or --json for the complete graph.";

/// Writes the human-readable output.
///
/// # Errors
/// Propagates stream write failures.
pub fn human(out: &mut dyn io::Write, palette: &Palette, output: &DepsOutput) -> io::Result<()> {
    match output {
        DepsOutput::Overview(overview) => write_overview(out, palette, overview),
        DepsOutput::Focused(focused) => write_focused(out, palette, focused),
    }
}

fn write_overview(
    out: &mut dyn io::Write,
    palette: &Palette,
    overview: &OverviewOut,
) -> io::Result<()> {
    writeln!(out, "{}", palette.bold("Dependency graph"))?;
    writeln!(out)?;
    writeln!(out, "{} model objects", overview.objects)?;
    writeln!(out, "{} dependency edges", overview.edges)?;
    writeln!(out, "{} report bindings", overview.bindings)?;
    writeln!(out)?;
    writeln!(out, "By type")?;
    for (label, count) in &overview.by_type {
        writeln!(out, "  {label:<14}{count}")?;
    }
    writeln!(out)?;
    writeln!(out, "Try:")?;
    writeln!(out, "  ripbi deps \"'Table'[Name]\"")?;
    Ok(())
}

fn write_focused(
    out: &mut dyn io::Write,
    palette: &Palette,
    focused: &FocusedOut,
) -> io::Result<()> {
    writeln!(out, "{}  {}", focused.root, palette.dim(focused.kind))?;
    if let Some(section) = &focused.dependencies {
        write_section(out, palette, "Dependencies", section)?;
    }
    if let Some(impact) = &focused.impact {
        writeln!(out)?;
        writeln!(out, "{}", palette.bold("Impact"))?;
        let model_empty = impact.model.as_ref().is_none_or(|model| model.empty);
        if model_empty && impact.reports.is_empty() {
            writeln!(out, "└─ nothing")?;
        } else {
            if let Some(model) = &impact.model {
                write_section(out, palette, "Model", model)?;
            }
            if !impact.reports.is_empty() {
                writeln!(out)?;
                writeln!(out, "{}", palette.bold("Reports"))?;
                write_children(out, &impact.reports, "")?;
            }
        }
    }
    Ok(())
}

fn write_section(
    out: &mut dyn io::Write,
    palette: &Palette,
    title: &str,
    section: &Section,
) -> io::Result<()> {
    writeln!(out)?;
    writeln!(out, "{}", palette.bold(title))?;
    if section.empty {
        writeln!(out, "└─ nothing")?;
        return Ok(());
    }
    write_children(out, &section.tree.children, "")?;
    if section.suppressed {
        writeln!(out)?;
        writeln!(out, "{} nodes are reachable.", section.reachable)?;
        writeln!(out, "{TRUNCATION_FOOTER}")?;
    }
    Ok(())
}

/// Draws one level of the tree: `├─`/`└─` connectors, `│` continuation
/// lines, and the `… N additional branches` line under every branch the
/// budget cut. Meaning never depends on colour — the markers are characters.
fn write_children(out: &mut dyn io::Write, children: &[Node], prefix: &str) -> io::Result<()> {
    for (index, node) in children.iter().enumerate() {
        let last = index + 1 == children.len();
        let connector = if last { "└─ " } else { "├─ " };
        let note = node
            .note
            .map(|note| format!("  {note}"))
            .unwrap_or_default();
        writeln!(out, "{prefix}{connector}{}{note}", node.label)?;
        let deeper = format!("{prefix}{}", if last { "   " } else { "│  " });
        write_children(out, &node.children, &deeper)?;
        if node.hidden > 0 {
            writeln!(out, "{deeper}└─ … {} additional branches", node.hidden)?;
        }
    }
    Ok(())
}
