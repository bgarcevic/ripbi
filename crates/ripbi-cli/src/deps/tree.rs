//! The tree projection of a [`DepSlice`]: the graph as a terminal reads it.
//!
//! The underlying structure is a graph; the human view is a tree. Three rules
//! keep the projection honest: a node on the current path prints `↺ cycle`
//! and stops there; a node already expanded anywhere earlier in the tree
//! prints `↩ already shown` instead of duplicating its whole subtree; and a
//! fixed budget stops expansion, saying so with `… N additional branches` at
//! every branch that was cut.

use std::collections::HashSet;

use ripbi_core::{DepSlice, ObjectId, Provenance};

/// The maximum number of child lines one human tree renders. `--plain` and
/// `--json` never truncate — this budget is the renderer's alone.
pub(crate) const HUMAN_BUDGET: usize = 200;

/// Which way a tree walks the graph: dependencies read the producers of each
/// node, impact reads its consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Orientation {
    /// The objects a node relies on (upstream).
    Dependencies,
    /// The objects that rely on a node (downstream).
    Impact,
}

impl Orientation {
    fn neighbors<'a>(
        &self,
        slice: &'a DepSlice,
        id: &ObjectId,
    ) -> Vec<(&'a ObjectId, &'a Provenance)> {
        match self {
            Self::Dependencies => slice.producers_of(id),
            Self::Impact => slice.consumers_of(id),
        }
    }
}

/// One rendered tree node: its label line, an optional note explaining why the
/// subtree is not below it, the children it was expanded to, and how many of
/// its children were cut by the budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    /// The node's own text — the object's display form and kind label, or the
    /// name of a report level.
    pub label: String,
    /// Why this node's children are not below it, when they are not.
    pub note: Option<&'static str>,
    /// The expanded children, in deterministic order.
    pub children: Vec<Node>,
    /// Children cut by [`HUMAN_BUDGET`] at this branch — rendered as one
    /// `… N additional branches` line.
    pub hidden: usize,
}

impl Node {
    /// A leaf node with a label and no note.
    pub(crate) fn leaf(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            note: None,
            children: Vec::new(),
            hidden: 0,
        }
    }
}

/// Builds the human tree projection of one slice.
pub(crate) struct TreeBuilder {
    orientation: Orientation,
    budget: usize,
    suppressed: bool,
}

impl TreeBuilder {
    /// A builder with a fresh budget, walking its slice in `orientation`.
    pub(crate) fn new(orientation: Orientation) -> Self {
        Self {
            orientation,
            budget: HUMAN_BUDGET,
            suppressed: false,
        }
    }

    /// Whether any branch was cut by the budget.
    pub(crate) fn suppressed(&self) -> bool {
        self.suppressed
    }

    /// Projects the subtree at `root` — every neighbor chain the budget
    /// allows, with cycles and repeats marked instead of expanded.
    pub(crate) fn build(
        &mut self,
        slice: &DepSlice,
        root: &ObjectId,
        label: impl Fn(&ObjectId) -> String,
    ) -> Node {
        let mut expanded = HashSet::new();
        let mut path = HashSet::new();
        let orientation = self.orientation;
        let budget = &mut self.budget;
        let suppressed = &mut self.suppressed;
        expand(
            slice,
            orientation,
            root,
            &label,
            &mut expanded,
            &mut path,
            budget,
            suppressed,
        )
    }
}

/// One recursive expansion. `expanded` holds every node whose children have
/// been rendered anywhere in the tree; `path` holds the ancestors of `id`,
/// which is what makes a back edge a cycle rather than a repeat.
#[allow(clippy::too_many_arguments)]
fn expand(
    slice: &DepSlice,
    orientation: Orientation,
    id: &ObjectId,
    label: &impl Fn(&ObjectId) -> String,
    expanded: &mut HashSet<ObjectId>,
    path: &mut HashSet<ObjectId>,
    budget: &mut usize,
    suppressed: &mut bool,
) -> Node {
    expanded.insert(id.clone());
    path.insert(id.clone());
    let mut children = Vec::new();
    let mut hidden = 0;
    for (neighbor, _provenance) in orientation.neighbors(slice, id) {
        if *budget == 0 {
            hidden += 1;
            continue;
        }
        *budget -= 1;
        let node = if path.contains(neighbor) {
            Node {
                label: label(neighbor),
                note: Some("↺ cycle"),
                children: Vec::new(),
                hidden: 0,
            }
        } else if expanded.contains(neighbor) {
            Node {
                label: label(neighbor),
                note: Some("↩ already shown"),
                children: Vec::new(),
                hidden: 0,
            }
        } else {
            expand(
                slice,
                orientation,
                neighbor,
                label,
                expanded,
                path,
                budget,
                suppressed,
            )
        };
        children.push(node);
    }
    path.remove(id);
    if hidden > 0 {
        *suppressed = true;
    }
    Node {
        label: label(id),
        note: None,
        children,
        hidden,
    }
}
