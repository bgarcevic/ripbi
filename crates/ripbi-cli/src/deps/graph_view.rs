//! The `--graph` view: one slice drawn as a layered topology diagram instead
//! of a tree. Deterministic by construction — layers are BFS distances from
//! the root, rows within a layer are barycenter-ordered then sorted by object
//! identity, and every draw writes to a fixed canvas. This is presentation
//! only: the same slice, depth, and filters feed the tree, `--plain`, and
//! `--json`.
//!
//! The diagram is for focused slices. Beyond [`COMPACT_NODES`] nodes it
//! degrades to a numbered legend and a pointer to the other modes instead of
//! producing unreadable output — the degradation the `deps` contract
//! promises.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;

use ripbi_core::{DepSlice, ObjectId};

/// Beyond this many nodes, the diagram degrades to a numbered legend.
pub(crate) const COMPACT_NODES: usize = 24;

/// The widest a box label may get before it is abbreviated with `…`.
const LABEL_WIDTH: usize = 18;

/// Boxes are three rows tall with one blank row between them.
const ROW_STRIDE: usize = 4;

/// The gutter between box columns: wide enough for one bend column and an
/// arrowhead on either side.
const GUTTER: usize = 6;

/// Which way arrowheads point. Dependencies point away from the root (at the
/// producer); impact points back at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arrow {
    /// Arrowheads on the far side of each gutter — dependencies.
    Right,
    /// Arrowheads on the near side — impact.
    Left,
}

/// Renders one slice as a diagram, or — past the size threshold — as a
/// numbered legend with a pointer to the machine modes.
pub(crate) fn diagram(slice: &DepSlice, arrow: Arrow) -> String {
    let mut ids: Vec<ObjectId> = slice.nodes.clone();
    ids.sort();
    if ids.len() > COMPACT_NODES {
        return compact(slice, &ids);
    }
    if ids.len() <= 1 {
        // The root alone: nothing to route.
        let mut out = String::new();
        for id in &ids {
            let _ = writeln!(out, "[0] {}", id);
        }
        return out;
    }
    let number: HashMap<&ObjectId, usize> = ids.iter().enumerate().map(|(i, id)| (id, i)).collect();

    let levels = bfs_levels(slice, root_of(slice, &ids));
    let layers = group_by_level(&ids, &levels);
    let rows = assign_rows(&layers, slice);

    // The number prefix widens as the slice does ("[0] " vs "[10] "); budget
    // the widest one the numbering will use, or box labels overflow the box.
    let prefix_width = format!("[{}] ", ids.len() - 1).chars().count();
    let box_width = ids
        .iter()
        .map(|id| abbreviated(id).chars().count())
        .max()
        .unwrap_or(0)
        + prefix_width;
    let label_of = |id: &ObjectId| format!("[{}] {}", number[id], abbreviated(id));

    let mut canvas = Canvas::new(layers.len(), box_width, &rows);
    // Boxes first; edges may then only write on empty cells.
    for (level, layer) in layers.iter().enumerate() {
        for id in layer {
            canvas.draw_box(level, rows[id], &label_of(id));
        }
    }
    for edge in &slice.edges {
        // Route between the two boxes' columns; the arrowhead lands on the
        // used side — away from the root for dependencies, at the root for
        // impact.
        let (Some(ln), Some(lf)) = (levels.get(&edge.from), levels.get(&edge.to)) else {
            continue;
        };
        if lf.abs_diff(*ln) != 1 {
            // A cycle or a long jump stays unrouted: the tree and the machine
            // modes show it exactly, and this view is for the shape.
            continue;
        }
        let (near_row, far_row) = if ln <= lf {
            (rows[&edge.from], rows[&edge.to])
        } else {
            (rows[&edge.to], rows[&edge.from])
        };
        canvas.route(
            *ln.min(lf),
            near_row,
            far_row,
            match arrow {
                Arrow::Right => Arrow::Right,
                Arrow::Left => Arrow::Left,
            },
        );
    }
    canvas.render()
}

fn root_of<'a>(slice: &'a DepSlice, ids: &'a [ObjectId]) -> &'a ObjectId {
    ids.iter().find(|id| **id == slice.root).unwrap_or(&ids[0])
}

/// Minimum hop counts from the root over the slice's edges, both directions
/// — layering is a drawing concern, and undirected layers keep every edge
/// between adjacent columns except deliberate back-references.
fn bfs_levels(slice: &DepSlice, root: &ObjectId) -> HashMap<ObjectId, usize> {
    let mut adjacency: HashMap<&ObjectId, Vec<&ObjectId>> = HashMap::new();
    for edge in &slice.edges {
        adjacency.entry(&edge.from).or_default().push(&edge.to);
        adjacency.entry(&edge.to).or_default().push(&edge.from);
    }
    let mut levels: HashMap<ObjectId, usize> = HashMap::from([(root.clone(), 0)]);
    let mut queue: VecDeque<&ObjectId> = VecDeque::from([root]);
    while let Some(id) = queue.pop_front() {
        let next = levels[id] + 1;
        for neighbor in adjacency.get(id).into_iter().flatten() {
            if !levels.contains_key(*neighbor) {
                levels.insert((*neighbor).clone(), next);
                queue.push_back(neighbor);
            }
        }
    }
    levels
}

/// Nodes grouped by level, identity-sorted within a level.
fn group_by_level<'a>(
    ids: &'a [ObjectId],
    levels: &HashMap<ObjectId, usize>,
) -> Vec<Vec<&'a ObjectId>> {
    let mut layers: BTreeMap<usize, Vec<&ObjectId>> = BTreeMap::new();
    for id in ids {
        layers.entry(levels[id]).or_default().push(id);
    }
    layers.into_values().collect()
}

/// One row per node: the first layer at the top, later layers ordered by the
/// average row of their already-rowed neighbors (a single barycenter pass),
/// ties by identity, collisions pushed down.
fn assign_rows<'a>(layers: &[Vec<&'a ObjectId>], slice: &DepSlice) -> HashMap<&'a ObjectId, usize> {
    let mut rows: HashMap<&ObjectId, usize> = HashMap::new();
    for layer in layers {
        // Desired rows first, borrowing `rows` alone; assignment mutates it
        // after.
        let mut ordered: Vec<(&ObjectId, usize)> = layer
            .iter()
            .map(|&id| {
                let neighbors = slice
                    .edges
                    .iter()
                    .filter_map(|edge| {
                        if edge.from == *id {
                            rows.get(&edge.to).copied()
                        } else if edge.to == *id {
                            rows.get(&edge.from).copied()
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                let desired = if neighbors.is_empty() {
                    usize::MAX
                } else {
                    neighbors.iter().sum::<usize>() / neighbors.len()
                };
                (id, desired)
            })
            .collect();
        ordered.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
        let mut last_row = 0;
        for (id, desired) in ordered {
            let row = if desired == usize::MAX {
                last_row
            } else {
                desired.max(last_row)
            };
            let row = row.div_ceil(ROW_STRIDE) * ROW_STRIDE;
            rows.insert(id, row);
            last_row = row + ROW_STRIDE;
        }
    }
    rows
}

/// The abbreviated box label of an object: the identity, cut to
/// [`LABEL_WIDTH`] with an ellipsis when longer.
fn abbreviated(id: &ObjectId) -> String {
    let display = id.to_string();
    if display.chars().count() <= LABEL_WIDTH {
        display
    } else {
        let head: String = display.chars().take(LABEL_WIDTH - 1).collect();
        format!("{head}…")
    }
}

/// A character grid with box-drawing helpers. Writes on empty cells only, so
/// an unlucky edge bend can never eat a box.
struct Canvas {
    grid: Vec<Vec<char>>,
    box_width: usize,
}

impl Canvas {
    fn new(layers: usize, box_width: usize, rows: &HashMap<&ObjectId, usize>) -> Self {
        let height = rows.values().copied().max().unwrap_or(0) + 3;
        let width = layers * (box_width + GUTTER) + GUTTER;
        Self {
            grid: vec![vec![' '; width]; height],
            box_width,
        }
    }

    /// Draws `[label]` as a three-row box in its layer column.
    fn draw_box(&mut self, layer: usize, row: usize, label: &str) {
        let x = layer * (self.box_width + GUTTER);
        let width = self.box_width + 2;
        let pad = width - 2 - label.chars().count();
        for (dy, line) in [
            format!("┌{}┐", "─".repeat(width - 2)),
            format!("│{label}{}│", " ".repeat(pad)),
            format!("└{}┘", "─".repeat(width - 2)),
        ]
        .into_iter()
        .enumerate()
        {
            for (dx, ch) in line.chars().enumerate() {
                self.put(x + dx, row + dy, ch);
            }
        }
    }

    /// Routes one adjacent-layer edge across the gutter: a straight line when
    /// the rows align, one bend column otherwise. The arrowhead lands on the
    /// used side.
    fn route(&mut self, near_layer: usize, near_row: usize, far_row: usize, arrow: Arrow) {
        // The gutter runs between the two borders: from the cell after the
        // near box's right border to the cell before the far box.
        let x0 = near_layer * (self.box_width + GUTTER) + self.box_width + 2;
        let x1 = x0 + GUTTER - 2;
        let (head_near, head_far) = match arrow {
            Arrow::Right => (' ', '►'),
            Arrow::Left => ('◄', ' '),
        };
        if near_row == far_row {
            for x in x0..x1 {
                self.put(x, near_row + 1, '─');
            }
            // The head overwrites the line's end cell by design.
            if head_near != ' ' {
                self.force(x0, near_row + 1, head_near);
            }
            if head_far != ' ' {
                self.force(x1 - 1, near_row + 1, head_far);
            }
            return;
        }
        // Bend route: ──┐ / │ / ┌──  (mirrored when climbing).
        let bend = x0 + 1;
        let (top, bottom) = if near_row < far_row {
            (near_row + 1, far_row + 1)
        } else {
            (far_row + 1, near_row + 1)
        };
        self.put(x0, near_row + 1, '─');
        self.put(x0 + 1, near_row + 1, '─');
        // The corner at the origin row turns toward the other row.
        let origin_corner = if near_row < far_row { '┐' } else { '┘' };
        self.put(bend, near_row + 1, origin_corner);
        for y in top + 1..bottom {
            self.put(bend, y, '│');
        }
        let target_corner = if near_row < far_row { '└' } else { '┌' };
        self.put(bend, far_row + 1, target_corner);
        self.put(bend + 1, far_row + 1, '─');
        for x in bend + 2..x1 {
            self.put(x, far_row + 1, '─');
        }
        if head_far != ' ' {
            self.force(x1 - 1, far_row + 1, head_far);
        }
        if head_near != ' ' {
            self.force(x0, near_row + 1, head_near);
        }
    }

    /// Writes even over drawn cells — arrowheads land on line ends.
    fn force(&mut self, x: usize, y: usize, ch: char) {
        if let Some(line) = self.grid.get_mut(y)
            && let Some(cell) = line.get_mut(x)
        {
            *cell = ch;
        }
    }

    fn put(&mut self, x: usize, y: usize, ch: char) {
        if let Some(line) = self.grid.get_mut(y)
            && let Some(cell) = line.get_mut(x)
            && *cell == ' '
        {
            *cell = ch;
        }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for line in &self.grid {
            let text: String = line.iter().collect();
            let _ = writeln!(out, "{}", text.trim_end());
        }
        out
    }
}

/// The over-threshold form: numbered legend, then the pointer at the modes
/// that never truncate.
fn compact(slice: &DepSlice, ids: &[ObjectId]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} objects — too many to draw. Numbered graph:",
        slice.nodes.len()
    );
    for (i, id) in ids.iter().enumerate() {
        let _ = writeln!(out, "[{i}] {id}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ripbi_core::{DepEdge, NameKey, Provenance, StructuralEdge};

    fn id(name: &str) -> ObjectId {
        ObjectId::Measure {
            table: NameKey::new("Sales"),
            measure: NameKey::new(name),
        }
    }

    fn edge(from: ObjectId, to: ObjectId) -> DepEdge {
        DepEdge {
            from,
            to,
            provenance: Provenance::Structural {
                role: StructuralEdge::TableMember,
            },
        }
    }

    /// A → B → C: one box per layer, arrows along the chain.
    #[test]
    fn a_chain_draws_three_layers_with_arrows() {
        let (a, b, c) = (id("A"), id("B"), id("C"));
        let slice = DepSlice {
            root: a.clone(),
            depth: None,
            nodes: vec![a.clone(), b.clone(), c.clone()],
            edges: vec![edge(a, b.clone()), edge(b, c)],
        };

        let out = diagram(&slice, Arrow::Right);

        assert!(out.contains("[0] 'Sales'[A]"));
        assert!(
            out.contains("───►"),
            "arrows point at the used side:
{out}"
        );
        assert!(out.contains('│'), "bent routes use verticals");
    }

    #[test]
    fn impact_arrows_point_back_at_the_root() {
        let (a, b) = (id("A"), id("B"));
        let slice = DepSlice {
            root: a.clone(),
            depth: None,
            nodes: vec![a.clone(), b.clone()],
            edges: vec![edge(b, a)],
        };

        let out = diagram(&slice, Arrow::Left);

        assert!(
            out.contains('◄'),
            "impact arrowheads face the root:
{out}"
        );
    }

    #[test]
    fn rendering_is_deterministic() {
        let (a, b, c) = (id("A"), id("B"), id("C"));
        let slice = DepSlice {
            root: a.clone(),
            depth: None,
            nodes: vec![a.clone(), b.clone(), c.clone()],
            edges: vec![edge(a.clone(), b), edge(a, c)],
        };

        assert_eq!(diagram(&slice, Arrow::Right), diagram(&slice, Arrow::Right));
    }

    #[test]
    fn a_large_slice_degrades_to_the_numbered_legend() {
        let a = id("A");
        let mut nodes = vec![a.clone()];
        let mut edges = Vec::new();
        for i in 0..COMPACT_NODES + 1 {
            let node = id(&format!("N{i}"));
            edges.push(edge(a.clone(), node.clone()));
            nodes.push(node);
        }
        let slice = DepSlice {
            root: a,
            depth: None,
            nodes,
            edges,
        };

        let out = diagram(&slice, Arrow::Right);

        assert!(out.contains("too many to draw"));
        assert!(
            out.contains("[0] 'Sales'[A]"),
            "the legend numbers every node"
        );
        assert!(
            !out.contains('┌'),
            "no boxes are drawn:
{out}"
        );
    }

    #[test]
    fn a_single_node_draws_no_boxes() {
        let a = id("A");
        let slice = DepSlice {
            root: a.clone(),
            depth: None,
            nodes: vec![a],
            edges: Vec::new(),
        };

        let out = diagram(&slice, Arrow::Right);

        assert_eq!(
            out,
            "[0] 'Sales'[A]
"
        );
    }

    /// The box width budgets the label prefix; node numbers past nine need a
    /// wider prefix ("[10] " vs "[0] "), and when such a node also carries
    /// the widest label the padding arithmetic used to underflow.
    #[test]
    fn double_digit_node_numbers_draw_without_underflowing_the_box_width() {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let root = id("A");
        nodes.push(root.clone());
        for name in ["B", "C", "D", "E", "F", "G", "H", "I", "J", "Widest label"] {
            let node = id(name);
            edges.push(edge(root.clone(), node.clone()));
            nodes.push(node);
        }
        let slice = DepSlice {
            root,
            depth: None,
            nodes,
            edges,
        };

        let out = diagram(&slice, Arrow::Right);

        assert!(
            out.contains("[10] 'Sales'[Widest la…"),
            "the two-digit, widest-label node renders:
{out}"
        );
    }
}
