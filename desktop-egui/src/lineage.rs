//! The Lineage tab: one object's upstream (what it uses, to the left) and
//! downstream (what uses it, out to the report visuals on the right) as
//! columns by edge distance, painted directly with egui. Every edge runs from
//! the used object to its user, so data flows left to right.
//!
//! `ripbi_app::deps::load` builds the graph once per analysis on a worker
//! thread; each focus change then asks the loaded `Explorer` for a fresh
//! view, which stays in memory and is quick.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui::{
    self, Align, Color32, ComboBox, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit,
    pos2, vec2,
};
use ripbi_app::deps::{DepsOutput, DepsRequest, Explorer, FocusedOut};
use ripbi_app::output::kind_of;
use ripbi_app::scan::Observer;
use ripbi_core::graph::DepSlice;
use ripbi_core::{BindingEdge, ObjectId};

use crate::inventory::{Inventory, Pick};

/// More than this many objects in one column collapse into a "+N more" node.
const CAP: usize = 40;
const NODE_W: f32 = 210.0;
const NODE_H: f32 = 40.0;
const GAP_X: f32 = 90.0;
/// The narrowest nodes and gaps a graph shrinks to before it scrolls.
const MIN_NODE_W: f32 = 120.0;
const MIN_GAP_X: f32 = 36.0;
const GAP_Y: f32 = 10.0;
const MARGIN: f32 = 24.0;

/// The tab's state for one analysis: the loaded graph and where the user is.
pub struct Lineage {
    base: DepsRequest,
    /// The scan's unused findings, by display id, to dim in the graph.
    unused: HashSet<String>,
    loading: Option<Receiver<Result<(Explorer, Inventory), String>>>,
    explorer: Option<Explorer>,
    /// Every object as `(display id, lowercased, id)`, sorted, for search.
    objects: Vec<(String, String, ObjectId)>,
    error: Option<String>,
    /// An object asked for before the graph finished loading.
    pending: Option<String>,
    focus: Option<ObjectId>,
    back: Vec<ObjectId>,
    depth: Option<usize>,
    upstream: bool,
    downstream: bool,
    search: String,
    graph: Option<Graph>,
    /// Every object, to browse beside the graph and on the Inventory tab.
    inventory: Option<Inventory>,
    /// The object selected on the Inventory tab, with its direct links.
    picked: Option<Detail>,
    /// Whether the object tree shows beside the graph.
    browse: bool,
    /// Scroll the focused object into view on the next frame.
    recenter: bool,
}

impl Lineage {
    pub fn new(base: DepsRequest, unused: HashSet<String>) -> Self {
        Self {
            base,
            unused,
            loading: None,
            explorer: None,
            objects: Vec::new(),
            error: None,
            pending: None,
            focus: None,
            back: Vec::new(),
            depth: Some(2),
            upstream: true,
            downstream: true,
            search: String::new(),
            graph: None,
            inventory: None,
            picked: None,
            browse: true,
            recenter: false,
        }
    }

    /// Starts loading the graph on a worker thread, once.
    pub fn ensure_loaded(&mut self, ctx: &egui::Context) {
        if self.explorer.is_some() || self.loading.is_some() || self.error.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let base = self.base.clone();
        let unused = self.unused.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            _ = tx.send(load(&base).map(|explorer| {
                let inventory = Inventory::build(&explorer, &unused);
                (explorer, inventory)
            }));
            ctx.request_repaint();
        });
        self.loading = Some(rx);
    }

    /// Focuses the object whose display id is `id` — now, or once loaded.
    pub fn show(&mut self, ctx: &egui::Context, id: &str) {
        self.ensure_loaded(ctx);
        self.pending = Some(id.to_owned());
        self.take_pending();
    }

    pub fn poll(&mut self) {
        let Some(loading) = &self.loading else { return };
        match loading.try_recv() {
            Ok(Ok((explorer, inventory))) => {
                let mut objects: Vec<_> = explorer
                    .object_ids()
                    .map(|id| {
                        let text = id.to_string();
                        (text.clone(), text.to_lowercase(), id.clone())
                    })
                    .collect();
                objects.sort();
                self.objects = objects;
                self.inventory = Some(inventory);
                self.explorer = Some(explorer);
                self.loading = None;
                self.take_pending();
            }
            Ok(Err(error)) => {
                self.error = Some(error);
                self.loading = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.error = Some("Lineage worker stopped unexpectedly.".into());
                self.loading = None;
            }
        }
    }

    fn take_pending(&mut self) {
        if self.explorer.is_none() {
            return;
        }
        let Some(wanted) = self.pending.take() else {
            return;
        };
        match self.objects.iter().find(|(text, ..)| *text == wanted) {
            Some((.., id)) => {
                let id = id.clone();
                self.go(id);
            }
            None => self.error = Some(format!("{wanted} is not in the dependency graph.")),
        }
    }

    /// Moves the focus, remembering the previous one for Back, and opens the
    /// object tree to it.
    fn go(&mut self, id: ObjectId) {
        if let Some(inventory) = &mut self.inventory {
            inventory.reveal(&id);
        }
        self.focus_on(id);
    }

    /// Moves the focus without touching the tree: for a pick made there.
    fn focus_on(&mut self, id: ObjectId) {
        if self.focus.as_ref() == Some(&id) {
            self.recenter = true;
            return;
        }
        if let Some(previous) = self.focus.replace(id) {
            self.back.push(previous);
        }
        self.refresh();
    }

    fn refresh(&mut self) {
        let (Some(explorer), Some(focus)) = (&self.explorer, &self.focus) else {
            return;
        };
        let request = DepsRequest {
            object: Some(focus.to_string()),
            depth: self.depth.map(|depth| depth.to_string()),
            dependencies: self.upstream,
            impact: self.downstream,
            ..DepsRequest::default()
        };
        match explorer.view(&request) {
            Ok(DepsOutput::Focused(focused)) => {
                self.graph = Some(Graph::build(&focused, &self.unused));
                self.error = None;
            }
            Ok(DepsOutput::Overview(_)) => self.graph = None,
            Err(error) => self.error = Some(error.to_string()),
        }
        self.recenter = true;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.ensure_loaded(ui.ctx());
        if let Some(error) = &self.error {
            ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
        }
        if self.explorer.is_none() {
            if self.loading.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Building the dependency graph…");
                });
            }
            return;
        }

        let mut browsed = None;
        if self.browse
            && let Some(inventory) = &mut self.inventory
        {
            egui::Panel::left("lineage-browse")
                .resizable(true)
                .default_size(280.0)
                .min_size(200.0)
                .show(ui, |ui| browsed = inventory.ui(ui, self.focus.as_ref()));
        }
        if let Some(Pick::Select(id) | Pick::Open(id)) = browsed {
            self.focus_on(id);
        }

        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(egui::Button::selectable(self.browse, "Objects"))
                .on_hover_text("Show or hide the object tree, to give the graph more room")
                .clicked()
            {
                self.browse = !self.browse;
                self.recenter = true;
            }
            if ui
                .add_enabled(!self.back.is_empty(), egui::Button::new("←"))
                .on_hover_text("Back to the previous object")
                .clicked()
                && let Some(previous) = self.back.pop()
            {
                if let Some(inventory) = &mut self.inventory {
                    inventory.reveal(&previous);
                }
                self.focus = Some(previous);
                changed = true;
            }
            self.search_box(ui);
            ui.separator();
            ui.label("Depth");
            ComboBox::from_id_salt("lineage-depth")
                .width(56.0)
                .selected_text(self.depth.map_or("All".into(), |d| d.to_string()))
                .show_ui(ui, |ui| {
                    for depth in [Some(1), Some(2), Some(3), Some(4), None] {
                        let text = depth.map_or("All".into(), |d: usize| d.to_string());
                        changed |= ui.selectable_value(&mut self.depth, depth, text).changed();
                    }
                });
            // At least one direction stays on: neither would mean both.
            let upstream = ui
                .add_enabled(
                    self.downstream,
                    egui::Checkbox::new(&mut self.upstream, "Uses"),
                )
                .on_hover_text("What this object depends on (left)");
            let downstream = ui
                .add_enabled(
                    self.upstream,
                    egui::Checkbox::new(&mut self.downstream, "Used by"),
                )
                .on_hover_text("What depends on this object, out to report visuals (right)");
            changed |= upstream.changed() || downstream.changed();
        });
        if changed {
            self.refresh();
        }
        legend(ui);
        ui.separator();

        let Some(graph) = &self.graph else {
            ui.label(
                RichText::new(
                    "Pick an object on the left (or search above) to draw what it uses \
                     and what uses it.",
                )
                .weak(),
            );
            return;
        };
        if let Some(id) = graph.paint(ui, std::mem::take(&mut self.recenter)) {
            self.go(id);
        }
    }

    /// The Inventory tab: the object tree beside the selected object's
    /// direct links. True when the user asked for the selection's lineage.
    pub fn inventory_ui(&mut self, ui: &mut egui::Ui) -> bool {
        self.ensure_loaded(ui.ctx());
        if let Some(error) = &self.error {
            ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
        }
        let Some(inventory) = &mut self.inventory else {
            if self.loading.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading the analyzed objects…");
                });
            }
            return false;
        };

        let mut pick = None;
        egui::Panel::right("inventory-detail")
            .resizable(true)
            .default_size(360.0)
            .min_size(240.0)
            .show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| match &self.picked {
                    Some(detail) => pick = detail.ui(ui),
                    None => {
                        ui.label(
                            RichText::new(
                                "Select an object to see what it uses and what uses it. \
                                 Double-click one for its lineage.",
                            )
                            .weak(),
                        );
                    }
                });
            });
        let selected = self.picked.as_ref().map(|detail| &detail.id);
        if let Some(clicked) = inventory.ui(ui, selected) {
            pick = Some(clicked);
        }
        match pick {
            Some(Pick::Select(id)) => {
                self.pick(id);
                false
            }
            Some(Pick::Open(id)) => {
                self.pick(id.clone());
                self.go(id);
                true
            }
            None => false,
        }
    }

    /// Selects `id` on the Inventory tab, with its one-step neighbourhood.
    fn pick(&mut self, id: ObjectId) {
        let Some(explorer) = &self.explorer else {
            return;
        };
        let request = DepsRequest {
            object: Some(id.to_string()),
            depth: Some("1".into()),
            dependencies: true,
            impact: true,
            ..DepsRequest::default()
        };
        match explorer.view(&request) {
            Ok(DepsOutput::Focused(focused)) => {
                self.picked = Some(Detail::new(&focused, &self.unused));
            }
            Ok(DepsOutput::Overview(_)) => {}
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn search_box(&mut self, ui: &mut egui::Ui) {
        let edit = ui.add(
            TextEdit::singleline(&mut self.search)
                .hint_text("Find an object…")
                .desired_width(260.0),
        );
        let needle = self.search.trim().to_lowercase();
        if needle.is_empty() {
            return;
        }
        let matches: Vec<&(String, String, ObjectId)> = self
            .objects
            .iter()
            .filter(|(_, folded, _)| folded.contains(&needle))
            .take(12)
            .collect();
        let mut picked = None;
        let popup = egui::Popup::from_response(&edit)
            .open(edit.has_focus() || edit.lost_focus())
            .width(edit.rect.width().max(320.0));
        popup.show(|ui| {
            if matches.is_empty() {
                ui.label(RichText::new("No matching objects").weak());
            }
            for (text, _, id) in &matches {
                let row = ui.horizontal(|ui| {
                    ui.label(RichText::new(kind_of(id)).small().color(tint(kind_of(id))));
                    ui.add(egui::Button::selectable(false, text.as_str()))
                });
                if row.inner.clicked() {
                    picked = Some(id.clone());
                }
            }
        });
        if edit.lost_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Enter))
            && let Some((.., id)) = matches.first()
        {
            picked = Some(id.clone());
        }
        if let Some(id) = picked {
            self.search.clear();
            self.go(id);
        }
    }
}

/// One object's direct links, for the Inventory tab's detail panel.
struct Detail {
    id: ObjectId,
    unused: bool,
    kept: Option<String>,
    queried: Option<usize>,
    /// `(object, why)`, for what it uses and what uses it.
    uses: Vec<(ObjectId, String)>,
    used_by: Vec<(ObjectId, String)>,
    /// Where reports bind it, as a person would find it in Power BI Desktop.
    visuals: Vec<String>,
}

impl Detail {
    fn new(focused: &FocusedOut, unused: &HashSet<String>) -> Self {
        let id = focused.roots[0].clone();
        let edges = |slices: &Option<Vec<DepSlice>>| {
            slices
                .as_ref()
                .and_then(|slices| slices.first())
                .map(|slice| slice.edges.clone())
                .unwrap_or_default()
        };
        let mut uses: Vec<_> = edges(&focused.dependencies)
            .into_iter()
            .filter(|edge| edge.from == id)
            .map(|edge| (edge.to, edge.provenance.to_string()))
            .collect();
        let mut used_by: Vec<_> = edges(&focused.impact)
            .into_iter()
            .filter(|edge| edge.to == id)
            .map(|edge| (edge.from, edge.provenance.to_string()))
            .collect();
        for list in [&mut uses, &mut used_by] {
            list.sort_by_key(|(other, _)| (kind_of(other), short_name(other).to_lowercase()));
            list.dedup_by(|a, b| a.0 == b.0);
        }
        let mut visuals: Vec<String> = focused
            .bindings
            .first()
            .into_iter()
            .flatten()
            .filter(|(target, _)| *target == id)
            .map(|(_, binding)| crate::inventory::site_text(binding))
            .collect();
        visuals.sort();
        visuals.dedup();
        Self {
            unused: unused.contains(&id.to_string()),
            kept: focused
                .kept
                .first()
                .into_iter()
                .flatten()
                .find(|(kept, _)| *kept == id)
                .map(|(_, kept)| kept.reason.clone()),
            queried: focused
                .queried
                .first()
                .into_iter()
                .flatten()
                .find(|queried| queried.id == id)
                .map(|queried| queried.count),
            id,
            uses,
            used_by,
            visuals,
        }
    }

    fn ui(&self, ui: &mut egui::Ui) -> Option<Pick> {
        let kind = kind_of(&self.id);
        let mut pick = None;
        ui.label(
            RichText::new(kind.replace('_', " ").to_uppercase())
                .small()
                .color(tint(kind)),
        );
        ui.heading(short_name(&self.id));
        ui.label(RichText::new(self.id.to_string()).weak());
        ui.add_space(4.0);
        let status = if self.unused {
            RichText::new("Unused: no report, role or kept object reaches it")
                .color(ui.visuals().warn_fg_color)
        } else if let Some(reason) = &self.kept {
            RichText::new(format!("Kept by ripbi_keep: {reason}"))
        } else {
            RichText::new("Used").weak()
        };
        ui.label(status);
        if let Some(count) = self.queried {
            ui.label(RichText::new(format!("Referenced by {count} logged queries")).weak());
        }
        ui.add_space(6.0);
        if ui.button("Show lineage  →").clicked() {
            pick = Some(Pick::Open(self.id.clone()));
        }

        for (title, links) in [("Uses", &self.uses), ("Used by", &self.used_by)] {
            ui.add_space(10.0);
            ui.label(RichText::new(format!("{title} ({})", links.len())).strong());
            if links.is_empty() {
                ui.label(RichText::new("Nothing").weak());
            }
            for (other, why) in links {
                ui.horizontal_wrapped(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter()
                        .circle_filled(dot.center(), 3.5, tint(kind_of(other)));
                    if ui
                        .add(egui::Button::selectable(false, short_name(other)).wrap())
                        .on_hover_text(other.to_string())
                        .clicked()
                    {
                        pick = Some(Pick::Select(other.clone()));
                    }
                    ui.label(RichText::new(why).small().weak());
                });
            }
        }
        ui.add_space(10.0);
        ui.label(RichText::new(format!("In report visuals ({})", self.visuals.len())).strong());
        if self.visuals.is_empty() {
            ui.label(RichText::new("Not bound by any report").weak());
        }
        for site in &self.visuals {
            ui.label(RichText::new(site).small());
        }
        pick
    }
}

/// Builds the analysis's graph, quietly: the scan already reported its notes.
pub fn load(base: &DepsRequest) -> Result<Explorer, String> {
    struct Quiet;
    impl Observer for Quiet {}
    ripbi_app::deps::load(base, &mut Quiet).map_err(|error| error.to_string())
}

/// A laid-out view: nodes in columns by signed distance from the focus.
pub struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    /// Column → node indices, top to bottom.
    columns: BTreeMap<i32, Vec<usize>>,
}

struct Node {
    what: What,
    /// The object's own name, or the visual's title.
    title: String,
    /// The kind, and the table or page it sits in.
    subtitle: String,
    /// The full identity or binding site, for the tooltip.
    detail: String,
    column: i32,
    unused: bool,
    /// The `ripbi_keep` reason keeping it, when one does.
    kept: Option<String>,
    /// How many logged queries reference it.
    queried: Option<usize>,
}

enum What {
    Object(ObjectId),
    Visual,
    More(usize),
}

/// One edge, used → user.
struct Edge {
    from: usize,
    to: usize,
    why: String,
}

impl Graph {
    fn build(focused: &FocusedOut, unused: &HashSet<String>) -> Self {
        let mut nodes: Vec<Node> = Vec::new();
        let mut index: HashMap<ObjectId, usize> = HashMap::new();
        let mut add = |id: &ObjectId, column: i32, nodes: &mut Vec<Node>| {
            index.entry(id.clone()).or_insert_with(|| {
                nodes.push(object_node(id, column, unused));
                nodes.len() - 1
            });
        };
        let root = &focused.roots[0];
        add(root, 0, &mut nodes);
        let upstream = focused
            .dependencies
            .as_ref()
            .and_then(|slices| slices.first());
        let downstream = focused.impact.as_ref().and_then(|slices| slices.first());
        if let Some(slice) = upstream {
            for (id, distance) in distances(slice, |edge| (&edge.from, &edge.to)) {
                add(id, -(distance as i32), &mut nodes);
            }
        }
        if let Some(slice) = downstream {
            for (id, distance) in distances(slice, |edge| (&edge.to, &edge.from)) {
                add(id, distance as i32, &mut nodes);
            }
        }

        let mut edges = Vec::new();
        let mut seen = HashSet::new();
        for slice in upstream.into_iter().chain(downstream) {
            for edge in &slice.edges {
                if let (Some(&from), Some(&to)) = (index.get(&edge.to), index.get(&edge.from))
                    && seen.insert((from, to))
                {
                    edges.push(Edge {
                        from,
                        to,
                        why: edge.provenance.to_string(),
                    });
                }
            }
        }

        // Report bindings: one node per visual (or per page or report for
        // filters outside visuals), a column right of its furthest field.
        let mut visuals: HashMap<String, usize> = HashMap::new();
        for (target, binding) in focused.bindings.first().into_iter().flatten() {
            let Some(&from) = index.get(target) else {
                continue;
            };
            let column = nodes[from].column + 1;
            let key = site_key(binding);
            let to = *visuals.entry(key).or_insert_with(|| {
                nodes.push(visual_node(binding, column));
                nodes.len() - 1
            });
            nodes[to].column = nodes[to].column.max(column);
            if seen.insert((from, to)) {
                edges.push(Edge {
                    from,
                    to,
                    why: binding.located().to_string(),
                });
            }
        }
        for (id, kept) in focused.kept.first().into_iter().flatten() {
            if let Some(&i) = index.get(id) {
                nodes[i].kept = Some(kept.reason.clone());
            }
        }
        for queried in focused.queried.first().into_iter().flatten() {
            if let Some(&i) = index.get(&queried.id) {
                nodes[i].queried = Some(queried.count);
            }
        }

        let columns = arrange(&mut nodes, &edges);
        Self {
            nodes,
            edges,
            columns,
        }
    }

    /// Paints the graph; returns the object clicked to focus on.
    fn paint(&self, ui: &mut egui::Ui, recenter: bool) -> Option<ObjectId> {
        let (Some(&first), Some(&last)) = (self.columns.keys().next(), self.columns.keys().last())
        else {
            return None;
        };
        let tallest = self.columns.values().map(Vec::len).max().unwrap_or(1);
        let columns = (last - first + 1) as f32;
        let (node_w, gap_x) = fit(columns, ui.available_width());
        let width = columns * (node_w + gap_x) - gap_x + 2.0 * MARGIN;
        let height = tallest as f32 * (NODE_H + GAP_Y) - GAP_Y + 2.0 * MARGIN;
        let mut clicked = None;
        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            let size = vec2(width, height).max(ui.available_size());
            let (canvas, _) = ui.allocate_exact_size(size, Sense::hover());
            let mut rects = vec![Rect::NOTHING; self.nodes.len()];
            for (&column, members) in &self.columns {
                let x = canvas.left()
                    + (canvas.width() - width) / 2.0
                    + MARGIN
                    + (column - first) as f32 * (node_w + gap_x);
                let top =
                    canvas.center().y - (members.len() as f32 * (NODE_H + GAP_Y) - GAP_Y) / 2.0;
                for (row, &i) in members.iter().enumerate() {
                    let y = top + row as f32 * (NODE_H + GAP_Y);
                    rects[i] = Rect::from_min_size(pos2(x, y), vec2(node_w, NODE_H));
                }
            }

            // Interact first so the hovered node can light its edges.
            let mut hovered = None;
            let mut responses = Vec::with_capacity(self.nodes.len());
            for (i, rect) in rects.iter().enumerate() {
                let response = ui.interact(
                    *rect,
                    ui.scope_id().with(("lineage-node", i)),
                    Sense::click(),
                );
                if response.hovered() {
                    hovered = Some(i);
                }
                responses.push(response);
            }

            let painter = ui.painter();
            let quiet = ui.visuals().weak_text_color().gamma_multiply(0.5);
            for edge in &self.edges {
                let lit = hovered.is_some_and(|i| i == edge.from || i == edge.to);
                let color = if lit {
                    tint(self.nodes[edge.from].kind())
                } else {
                    quiet
                };
                connect(
                    painter,
                    rects[edge.from],
                    rects[edge.to],
                    Stroke::new(if lit { 2.0 } else { 1.0 }, color),
                );
            }
            for (i, node) in self.nodes.iter().enumerate() {
                let focused = node.column == 0 && matches!(node.what, What::Object(_));
                paint_node(ui, node, rects[i], focused, hovered == Some(i));
            }

            for (i, response) in responses.into_iter().enumerate() {
                let node = &self.nodes[i];
                let response = response.on_hover_ui(|ui| self.tooltip(ui, i));
                if let What::Object(id) = &node.what
                    && node.column != 0
                {
                    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
                    if response.clicked() {
                        clicked = Some(id.clone());
                    }
                }
            }
            if recenter && let Some(root) = self.columns.get(&0).and_then(|c| c.first()) {
                ui.scroll_to_rect(rects[*root], Some(Align::Center));
            }
        });
        clicked
    }

    fn tooltip(&self, ui: &mut egui::Ui, i: usize) {
        let node = &self.nodes[i];
        ui.set_max_width(460.0);
        ui.label(
            RichText::new(&node.subtitle)
                .small()
                .color(tint(node.kind())),
        );
        ui.label(RichText::new(&node.detail).strong());
        if node.unused {
            ui.label(RichText::new("Unused: no report or kept object reaches it.").weak());
        }
        if let Some(reason) = &node.kept {
            ui.label(RichText::new(format!("Kept by ripbi_keep: {reason}")).weak());
        }
        if let Some(count) = node.queried {
            ui.label(RichText::new(format!("Referenced by {count} logged queries")).weak());
        }
        if let What::More(count) = node.what {
            ui.label(format!(
                "{count} more objects at this distance. Lower the depth, or focus a \
                 nearer object, to see them."
            ));
        }
        for (title, incoming) in [("Uses", true), ("Used by", false)] {
            let lines: Vec<String> = self
                .edges
                .iter()
                .filter(|edge| {
                    if incoming {
                        edge.to == i
                    } else {
                        edge.from == i
                    }
                })
                .map(|edge| {
                    let other = if incoming { edge.from } else { edge.to };
                    format!("{} · {}", self.nodes[other].title, edge.why)
                })
                .collect();
            if lines.is_empty() {
                continue;
            }
            ui.add_space(4.0);
            ui.label(RichText::new(title).small().strong());
            for line in lines.iter().take(8) {
                ui.label(RichText::new(line).small());
            }
            if lines.len() > 8 {
                ui.label(
                    RichText::new(format!("…and {} more", lines.len() - 8))
                        .small()
                        .weak(),
                );
            }
        }
        if matches!(node.what, What::Object(_)) && node.column != 0 {
            ui.add_space(4.0);
            ui.label(RichText::new("Click to focus this object").small().weak());
        }
    }
}

impl Node {
    fn kind(&self) -> &'static str {
        match &self.what {
            What::Object(id) => kind_of(id),
            What::Visual => "visual",
            What::More(_) => "more",
        }
    }
}

/// Every node of `slice` with its edge distance from the root, walking each
/// edge from `step(edge).0` to `step(edge).1`.
fn distances<'a>(
    slice: &'a DepSlice,
    step: impl Fn(&'a ripbi_core::graph::DepEdge) -> (&'a ObjectId, &'a ObjectId),
) -> Vec<(&'a ObjectId, usize)> {
    let mut out = vec![(&slice.root, 0)];
    let mut seen: HashSet<&ObjectId> = HashSet::from([&slice.root]);
    let mut queue = VecDeque::from([(&slice.root, 0)]);
    while let Some((id, distance)) = queue.pop_front() {
        for edge in &slice.edges {
            let (near, far) = step(edge);
            if near == id && seen.insert(far) {
                out.push((far, distance + 1));
                queue.push_back((far, distance + 1));
            }
        }
    }
    out
}

/// Orders each column by the average row of its neighbours one column nearer
/// the focus (fewer crossings), working outward, and caps it at [`CAP`].
fn arrange(nodes: &mut Vec<Node>, edges: &[Edge]) -> BTreeMap<i32, Vec<usize>> {
    let mut pending: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for (i, node) in nodes.iter().enumerate() {
        pending.entry(node.column).or_default().push(i);
    }
    let mut order: Vec<i32> = pending.keys().copied().collect();
    order.sort_by_key(|column| (column.abs(), *column < 0));

    let mut row: HashMap<usize, f32> = HashMap::new();
    let mut columns = BTreeMap::new();
    for column in order {
        let members = pending.remove(&column).unwrap_or_default();
        let inner = column - column.signum();
        let mut scored: Vec<(f32, usize)> = members
            .into_iter()
            .map(|i| {
                let ys: Vec<f32> = edges
                    .iter()
                    .filter_map(|edge| match (edge.from == i, edge.to == i) {
                        (true, _) => Some(edge.to),
                        (_, true) => Some(edge.from),
                        _ => None,
                    })
                    .filter(|&other| nodes[other].column == inner)
                    .filter_map(|other| row.get(&other).copied())
                    .collect();
                let score = if ys.is_empty() {
                    f32::INFINITY
                } else {
                    ys.iter().sum::<f32>() / ys.len() as f32
                };
                (score, i)
            })
            .collect();
        scored.sort_by(|(a, i), (b, j)| {
            a.total_cmp(b)
                .then_with(|| nodes[*i].kind().cmp(nodes[*j].kind()))
                .then_with(|| nodes[*i].title.cmp(&nodes[*j].title))
        });
        let hidden = scored.len().saturating_sub(CAP);
        scored.truncate(CAP);
        let mut members: Vec<usize> = scored.into_iter().map(|(_, i)| i).collect();
        if hidden > 0 {
            nodes.push(Node {
                what: What::More(hidden),
                title: format!("+{hidden} more"),
                subtitle: "Hidden at this depth".into(),
                detail: format!("{hidden} more objects"),
                column,
                unused: false,
                kept: None,
                queried: None,
            });
            members.push(nodes.len() - 1);
        }
        let middle = (members.len() as f32 - 1.0) / 2.0;
        for (r, &i) in members.iter().enumerate() {
            row.insert(i, r as f32 - middle);
        }
        columns.insert(column, members);
    }
    columns
}

/// The object's own name, without its table or kind: `Profit` for
/// `'Sales'[Profit]`. Relationships keep their endpoints.
pub(crate) fn short_name(id: &ObjectId) -> String {
    match id {
        ObjectId::Table { table } => table.as_str().to_owned(),
        ObjectId::Column { column: name, .. }
        | ObjectId::Measure { measure: name, .. }
        | ObjectId::Hierarchy {
            hierarchy: name, ..
        }
        | ObjectId::Partition {
            partition: name, ..
        }
        | ObjectId::CalculationItem { item: name, .. }
        | ObjectId::Role { role: name }
        | ObjectId::Expression { name }
        | ObjectId::Function { name }
        | ObjectId::ReportMeasure { measure: name } => name.as_str().to_owned(),
        _ => {
            let text = id.to_string();
            let kind = kind_of(id).replace('_', " ");
            text.strip_prefix(&format!("{kind} "))
                .map_or(text.clone(), str::to_owned)
        }
    }
}

fn object_node(id: &ObjectId, column: i32, unused: &HashSet<String>) -> Node {
    let detail = id.to_string();
    let title = short_name(id);
    let place = match id {
        ObjectId::Column { table, .. }
        | ObjectId::Measure { table, .. }
        | ObjectId::Hierarchy { table, .. }
        | ObjectId::Partition { table, .. }
        | ObjectId::CalculationItem { table, .. } => Some(table),
        _ => None,
    };
    let kind = kind_of(id).replace('_', " ").to_uppercase();
    Node {
        subtitle: match place {
            Some(table) => format!("{kind} · {}", table.as_str()),
            None => kind,
        },
        unused: unused.contains(&detail),
        what: What::Object(id.clone()),
        title,
        detail,
        column,
        kept: None,
        queried: None,
    }
}

fn visual_node(binding: &BindingEdge, column: i32) -> Node {
    let labels = &binding.labels;
    let page = labels
        .page
        .clone()
        .or_else(|| binding.page.as_ref().map(|page| page.as_str().to_owned()));
    let report = binding.report.as_ref().map(|report| report.as_str());
    let title = match (&binding.visual, &page, report) {
        (Some(visual), ..) => labels
            .visual_title
            .clone()
            .or_else(|| labels.visual_type.clone())
            .unwrap_or_else(|| visual.as_str().to_owned()),
        (None, Some(page), _) => format!("{page} filters"),
        (None, None, Some(report)) => report.to_owned(),
        (None, None, None) => "Report".into(),
    };
    let kind = match (&binding.visual, &labels.visual_type) {
        (Some(_), Some(visual_type)) => visual_type.to_uppercase(),
        (Some(_), None) => "VISUAL".into(),
        (None, _) => "REPORT".into(),
    };
    let place = [page.as_deref(), report]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    Node {
        what: What::Visual,
        subtitle: if place.is_empty() {
            kind
        } else {
            format!("{kind} · {}", place.join(" · "))
        },
        detail: binding.located().to_string(),
        title,
        column,
        unused: false,
        kept: None,
        queried: None,
    }
}

/// Bindings sharing a site share a node: the same visual, or the same page
/// or report outside visuals.
fn site_key(binding: &BindingEdge) -> String {
    let name = |key: &Option<ripbi_core::NameKey>| {
        key.as_ref()
            .map_or(String::new(), |key| key.folded().to_owned())
    };
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        name(&binding.report),
        name(&binding.page),
        name(&binding.visual),
        name(&binding.bookmark),
        binding.mobile
    )
}

fn paint_node(ui: &egui::Ui, node: &Node, rect: Rect, focused: bool, hovered: bool) {
    let visuals = ui.visuals();
    let painter = ui.painter();
    let color = tint(node.kind());
    let fill = if focused {
        color.gamma_multiply(0.25)
    } else if node.unused || matches!(node.what, What::More(_)) {
        visuals.faint_bg_color
    } else {
        visuals.extreme_bg_color
    };
    let stroke = if focused || hovered {
        Stroke::new(2.0, color)
    } else {
        Stroke::new(1.0, color.gamma_multiply(0.6))
    };
    painter.rect(rect, 5.0, fill, stroke, egui::StrokeKind::Inside);
    painter.rect_filled(
        Rect::from_min_size(rect.min, vec2(4.0, rect.height())),
        egui::CornerRadius {
            nw: 5,
            sw: 5,
            ..Default::default()
        },
        color,
    );

    let text = if node.unused {
        visuals.weak_text_color()
    } else {
        visuals.strong_text_color()
    };
    let width = rect.width() - 18.0;
    let mut subtitle = node.subtitle.clone();
    if node.unused {
        subtitle = format!("UNUSED · {subtitle}");
    } else if node.kept.is_some() {
        subtitle = format!("KEPT · {subtitle}");
    }
    let small = egui::TextStyle::Small.resolve(ui.style());
    let body = egui::TextStyle::Body.resolve(ui.style());
    let top = one_line(painter, subtitle, small, visuals.weak_text_color(), width);
    let name = one_line(painter, node.title.clone(), body, text, width);
    painter.galley(rect.min + vec2(11.0, 5.0), top, text);
    painter.galley(rect.min + vec2(11.0, 19.0), name, text);
}

fn one_line(
    painter: &egui::Painter,
    text: String,
    font: egui::FontId,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    painter.layout_job(job)
}

/// Node width and column gap for `columns` columns in `available` points:
/// full size when they fit, else narrower down to the minimums (then the
/// canvas scrolls).
fn fit(columns: f32, available: f32) -> (f32, f32) {
    let room = available - 2.0 * MARGIN;
    if columns * NODE_W + (columns - 1.0) * GAP_X <= room {
        return (NODE_W, GAP_X);
    }
    let width = (room - (columns - 1.0) * MIN_GAP_X) / columns;
    (width.clamp(MIN_NODE_W, NODE_W), MIN_GAP_X)
}

/// A curve from `from`'s right edge to `to`'s left edge, with an arrowhead.
fn connect(painter: &egui::Painter, from: Rect, to: Rect, stroke: Stroke) {
    let start = from.right_center();
    let end = to.left_center() - vec2(4.0, 0.0);
    let bend = ((end.x - start.x).abs() / 2.0).max(40.0);
    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
        [start, start + vec2(bend, 0.0), end - vec2(bend, 0.0), end],
        false,
        Color32::TRANSPARENT,
        stroke,
    ));
    let tip = to.left_center();
    let arrow: Vec<Pos2> = vec![tip, tip + vec2(-6.0, -3.5), tip + vec2(-6.0, 3.5)];
    painter.add(egui::Shape::convex_polygon(
        arrow,
        stroke.color,
        Stroke::NONE,
    ));
}

/// One hue per kind, legible on both themes.
pub(crate) fn tint(kind: &str) -> Color32 {
    match kind {
        "table" => Color32::from_rgb(0x4c, 0x8b, 0xf5),
        "column" => Color32::from_rgb(0x26, 0xa6, 0x9a),
        "measure" | "report_measure" => Color32::from_rgb(0xa3, 0x71, 0xf7),
        "hierarchy" | "calculation_item" => Color32::from_rgb(0xe0, 0x8a, 0x2e),
        "partition" | "expression" | "function" => Color32::from_rgb(0x5d, 0xa8, 0x3c),
        "visual" => Color32::from_rgb(0xe0, 0x5a, 0x8a),
        _ => Color32::from_rgb(0x8a, 0x8f, 0x98),
    }
}

fn legend(ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        for (kind, text) in [
            ("table", "Table"),
            ("column", "Column"),
            ("measure", "Measure"),
            ("hierarchy", "Hierarchy / calc item"),
            ("partition", "Power Query / function"),
            ("relationship", "Relationship / other"),
            ("visual", "Report visual"),
        ] {
            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 4.0, tint(kind));
            ui.label(RichText::new(text).small());
        }
        ui.label(
            RichText::new(
                "· Data flows left to right · Dimmed objects are unused · Hover for details",
            )
            .small()
            .weak(),
        );
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;

    pub(crate) fn sample() -> Explorer {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../samples")
            .canonicalize()
            .unwrap();
        let model = root.join("AdventureWorks Sales.SemanticModel");
        load(&crate::analysis::deps_request(&model, &[root]).unwrap()).unwrap()
    }

    /// A bound measure lays out with its inputs to the left and its visuals
    /// to the right; the edges at the focus flow rightward.
    #[test]
    fn lays_out_a_measure_between_its_inputs_and_its_visuals() {
        let explorer = sample();
        let measure = explorer
            .object_ids()
            .filter(|id| kind_of(id) == "measure")
            .map(|id| {
                let request = DepsRequest {
                    object: Some(id.to_string()),
                    ..DepsRequest::default()
                };
                let DepsOutput::Focused(focused) = explorer.view(&request).unwrap() else {
                    unreachable!()
                };
                Graph::build(&focused, &HashSet::new())
            })
            .find(|graph| {
                graph
                    .nodes
                    .iter()
                    .any(|node| matches!(node.what, What::Visual))
                    && graph.columns.keys().any(|&column| column < 0)
            })
            .expect("a measure with inputs and visuals");
        let root = measure.columns[&0][0];
        assert!(matches!(measure.nodes[root].what, What::Object(_)));
        for edge in &measure.edges {
            if edge.to == root {
                assert_eq!(measure.nodes[edge.from].column, -1);
            }
            if edge.from == root {
                assert_eq!(measure.nodes[edge.to].column, 1);
            }
        }
        for members in measure.columns.values() {
            assert!(members.len() <= CAP + 1);
        }
    }
}
