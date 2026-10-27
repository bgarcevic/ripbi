#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analysis;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use analysis::Analysis;
use eframe::egui::{
    self, Align, Color32, ComboBox, Layout, ProgressBar, RichText, ScrollArea, Sense, TextEdit,
};
use egui_extras::{Column, TableBuilder};
use ripbi_app::output::Finding;

const GREEN: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
const AMBER: Color32 = Color32::from_rgb(0xd2, 0x99, 0x22);
const RED: Color32 = Color32::from_rgb(0xe5, 0x53, 0x4b);

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ripbi")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("ripbi", options, Box::new(|_| Ok(Box::<App>::default())))
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Unused,
    Broken,
    Dates,
    Coverage,
}

#[derive(Default)]
struct App {
    model: Option<PathBuf>,
    reports: Vec<PathBuf>,
    job: Option<Receiver<Result<Analysis, String>>>,
    analysis: Option<Analysis>,
    error: Option<String>,
    notice: Option<String>,
    tab: Tab,
    query: String,
    kind: Option<String>,
    /// Index into `analysis.output.findings`.
    selected: Option<usize>,
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let Some(job) = &self.job else { return };
        match job.try_recv() {
            Ok(Ok(analysis)) => {
                self.analysis = Some(analysis);
                self.tab = Tab::Unused;
                self.query.clear();
                self.kind = None;
                self.selected = None;
                self.job = None;
            }
            Ok(Err(error)) => {
                self.error = Some(error);
                self.job = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.error = Some("Analysis worker stopped unexpectedly.".into());
                self.job = None;
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.accept_dropped_files(ui.ctx());

        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("ripbi").strong());
                ui.label(RichText::new("DESKTOP").small().weak());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new("✔ Read-only analysis · Your model stays on your machine")
                            .weak(),
                    );
                });
            });
            ui.add_space(6.0);
        });

        egui::Panel::left("inputs")
            .resizable(true)
            .default_size(320.0)
            .min_size(260.0)
            .show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| self.inputs(ui));
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(error) = &self.error {
                banner(ui, RED, error);
            }
            if let Some(notice) = &self.notice {
                banner(ui, GREEN, notice);
            }
            if self.analysis.is_some() {
                self.results(ui);
            } else {
                welcome(ui, self.job.is_some());
            }
        });
    }
}

impl App {
    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn inputs(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.label(RichText::new("01  SEMANTIC MODEL").small().strong());
        ui.add_space(4.0);
        match &self.model {
            Some(model) => path_label(ui, model),
            None => {
                ui.label(
                    RichText::new("Select a .SemanticModel / TMDL folder or a .pbip project.")
                        .weak(),
                );
            }
        }
        ui.add_enabled_ui(!self.busy(), |ui| {
            ui.horizontal(|ui| {
                if ui.button("📁 Model folder…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title("Select a semantic model")
                        .pick_folder()
                {
                    self.set_model(path);
                }
                if ui.button("Choose .pbip…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title("Select a semantic model")
                        .add_filter("Power BI project", &["pbip"])
                        .pick_file()
                {
                    self.set_model(path);
                }
            });
        });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(RichText::new("02  REPORTS").small().strong());
        ui.add_space(4.0);
        ui.add_enabled_ui(!self.busy() && self.model.is_some(), |ui| {
            ui.horizontal(|ui| {
                if ui.button("+ .pbip projects…").clicked()
                    && let Some(paths) = rfd::FileDialog::new()
                        .set_title("Select reports")
                        .add_filter("Power BI project", &["pbip"])
                        .pick_files()
                {
                    self.add_reports(paths);
                }
                if ui.button("📁 Report / search folder…").clicked()
                    && let Some(paths) = rfd::FileDialog::new()
                        .set_title("Select report or search folders")
                        .pick_folders()
                {
                    self.add_reports(paths);
                }
            });
            ui.add_space(4.0);
            let mut remove = None;
            for (i, path) in self.reports.iter().enumerate() {
                ui.horizontal_top(|ui| {
                    let remove_button = egui::Button::new(RichText::new("×").size(16.0))
                        .frame(false)
                        .min_size(egui::vec2(18.0, 18.0));
                    if ui.add(remove_button).on_hover_text("Remove").clicked() {
                        remove = Some(i);
                    }
                    path_label(ui, path);
                });
            }
            if let Some(i) = remove {
                self.reports.remove(i);
            }
            if self.reports.is_empty() {
                ui.label(
                    RichText::new(
                        "Optional. Reports beside the model are found automatically. \
                         For reports elsewhere, add their .pbip projects or a folder to \
                         search for reports bound to this model.",
                    )
                    .weak(),
                );
            }
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new("Tip: drop folders or .pbip files onto the window.")
                .small()
                .weak(),
        );

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        let ready = self.model.is_some() && !self.busy();
        ui.horizontal(|ui| {
            let button = egui::Button::new(RichText::new("Analyze model  →").strong())
                .min_size(egui::vec2(140.0, 30.0));
            if ui.add_enabled(ready, button).clicked() {
                self.start(ui.ctx());
            }
            if self.busy() {
                ui.spinner();
                ui.label("Analyzing…");
            }
        });
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "Only reports bound to this model are included. Objects used by \
                 reports outside the searched folders may appear unused.",
            )
            .small()
            .weak(),
        );
    }

    fn set_model(&mut self, path: PathBuf) {
        self.model = Some(path);
        self.reports.clear();
        self.analysis = None;
        self.error = None;
        self.notice = None;
    }

    fn add_reports(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            if !self.reports.contains(&path) {
                self.reports.push(path);
            }
        }
    }

    /// The first drop with no model picks the model; later drops add reports.
    fn accept_dropped_files(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        let mut paths = dropped.into_iter();
        if self.model.is_none()
            && let Some(first) = paths.next()
        {
            self.set_model(first);
        }
        self.add_reports(paths.collect());
    }

    fn start(&mut self, ctx: &egui::Context) {
        let (Some(model), reports) = (self.model.clone(), self.reports.clone()) else {
            return;
        };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            _ = tx.send(analysis::run(&model, &reports));
            ctx.request_repaint();
        });
        self.job = Some(rx);
        self.error = None;
        self.notice = None;
    }

    fn export(&mut self) {
        let Some(analysis) = &self.analysis else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export ripbi analysis")
            .set_file_name("ripbi-analysis.json")
            .add_filter("JSON", &["json"])
            .save_file()
        else {
            return;
        };
        let written = analysis
            .export()
            .and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string()));
        match written {
            Ok(()) => self.notice = Some(format!("Exported to {}", path.display())),
            Err(error) => self.error = Some(format!("Could not save analysis: {error}")),
        }
    }

    fn results(&mut self, ui: &mut egui::Ui) {
        let Some(analysis) = &self.analysis else {
            return;
        };
        let r = &analysis.output;
        let s = r;
        let reachable = if s.objects > 0 {
            s.reachable as f32 / s.objects as f32
        } else {
            0.0
        };
        let mut export = false;

        ui.horizontal(|ui| {
            ui.label(RichText::new("●").color(GREEN));
            ui.label(RichText::new("Analysis complete").strong());
            ui.label(RichText::new(file_name(Path::new(&r.target))).weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                export = ui.button("Export JSON ↓").clicked();
            });
        });
        ui.add_space(8.0);

        ui.columns(4, |cols| {
            metric(
                &mut cols[0],
                "Model objects",
                s.objects,
                &format!("{} report binding roots", s.roots),
                None,
            );
            metric(
                &mut cols[1],
                "Reachable",
                s.reachable,
                &format!("{:.0}% of the dependency graph", reachable * 100.0),
                Some(GREEN),
            );
            metric(
                &mut cols[2],
                "Unused",
                s.unused_raw,
                "Includes hidden date machinery",
                Some(AMBER),
            );
            metric(
                &mut cols[3],
                "Connected reports",
                r.reports.len(),
                &format!(
                    "{} broken references",
                    s.broken.len() + s.broken_artifacts.len()
                ),
                None,
            );
        });
        ui.add_space(8.0);

        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &r.findings {
            *counts.entry(f.kind).or_default() += 1;
        }
        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.label(RichText::new("Usage at a glance").strong());
                ui.add_space(4.0);
                ui.add(
                    ProgressBar::new(reachable)
                        .fill(GREEN)
                        .text(format!("{:.0}% reachable", reachable * 100.0)),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Reachability includes indirect dependencies and engine-required objects. \
                         It does not measure memory or file size.",
                    )
                    .small()
                    .weak(),
                );
            });
            card(&mut cols[1], |ui| {
                ui.label(RichText::new("Cleanup candidates by type").strong());
                ui.add_space(4.0);
                let max = counts.values().copied().max().unwrap_or(1).max(1);
                let mut by_count: Vec<_> = counts.iter().collect();
                by_count.sort_by(|a, b| b.1.cmp(a.1));
                for (kind, count) in by_count.into_iter().take(5) {
                    ui.add(
                        ProgressBar::new(*count as f32 / max as f32)
                            .fill(AMBER)
                            .text(format!("{}  ·  {count}", label(kind))),
                    );
                }
                if counts.is_empty() {
                    ui.label("No unused object findings in this selection.");
                }
                ui.label(
                    RichText::new(format!("{} ignored by project configuration.", s.ignored))
                        .small()
                        .weak(),
                );
            });
        });
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            for (tab, text) in [
                (
                    Tab::Unused,
                    format!("Unused objects ({})", r.findings.len()),
                ),
                (
                    Tab::Broken,
                    format!(
                        "Broken references ({})",
                        s.broken.len() + s.broken_artifacts.len()
                    ),
                ),
                (
                    Tab::Dates,
                    format!("Date tables ({})", r.auto_date_time.len()),
                ),
                (
                    Tab::Coverage,
                    format!("Coverage & notices ({})", r.skips.len()),
                ),
            ] {
                ui.selectable_value(&mut self.tab, tab, text);
            }
        });
        ui.separator();

        let kinds: Vec<&str> = counts.keys().copied().collect();
        match self.tab {
            Tab::Unused => unused_tab(
                ui,
                &r.findings,
                &kinds,
                &mut self.query,
                &mut self.kind,
                &mut self.selected,
            ),
            Tab::Broken => broken_tab(ui, analysis),
            Tab::Dates => dates_tab(ui, analysis),
            Tab::Coverage => coverage_tab(ui, analysis),
        }

        if export {
            self.export();
        }
    }
}

fn unused_tab(
    ui: &mut egui::Ui,
    unused: &[Finding],
    kinds: &[&str],
    query: &mut String,
    kind: &mut Option<String>,
    selected: &mut Option<usize>,
) {
    ui.horizontal(|ui| {
        ui.add(
            TextEdit::singleline(query)
                .hint_text("Search objects or tables…")
                .desired_width(280.0),
        );
        ComboBox::from_id_salt("kind")
            .selected_text(kind.as_deref().map_or("All object types".into(), label))
            .show_ui(ui, |ui| {
                ui.selectable_value(kind, None, "All object types");
                for k in kinds {
                    ui.selectable_value(kind, Some((*k).to_owned()), label(k));
                }
            });
    });
    let needle = query.to_lowercase();
    let rows: Vec<usize> = unused
        .iter()
        .enumerate()
        .filter(|(_, f)| kind.as_deref().is_none_or(|k| k == f.kind))
        .filter(|(_, f)| {
            needle.is_empty()
                || f.id.to_lowercase().contains(&needle)
                || f.table
                    .as_ref()
                    .is_some_and(|t| t.as_str().to_lowercase().contains(&needle))
        })
        .map(|(i, _)| i)
        .collect();
    ui.label(
        RichText::new(format!(
            "{} matching objects · select one to inspect dependencies",
            rows.len()
        ))
        .weak(),
    );

    egui::Panel::right("finding-detail")
        .resizable(true)
        .default_size(360.0)
        .min_size(240.0)
        .show(ui, |ui| {
            ScrollArea::vertical().show(ui, |ui| match selected.and_then(|i| unused.get(i)) {
                Some(f) => finding_detail(ui, f),
                None => {
                    ui.label(RichText::new("Select an object to see what references it.").weak());
                }
            });
        });

    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    TableBuilder::new(ui)
        .striped(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::auto().at_least(110.0))
        .column(Column::remainder().at_least(160.0).clip(true))
        .column(Column::auto().at_least(120.0).clip(true))
        .column(Column::auto().at_least(50.0))
        .header(row_height, |mut header| {
            for title in ["Type", "Object", "Table", "Refs"] {
                header.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|body| {
            body.rows(row_height, rows.len(), |mut row| {
                let index = rows[row.index()];
                let f = &unused[index];
                row.set_selected(*selected == Some(index));
                row.col(|ui| {
                    ui.label(RichText::new(label(f.kind)).small().color(AMBER));
                });
                row.col(|ui| {
                    ui.label(&f.id);
                });
                row.col(|ui| {
                    ui.label(f.table.as_ref().map_or("—", |t| t.as_str()));
                });
                row.col(|ui| {
                    ui.label(f.used_by.len().to_string());
                });
                if row.response().clicked() {
                    *selected = Some(index);
                }
            });
        });
}

fn finding_detail(ui: &mut egui::Ui, f: &Finding) {
    ui.label(RichText::new(label(f.kind)).small().color(AMBER));
    ui.label(RichText::new(&f.id).strong().size(16.0));
    if let Some(table) = &f.table {
        ui.label(RichText::new(format!("in {table}")).weak());
    }
    ui.add_space(8.0);
    ui.label(RichText::new("Referenced by").strong());
    if f.used_by.is_empty() {
        ui.label(RichText::new("No other model objects reference this object.").weak());
    }
    for u in &f.used_by {
        ui.horizontal_wrapped(|ui| {
            ui.monospace(&u.id);
            ui.label(RichText::new(&u.provenance).weak());
            if u.also_unused {
                ui.label(RichText::new("ALSO UNUSED").small().color(AMBER));
            }
        });
    }
    if !f.named_in_power_query.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("Named in Power Query").strong());
        ui.label(
            RichText::new(
                "These expressions supply the column; they do not establish report usage.",
            )
            .small()
            .weak(),
        );
        for p in &f.named_in_power_query {
            ui.monospace(p);
        }
    }
}

fn broken_tab(ui: &mut egui::Ui, analysis: &Analysis) {
    let r = &analysis.output;
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        if r.broken.is_empty() && r.broken_artifacts.is_empty() {
            ui.label(
                "No broken references reported. Check coverage notices for parser limitations.",
            );
        }
        for b in &r.broken {
            card(ui, |ui| {
                ui.label(RichText::new(&b.target).strong().color(RED));
                ui.label(
                    RichText::new(format!(
                        "{} · {} · {}",
                        label(b.reason),
                        b.provenance,
                        b.location
                    ))
                    .weak(),
                );
            });
        }
        for b in &r.broken_artifacts {
            card(ui, |ui| {
                ui.label(RichText::new(&b.id).strong().color(RED));
                ui.label(
                    RichText::new(format!(
                        "{} · unresolved: {}",
                        label(b.kind),
                        b.unresolved_references.join(", ")
                    ))
                    .weak(),
                );
            });
        }
    });
}

fn dates_tab(ui: &mut egui::Ui, analysis: &Analysis) {
    let r = &analysis.output;
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        if r.auto_date_time.is_empty() {
            ui.label("No auto date/time tables found.");
        }
        for d in &r.auto_date_time {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(label(d.verdict)).small().color(AMBER));
                    ui.label(RichText::new(&d.id).strong());
                });
                let mut detail = d
                    .source_column
                    .clone()
                    .unwrap_or_else(|| "Date table template".into());
                if let Some(f) = &d.finding {
                    detail += &format!(" · {} referencing objects", f.used_by.len());
                }
                ui.label(RichText::new(detail).weak());
            });
        }
    });
}

fn coverage_tab(ui: &mut egui::Ui, analysis: &Analysis) {
    let r = &analysis.output;
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.label(RichText::new(format!("Connected reports ({})", r.reports.len())).strong());
        for p in &r.reports {
            ui.monospace(p);
        }
        ui.add_space(8.0);
        ui.label(RichText::new(format!("Parser notices ({})", r.skips.len())).strong());
        if r.skips.is_empty() {
            ui.label(RichText::new("No parser skips reported.").weak());
        }
        for n in &r.skips {
            card(ui, |ui| {
                ui.monospace(&n.path);
                ui.label(RichText::new(&n.detail).weak());
            });
        }
        ui.add_space(8.0);
        egui::CollapsingHeader::new(RichText::new("Discovery & configuration log").strong())
            .default_open(false)
            .show(ui, |ui| {
                ui.add(
                    TextEdit::multiline(
                        &mut analysis
                            .log
                            .join(
                                "
",
                            )
                            .as_str(),
                    )
                    .code_editor()
                    .desired_width(f32::INFINITY),
                );
            });
    });
}

fn welcome(ui: &mut egui::Ui, busy: bool) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.25);
        if busy {
            ui.spinner();
            ui.add_space(8.0);
            ui.label("Discovering connected reports and tracing dependencies…");
            return;
        }
        ui.label(RichText::new("A LEANER MODEL STARTS HERE").small().weak());
        ui.label(RichText::new("Make room for what matters.").size(28.0).strong());
        ui.add_space(6.0);
        ui.label("Pick a semantic model, then analyze to find unused objects. Reports beside it are found automatically.");
        ui.add_space(20.0);
        ui.label(RichText::new("See what is really used  ·  Bring every report  ·  Find your next cleanup").weak());
    });
}

fn metric(ui: &mut egui::Ui, title: &str, value: usize, detail: &str, accent: Option<Color32>) {
    card(ui, |ui| {
        ui.label(RichText::new(title).small().weak());
        let mut number = RichText::new(value.to_string()).size(26.0).strong();
        if let Some(color) = accent {
            number = number.color(color);
        }
        ui.label(number);
        ui.label(RichText::new(detail).small().weak());
    });
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(10.0)
        .corner_radius(6.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn banner(ui: &mut egui::Ui, color: Color32, text: &str) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.15))
        .stroke(egui::Stroke::new(1.0, color))
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(color));
        });
    ui.add_space(6.0);
}

fn path_label(ui: &mut egui::Ui, path: &Path) {
    ui.vertical(|ui| {
        ui.label(RichText::new(file_name(path)).strong());
        ui.label(RichText::new(path.display().to_string()).small().weak());
    });
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn label(value: &str) -> String {
    value.replace('_', " ")
}
