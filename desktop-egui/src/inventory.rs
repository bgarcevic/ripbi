//! Every object the analysis read, as a filterable tree with two ways in:
//!
//! - **Model → reports**: tables and their members, then the model's other
//!   objects, each drilling down to the report visuals it reaches — directly,
//!   or through the measures and columns that use it.
//! - **Reports → model**: reports, pages and visuals, each visual drilling
//!   down to the fields it binds and on through what those fields use.
//!
//! The Inventory tab browses it beside a detail panel; the Lineage tab uses it
//! to pick the object to focus.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use eframe::egui::{
    self, Align, Id, Label, RichText, ScrollArea, Sense, TextEdit,
    collapsing_header::CollapsingState, vec2,
};
use ripbi_app::deps::Explorer;
use ripbi_app::output::kind_of;
use ripbi_core::{BindingEdge, ObjectId};

use crate::lineage::{short_name, tint};

/// What a click on an object row asks for.
pub enum Pick {
    Select(ObjectId),
    /// A double-click: straight to its lineage.
    Open(ObjectId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Model,
    Reports,
}

/// A visual's place: report, page and visual indices.
type Site = (usize, usize, usize);

pub struct Inventory {
    tables: Vec<Table>,
    /// Objects outside tables, by group heading.
    others: Vec<(&'static str, Vec<Item>)>,
    reports: Vec<Report>,
    objects: usize,
    unused: usize,
    visuals: usize,
    /// What each object uses directly, to drill down from a visual's fields.
    uses: HashMap<ObjectId, Vec<Item>>,
    /// The visuals each object reaches, to drill down from a model object.
    reach: HashMap<ObjectId, Vec<Reach>>,
    mode: Mode,
    search: String,
    unused_only: bool,
    /// Open the model tree to this object and scroll it into view, once.
    reveal: Option<ObjectId>,
    /// Open the reports tree to this visual and scroll it into view, once.
    reveal_site: Option<Site>,
}

#[derive(Clone)]
struct Item {
    id: ObjectId,
    name: String,
    folded: String,
    kind: &'static str,
    unused: bool,
}

struct Table {
    name: String,
    folded: String,
    /// The table object itself; absent only if the graph has members without it.
    item: Option<Item>,
    members: Vec<Item>,
}

struct Report {
    name: String,
    pages: Vec<Page>,
}

struct Page {
    name: String,
    visuals: Vec<Visual>,
}

struct Visual {
    title: String,
    kind: String,
    folded: String,
    fields: Vec<Item>,
}

/// A visual an object reaches, and the bound field it reaches it through
/// when that is not the object itself.
struct Reach {
    site: Site,
    via: Option<ObjectId>,
}

impl Item {
    fn new(id: &ObjectId, unused: &HashSet<String>) -> Self {
        let name = short_name(id);
        Self {
            folded: name.to_lowercase(),
            kind: kind_of(id),
            unused: unused.contains(&id.to_string()),
            id: id.clone(),
            name,
        }
    }
}

/// Members sort by kind in this order, then by name.
fn rank(kind: &str) -> usize {
    [
        "table",
        "column",
        "measure",
        "hierarchy",
        "calculation_item",
        "partition",
    ]
    .iter()
    .position(|k| *k == kind)
    .unwrap_or(usize::MAX)
}

fn by_kind_then_name(a: &Item, b: &Item) -> std::cmp::Ordering {
    (rank(a.kind), &a.folded).cmp(&(rank(b.kind), &b.folded))
}

impl Inventory {
    /// Groups the graph's objects and bindings, and works out what each
    /// object reaches. Run it off the UI thread: it walks the whole graph.
    pub fn build(explorer: &Explorer, unused: &HashSet<String>) -> Self {
        let mut tables: BTreeMap<String, Table> = BTreeMap::new();
        let mut others: BTreeMap<usize, (&'static str, Vec<Item>)> = BTreeMap::new();
        let mut uses = HashMap::new();
        let mut objects = 0;
        let mut unused_count = 0;
        for id in explorer.object_ids() {
            let mut used: Vec<Item> = explorer
                .uses(id)
                .iter()
                .map(|(used, _)| Item::new(used, unused))
                .collect();
            used.sort_by(by_kind_then_name);
            used.dedup_by(|a, b| a.id == b.id);
            uses.insert(id.clone(), used);

            let item = Item::new(id, unused);
            objects += 1;
            unused_count += usize::from(item.unused);
            let group = match id {
                ObjectId::Relationship { .. } => Some((0, "Relationships")),
                ObjectId::Role { .. } => Some((1, "Roles")),
                ObjectId::Expression { .. } => Some((2, "Power Query expressions")),
                ObjectId::Function { .. } => Some((3, "Functions")),
                ObjectId::ReportMeasure { .. } => Some((4, "Report measures")),
                ObjectId::Bookmark { .. } => Some((5, "Bookmarks")),
                _ => None,
            };
            match (group, id.owning_table()) {
                (None, Some(table)) => {
                    let entry = tables
                        .entry(table.folded().to_owned())
                        .or_insert_with(|| Table {
                            name: table.as_str().to_owned(),
                            folded: table.folded().to_lowercase(),
                            item: None,
                            members: Vec::new(),
                        });
                    if matches!(id, ObjectId::Table { .. }) {
                        entry.item = Some(item);
                    } else {
                        entry.members.push(item);
                    }
                }
                (group, _) => {
                    let (order, heading) = group.unwrap_or((6, "Other"));
                    others
                        .entry(order)
                        .or_insert((heading, Vec::new()))
                        .1
                        .push(item);
                }
            }
        }
        let mut tables: Vec<Table> = tables.into_values().collect();
        for table in &mut tables {
            table.members.sort_by(by_kind_then_name);
        }
        let mut others: Vec<_> = others.into_values().collect();
        for (_, items) in &mut others {
            items.sort_by(|a, b| a.folded.cmp(&b.folded));
        }
        let (reports, visuals) = reports(explorer, unused);
        let reach = reach(&reports, &uses);
        Self {
            tables,
            others,
            reports,
            objects,
            unused: unused_count,
            visuals,
            uses,
            reach,
            mode: Mode::Model,
            search: String::new(),
            unused_only: false,
            reveal: None,
            reveal_site: None,
        }
    }

    /// Opens the model tree to `id` and scrolls it into view.
    pub fn reveal(&mut self, id: &ObjectId) {
        self.reveal = Some(id.clone());
    }

    /// Draws the mode switch, the filter row and the tree; returns the
    /// object clicked.
    pub fn ui(&mut self, ui: &mut egui::Ui, selected: Option<&ObjectId>) -> Option<Pick> {
        ui.horizontal(|ui| {
            for (mode, text, hover) in [
                (
                    Mode::Model,
                    "Model → reports",
                    "Tables and objects, each down to the visuals it reaches",
                ),
                (
                    Mode::Reports,
                    "Reports → model",
                    "Pages and visuals, each down to the fields it binds and what they use",
                ),
            ] {
                if ui
                    .selectable_value(&mut self.mode, mode, text)
                    .on_hover_text(hover)
                    .clicked()
                    && mode == Mode::Model
                    && let Some(id) = selected
                {
                    self.reveal = Some(id.clone());
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(
                TextEdit::singleline(&mut self.search)
                    .hint_text(match self.mode {
                        Mode::Model => "Filter objects…",
                        Mode::Reports => "Filter visuals and fields…",
                    })
                    .desired_width(200.0),
            );
            ui.checkbox(&mut self.unused_only, "Unused only");
        });
        ui.label(
            RichText::new(format!(
                "{} objects · {} unused · {} visuals",
                self.objects, self.unused, self.visuals
            ))
            .small()
            .weak(),
        );
        ui.separator();

        let needle = self.search.trim().to_lowercase();
        let mut view = View {
            filter: Filter {
                needle: &needle,
                unused_only: self.unused_only,
            },
            selected,
            reveal: self.reveal.take(),
            reveal_site: self.reveal_site.take(),
            pick: None,
            jump: None,
        };
        let mode = self.mode;
        ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("inventory-tree")
            .show(ui, |ui| match mode {
                Mode::Model => self.model_ui(ui, &mut view),
                Mode::Reports => self.reports_ui(ui, &mut view),
            });
        if let Some(site) = view.jump {
            self.mode = Mode::Reports;
            self.reveal_site = Some(site);
            ui.ctx().request_repaint();
        }
        view.pick
    }

    fn model_ui(&self, ui: &mut egui::Ui, view: &mut View) {
        let mut shown = false;
        for table in &self.tables {
            let named = view.filter.names(&table.folded);
            let items: Vec<&Item> = table
                .item
                .iter()
                .chain(&table.members)
                .filter(|item| view.filter.keeps(item, named))
                .collect();
            if items.is_empty() {
                continue;
            }
            shown = true;
            let unused = table.members.iter().filter(|item| item.unused).count();
            let mut text = format!("{}   {}", table.name, table.members.len());
            if unused > 0 {
                text.push_str(&format!(" · {unused} unused"));
            }
            let holds = view.holds(&items);
            group(
                ui,
                view.id(("table", &table.folded)),
                view.filter.active(),
                holds,
                RichText::new(text),
                |ui| {
                    for item in items {
                        self.object_ui(ui, item, view);
                    }
                },
            );
        }
        for (heading, items) in &self.others {
            let items: Vec<&Item> = items
                .iter()
                .filter(|item| view.filter.keeps(item, false))
                .collect();
            if items.is_empty() {
                continue;
            }
            shown = true;
            let holds = view.holds(&items);
            group(
                ui,
                view.id(("group", heading)),
                view.filter.active(),
                holds,
                RichText::new(format!("{heading}   {}", items.len())),
                |ui| {
                    for item in items {
                        self.object_ui(ui, item, view);
                    }
                },
            );
        }
        if !shown {
            ui.label(RichText::new("No matching model objects").weak());
        }
    }

    /// A model object, drilling down to the visuals it reaches.
    fn object_ui(&self, ui: &mut egui::Ui, item: &Item, view: &mut View) {
        let reach = self.reach.get(&item.id).map_or(&[][..], Vec::as_slice);
        if reach.is_empty() {
            leaf(ui, |ui| row(ui, item, Some("no visuals"), view));
            return;
        }
        let badge = match reach.len() {
            1 => "1 visual".to_owned(),
            n => format!("{n} visuals"),
        };
        let id = view.id(("reach", &item.id));
        CollapsingState::load_with_default_open(ui.ctx(), id, false)
            .show_header(ui, |ui| row(ui, item, Some(&badge), view))
            .body(|ui| {
                for reach in reach {
                    let (r, p, v) = reach.site;
                    let report = &self.reports[r];
                    let page = &report.pages[p];
                    let visual = &page.visuals[v];
                    ui.horizontal_wrapped(|ui| {
                        let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter()
                            .circle_filled(dot.center(), 3.5, tint("visual"));
                        if ui
                            .add(egui::Button::selectable(false, &visual.title).wrap())
                            .on_hover_text("Open this visual under Reports → model")
                            .clicked()
                        {
                            view.jump = Some(reach.site);
                        }
                        let mut place = page.name.clone();
                        if self.reports.len() > 1 {
                            place = format!("{place} · {}", report.name);
                        }
                        ui.label(RichText::new(place).small().weak());
                        if let Some(via) = &reach.via {
                            ui.label(
                                RichText::new(format!("via {}", short_name(via)))
                                    .small()
                                    .color(tint(kind_of(via))),
                            );
                        }
                    });
                }
            });
    }

    fn reports_ui(&self, ui: &mut egui::Ui, view: &mut View) {
        if self.reports.is_empty() {
            ui.label(RichText::new("No report bindings were read").weak());
            return;
        }
        let mut shown = false;
        for (r, report) in self.reports.iter().enumerate() {
            let pages: Vec<ShownPage> = report
                .pages
                .iter()
                .enumerate()
                .map(|(p, page)| {
                    let visuals = page
                        .visuals
                        .iter()
                        .enumerate()
                        .filter_map(|(v, visual)| {
                            let named = view.filter.names(&visual.folded);
                            let fields: Vec<&Item> = visual
                                .fields
                                .iter()
                                .filter(|item| view.filter.keeps(item, named))
                                .collect();
                            (!fields.is_empty()).then_some((v, visual, fields))
                        })
                        .collect::<Vec<_>>();
                    (p, page, visuals)
                })
                .filter(|(.., visuals)| !visuals.is_empty())
                .collect();
            if pages.is_empty() {
                continue;
            }
            shown = true;
            let count: usize = report.pages.iter().map(|page| page.visuals.len()).sum();
            let target = view.reveal_site;
            group(
                ui,
                view.id(("report", r)),
                view.filter.active(),
                target.is_some_and(|(tr, ..)| tr == r),
                RichText::new(format!("{}   {count} visuals", report.name)),
                |ui| {
                    for (p, page, visuals) in pages {
                        group(
                            ui,
                            view.id(("page", r, p)),
                            view.filter.active(),
                            target.is_some_and(|(tr, tp, _)| (tr, tp) == (r, p)),
                            RichText::new(format!("{}   {}", page.name, page.visuals.len())),
                            |ui| {
                                for (v, visual, fields) in visuals {
                                    self.visual_ui(ui, (r, p, v), visual, fields, view);
                                }
                            },
                        );
                    }
                },
            );
        }
        if !shown {
            ui.label(RichText::new("No matching visuals").weak());
        }
    }

    fn visual_ui(
        &self,
        ui: &mut egui::Ui,
        site: Site,
        visual: &Visual,
        fields: Vec<&Item>,
        view: &mut View,
    ) {
        let target = view.reveal_site == Some(site);
        let mut title = RichText::new(&visual.title).color(tint("visual"));
        if target {
            title = title.strong();
        }
        let id = view.id(("visual", site));
        let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, view.filter.active());
        if target {
            state.set_open(true);
        }
        let mut clicked = false;
        let mut header = state.show_header(ui, |ui| {
            let label = ui.add(Label::new(title).wrap().sense(Sense::click()));
            if !visual.kind.is_empty() && visual.kind != visual.title {
                ui.label(RichText::new(&visual.kind).small().weak());
            }
            if target {
                label.scroll_to_me(Some(Align::Center));
            }
            clicked = label.clicked();
        });
        if clicked {
            header.toggle();
        }
        header.body(|ui| {
            for item in fields {
                self.uses_ui(ui, item, id, &mut Vec::new(), view);
            }
        });
    }

    /// A bound field, or something it uses, drilling down through what it
    /// uses. `path` holds the ancestors, so a cycle ends in a leaf.
    fn uses_ui<'a>(
        &'a self,
        ui: &mut egui::Ui,
        item: &'a Item,
        parent: Id,
        path: &mut Vec<&'a ObjectId>,
        view: &mut View,
    ) {
        let uses = self.uses.get(&item.id).map_or(&[][..], Vec::as_slice);
        if uses.is_empty() || path.contains(&&item.id) {
            leaf(ui, |ui| row(ui, item, None, view));
            return;
        }
        let id = parent.with(&item.id);
        let badge = format!("uses {}", uses.len());
        CollapsingState::load_with_default_open(ui.ctx(), id, false)
            .show_header(ui, |ui| row(ui, item, Some(&badge), view))
            .body(|ui| {
                path.push(&item.id);
                for used in uses {
                    self.uses_ui(ui, used, id, path, view);
                }
                path.pop();
            });
    }
}

/// One frame's drawing state.
struct View<'a> {
    filter: Filter<'a>,
    selected: Option<&'a ObjectId>,
    reveal: Option<ObjectId>,
    reveal_site: Option<Site>,
    pick: Option<Pick>,
    /// A visual clicked in the model tree, to open under Reports.
    jump: Option<Site>,
}

impl View<'_> {
    /// Tree state lives under its own ids while filtering, so clearing the
    /// filter returns the tree to how the user left it.
    fn id(&self, salt: impl std::hash::Hash + std::fmt::Debug) -> Id {
        Id::unique(("inventory", salt, self.filter.active()))
    }

    /// Whether `items` hold the object to reveal, so their group opens.
    fn holds(&self, items: &[&Item]) -> bool {
        self.reveal
            .as_ref()
            .is_some_and(|id| items.iter().any(|item| item.id == *id))
    }
}

/// A page with the visuals, and their fields, that pass the filter.
type ShownPage<'a> = (usize, &'a Page, Vec<(usize, &'a Visual, Vec<&'a Item>)>);

/// The tree's filter for one frame.
struct Filter<'a> {
    needle: &'a str,
    unused_only: bool,
}

impl Filter<'_> {
    fn active(&self) -> bool {
        !self.needle.is_empty() || self.unused_only
    }

    /// Whether a group's own name matches, keeping all of its members.
    fn names(&self, folded: &str) -> bool {
        !self.needle.is_empty() && folded.contains(self.needle)
    }

    fn keeps(&self, item: &Item, named: bool) -> bool {
        (!self.unused_only || item.unused)
            && (self.needle.is_empty() || named || item.folded.contains(self.needle))
    }
}

/// A collapsible group whose wrapped heading also toggles it; `open` forces
/// it open this frame.
fn group(
    ui: &mut egui::Ui,
    id: Id,
    default_open: bool,
    open: bool,
    text: RichText,
    body: impl FnOnce(&mut egui::Ui),
) {
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
    if open {
        state.set_open(true);
    }
    let mut clicked = false;
    let mut header = state.show_header(ui, |ui| {
        clicked = ui
            .add(Label::new(text).wrap().sense(Sense::click()))
            .clicked();
    });
    if clicked {
        header.toggle();
    }
    header.body(body);
}

/// A row with no children, indented to line up with collapsible siblings.
fn leaf(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(ui.spacing().icon_width);
        add(ui);
    });
}

fn row(ui: &mut egui::Ui, item: &Item, badge: Option<&str>, view: &mut View) {
    let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    ui.painter()
        .circle_filled(dot.center(), 3.5, tint(item.kind));
    let text = if item.unused {
        RichText::new(&item.name).color(ui.visuals().weak_text_color())
    } else {
        RichText::new(&item.name)
    };
    let response = ui
        .add(egui::Button::selectable(view.selected == Some(&item.id), text).wrap())
        .on_hover_text(format!("{} · {}", item.kind.replace('_', " "), item.id));
    if item.unused {
        ui.label(
            RichText::new("unused")
                .small()
                .color(ui.visuals().warn_fg_color),
        );
    } else if let Some(badge) = badge {
        ui.label(RichText::new(badge).small().weak());
    }
    if view.reveal.as_ref() == Some(&item.id) {
        response.scroll_to_me(Some(Align::Center));
    }
    if response.double_clicked() {
        view.pick = Some(Pick::Open(item.id.clone()));
    } else if response.clicked() {
        view.pick = Some(Pick::Select(item.id.clone()));
    }
}

/// The reports' pages and visuals, in report order, from the live bindings
/// (bookmark state is left out); the second value counts the visuals.
fn reports(explorer: &Explorer, unused: &HashSet<String>) -> (Vec<Report>, usize) {
    let key = |name: &Option<ripbi_core::NameKey>| {
        name.as_ref()
            .map_or(String::new(), |name| name.folded().to_owned())
    };
    let mut reports: Vec<Report> = Vec::new();
    let mut report_at: HashMap<String, usize> = HashMap::new();
    let mut page_at: HashMap<(usize, String), usize> = HashMap::new();
    let mut visual_at: HashMap<(usize, usize, String), usize> = HashMap::new();
    let mut listed: HashSet<(usize, usize, usize, &ObjectId)> = HashSet::new();
    let mut visuals = 0;
    for (id, binding) in explorer.bindings() {
        if binding.bookmark.is_some() {
            continue;
        }
        let r = *report_at.entry(key(&binding.report)).or_insert_with(|| {
            reports.push(Report {
                name: report_name(binding),
                pages: Vec::new(),
            });
            reports.len() - 1
        });
        let pages = &mut reports[r].pages;
        let p = *page_at.entry((r, key(&binding.page))).or_insert_with(|| {
            pages.push(Page {
                name: page_name(binding),
                visuals: Vec::new(),
            });
            pages.len() - 1
        });
        let page = &mut pages[p];
        let v = *visual_at
            .entry((r, p, key(&binding.visual)))
            .or_insert_with(|| {
                let (title, kind) = visual_label(binding);
                visuals += usize::from(binding.visual.is_some());
                page.visuals.push(Visual {
                    folded: title.to_lowercase(),
                    title,
                    kind,
                    fields: Vec::new(),
                });
                page.visuals.len() - 1
            });
        if listed.insert((r, p, v, id)) {
            page.visuals[v].fields.push(Item::new(id, unused));
        }
    }
    for visual in reports
        .iter_mut()
        .flat_map(|report| &mut report.pages)
        .flat_map(|page| &mut page.visuals)
    {
        visual.fields.sort_by(by_kind_then_name);
    }
    (reports, visuals)
}

/// For every object, the visuals whose bound fields use it, transitively:
/// a column reaches the visuals of every measure built on it.
fn reach(reports: &[Report], uses: &HashMap<ObjectId, Vec<Item>>) -> HashMap<ObjectId, Vec<Reach>> {
    // What each bound field uses, walked once per field.
    let mut upstream: HashMap<&ObjectId, Vec<&ObjectId>> = HashMap::new();
    let mut reach: HashMap<ObjectId, Vec<Reach>> = HashMap::new();
    let mut seen: HashSet<(&ObjectId, Site)> = HashSet::new();
    for (r, report) in reports.iter().enumerate() {
        for (p, page) in report.pages.iter().enumerate() {
            for (v, visual) in page.visuals.iter().enumerate() {
                let site = (r, p, v);
                // Direct bindings first, so an object bound here is never
                // listed as reaching it only through another field.
                for field in &visual.fields {
                    if seen.insert((&field.id, site)) {
                        reach
                            .entry(field.id.clone())
                            .or_default()
                            .push(Reach { site, via: None });
                    }
                }
                for field in &visual.fields {
                    let walked = upstream
                        .entry(&field.id)
                        .or_insert_with(|| walk(&field.id, uses));
                    for &object in walked.iter() {
                        if seen.insert((object, site)) {
                            reach.entry(object.clone()).or_default().push(Reach {
                                site,
                                via: Some(field.id.clone()),
                            });
                        }
                    }
                }
            }
        }
    }
    reach
}

/// Everything `root` uses, transitively, without `root` itself.
fn walk<'a>(root: &'a ObjectId, uses: &'a HashMap<ObjectId, Vec<Item>>) -> Vec<&'a ObjectId> {
    let mut seen: HashSet<&ObjectId> = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    let mut out = Vec::new();
    while let Some(id) = queue.pop_front() {
        for used in uses.get(id).into_iter().flatten() {
            if seen.insert(&used.id) {
                out.push(&used.id);
                queue.push_back(&used.id);
            }
        }
    }
    out
}

fn report_name(binding: &BindingEdge) -> String {
    binding
        .report
        .as_ref()
        .map_or("Report".to_owned(), |report| report.as_str().to_owned())
}

fn page_name(binding: &BindingEdge) -> String {
    binding
        .labels
        .page
        .clone()
        .or_else(|| binding.page.as_ref().map(|page| page.as_str().to_owned()))
        .unwrap_or_else(|| "Report-level filters".to_owned())
}

/// The visual's title and type, or a filter group outside visuals.
fn visual_label(binding: &BindingEdge) -> (String, String) {
    let labels = &binding.labels;
    match &binding.visual {
        Some(visual) => (
            labels
                .visual_title
                .clone()
                .or_else(|| labels.visual_type.clone())
                .unwrap_or_else(|| visual.as_str().to_owned()),
            labels.visual_type.clone().unwrap_or_default(),
        ),
        None if binding.page.is_some() => ("Page filters".to_owned(), String::new()),
        None => ("Report filters".to_owned(), String::new()),
    }
}

/// Where a binding lives, in the words Power BI Desktop shows: the kind of
/// use, the visual's title or type, its page, and its report.
pub(crate) fn site_text(binding: &BindingEdge) -> String {
    let (title, _) = visual_label(binding);
    let mut text = format!("{} — {title}", binding.kind);
    if binding.visual.is_some() || binding.page.is_some() {
        text.push_str(&format!(" on {}", page_name(binding)));
    }
    if let Some(report) = &binding.report {
        text.push_str(&format!(" ({})", report.as_str()));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample's inventory holds every graph object once, with its
    /// reports' visuals and the fields they bind; what a bound field uses
    /// reaches that field's visual through it.
    #[test]
    fn lists_every_object_and_what_reaches_each_visual() {
        let explorer = crate::lineage::tests::sample();
        let inventory = Inventory::build(&explorer, &HashSet::new());
        let listed: usize = inventory
            .tables
            .iter()
            .map(|table| table.members.len() + usize::from(table.item.is_some()))
            .sum::<usize>()
            + inventory
                .others
                .iter()
                .map(|(_, items)| items.len())
                .sum::<usize>();
        assert_eq!(listed, explorer.object_ids().count());
        assert_eq!(listed, inventory.objects);
        assert!(inventory.visuals > 0);

        let (field, site) = inventory
            .reports
            .iter()
            .enumerate()
            .flat_map(|(r, report)| {
                report.pages.iter().enumerate().flat_map(move |(p, page)| {
                    page.visuals
                        .iter()
                        .enumerate()
                        .flat_map(move |(v, visual)| {
                            visual.fields.iter().map(move |field| (field, (r, p, v)))
                        })
                })
            })
            .find(|(field, _)| !inventory.uses[&field.id].is_empty())
            .expect("a bound field that uses something");
        let direct = &inventory.reach[&field.id];
        assert!(
            direct
                .iter()
                .any(|reach| reach.site == site && reach.via.is_none())
        );
        let used = &inventory.uses[&field.id][0].id;
        assert!(inventory.reach[used].iter().any(|reach| reach.site == site));
    }
}
