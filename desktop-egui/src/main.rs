#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analysis;
mod brand;
#[cfg(test)]
mod demo;
mod fetching;
mod update;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use analysis::Analysis;
use eframe::egui::{
    self, Align, Color32, ComboBox, Layout, ProgressBar, RichText, ScrollArea, Sense, TextEdit,
};
use egui_extras::{Column, TableBuilder};
use fetching::{Answer, Event, Tone};
use ripbi_app::fetch::AuthMode;
use ripbi_app::output::Finding;

/// A tone's colour in the current theme: darker on a light background so
/// coloured text stays legible.
fn hue(ui: &egui::Ui, tone: Tone) -> Color32 {
    let dark = ui.visuals().dark_mode;
    match tone {
        Tone::Plain => ui.visuals().text_color(),
        Tone::Good if dark => Color32::from_rgb(0x3f, 0xb9, 0x50),
        Tone::Good => Color32::from_rgb(0x1a, 0x7f, 0x37),
        Tone::Warn if dark => Color32::from_rgb(0xd2, 0x99, 0x22),
        Tone::Warn => Color32::from_rgb(0x9a, 0x67, 0x00),
        Tone::Bad if dark => Color32::from_rgb(0xe5, 0x53, 0x4b),
        Tone::Bad => Color32::from_rgb(0xcf, 0x22, 0x2e),
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ripbi")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(brand::icon(64))
            // Matches StartupWMClass in assets/ripbi.desktop on Linux.
            .with_app_id("ripbi"),
        ..Default::default()
    };
    eframe::run_native(
        "ripbi",
        options,
        Box::new(|cc| {
            // Earlier builds saved egui's memory, which eframe still loads:
            // drop its widget state so panels open at their default sizes.
            cc.egui_ctx.data_mut(|data| data.clear());
            styled(&cc.egui_ctx);
            if let Some(theme) = cc
                .storage
                .and_then(|storage| eframe::get_value::<egui::ThemePreference>(storage, THEME))
            {
                cc.egui_ctx.set_theme(theme);
            }
            Ok(Box::new(App {
                updates: update::spawn(cc.egui_ctx.clone()),
                ..App::default()
            }))
        }),
    )
}

fn styled(ctx: &egui::Context) {
    ctx.style_mut_of(egui::Theme::Light, legible_light);
    ctx.style_mut_of(egui::Theme::Dark, legible_dark);
}

/// egui fades weak text to 60% and disabled widgets by a further half, which
/// washes out hints and not-yet-enabled buttons. Give both themes solid weak
/// text and a lighter disabled fade so they stay readable.
fn legible_light(style: &mut egui::Style) {
    let visuals = &mut style.visuals;
    visuals.widgets.noninteractive.fg_stroke.color = Color32::from_gray(40);
    visuals.widgets.inactive.fg_stroke.color = Color32::from_gray(30);
    visuals.weak_text_color = Some(Color32::from_gray(100));
    visuals.disabled_alpha = 0.7;
}

/// One quiet icon that cycles system → light → dark: a half-filled circle, a
/// sun, a crescent. Painted rather than taken from a font, so it is the same
/// thin line weight as the text around it.
fn theme_toggle(ui: &mut egui::Ui) {
    use egui::ThemePreference::{Dark, Light, System};
    use egui::{Shape, Stroke, Vec2, vec2};
    use std::f32::consts::{PI, TAU};

    let preference = ui.options(|options| options.theme_preference);
    let (next, tip) = match preference {
        System => (Light, "Theme follows the system. Click for light."),
        Light => (Dark, "Light theme. Click for dark."),
        Dark => (System, "Dark theme. Click to follow the system."),
    };
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    let color = if response.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let painter = ui.painter();
    let center = rect.center();
    let stroke = Stroke::new(1.2, color);
    match preference {
        System => {
            let radius = 5.5;
            painter.circle_stroke(center, radius, stroke);
            let half = (0..=16)
                .map(|i| center + Vec2::angled(-PI / 2.0 + PI * i as f32 / 16.0) * radius)
                .collect();
            painter.add(Shape::convex_polygon(half, color, Stroke::NONE));
        }
        Light => {
            painter.circle_stroke(center, 3.2, stroke);
            for i in 0..8 {
                let direction = Vec2::angled(TAU * i as f32 / 8.0);
                painter.line_segment([center + direction * 5.3, center + direction * 7.3], stroke);
            }
        }
        Dark => {
            painter.circle_filled(center, 6.0, color);
            painter.circle_filled(center + vec2(2.6, -2.2), 5.0, ui.visuals().panel_fill);
        }
    }
    if response.on_hover_text(tip).clicked() {
        ui.ctx().set_theme(next);
    }
}

fn legible_dark(style: &mut egui::Style) {
    let visuals = &mut style.visuals;
    visuals.widgets.noninteractive.fg_stroke.color = Color32::from_gray(190);
    visuals.widgets.inactive.fg_stroke.color = Color32::from_gray(210);
    visuals.weak_text_color = Some(Color32::from_gray(150));
    visuals.disabled_alpha = 0.7;
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Unused,
    Broken,
    Dates,
    Coverage,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Source {
    #[default]
    Local,
    Tenant,
}

/// A fetch in flight, and the question it is waiting on.
struct FetchJob {
    events: Receiver<Event>,
    answers: Sender<Answer>,
    status: String,
    prompt: Option<Prompt>,
}

enum Prompt {
    Pick {
        title: String,
        prompt: String,
        labels: Vec<String>,
        filter: String,
    },
    Confirm {
        unreadable: usize,
        total: usize,
    },
}

#[derive(Default)]
struct App {
    /// Mirrors egui's preference so `save` can store it.
    theme: egui::ThemePreference,
    source: Source,
    form: fetching::Form,
    fetch: Option<FetchJob>,
    /// The last forecast, then the fetch's closing notes.
    fetch_log: Vec<(Tone, String)>,
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
    /// The background update check, until it stages an update.
    updates: Option<Receiver<update::Staged>>,
    /// A verified update waiting for "Restart to update".
    staged: Option<update::Staged>,
}

/// Storage key for the theme preference, the one setting kept across runs.
const THEME: &str = "theme";

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, THEME, &self.theme);
    }

    /// egui's memory would also keep panel widths, so a panel that once grew
    /// to fit wide content would open that wide every time after.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_fetch();
        if let Some(staged) = self
            .updates
            .as_ref()
            .and_then(|updates| updates.try_recv().ok())
        {
            self.staged = Some(staged);
            self.updates = None;
        }
        let Some(job) = &self.job else { return };
        match job.try_recv() {
            Ok(Ok(analysis)) => {
                self.show(analysis);
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
        self.theme = ui.options(|options| options.theme_preference);
        self.accept_dropped_files(ui.ctx());

        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let (logo, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::hover());
                brand::paint(ui.painter(), logo);
                ui.heading(RichText::new("ripbi").strong());
                ui.label(RichText::new("DESKTOP").small().weak());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Dark, light or follow the system; remembered across runs.
                    theme_toggle(ui);
                    ui.separator();
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
            if let Some(error) = &self.error
                && banner(ui, Tone::Bad, error)
            {
                self.error = None;
            }
            if let Some(notice) = &self.notice
                && banner(ui, Tone::Good, notice)
            {
                self.notice = None;
            }
            if !self.fetch_log.is_empty() {
                fetch_log(ui, &self.fetch_log);
            }
            if self.analysis.is_some() {
                self.results(ui);
            } else if let Some(fetch) = &self.fetch {
                waiting(ui, &fetch.status);
            } else {
                welcome(ui, self.job.is_some(), self.source);
            }
        });

        self.prompt(ui.ctx());
        self.update_ready(ui.ctx());
    }
}

impl App {
    fn busy(&self) -> bool {
        self.job.is_some() || self.fetch.is_some()
    }

    /// A small card in the bottom-left corner once an update is downloaded
    /// and verified: "Restart to update" installs it and reopens ripbi.
    fn update_ready(&mut self, ctx: &egui::Context) {
        let Some(staged) = &self.staged else { return };
        let busy = self.busy();
        let mut restart = false;
        let mut later = false;
        egui::Area::new(egui::Id::unique("update-ready"))
            .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(12.0, -12.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                        ui.painter()
                            .circle_filled(dot.center(), 4.0, hue(ui, Tone::Good));
                        ui.label(format!("ripbi {} is ready", staged.version));
                        restart = ui
                            .add_enabled(!busy, egui::Button::new("Restart to update"))
                            .on_disabled_hover_text("Available when the current job finishes")
                            .clicked();
                        later = ui
                            .add(egui::Button::new(RichText::new("×").weak()).frame(false))
                            .on_hover_text("Later: ripbi asks again next time it starts")
                            .clicked();
                    });
                });
            });
        if restart {
            match staged.apply() {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(error) => self.error = Some(error),
            }
            self.staged = None;
        } else if later {
            self.staged = None;
        }
    }

    fn inputs(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.add_enabled_ui(!self.busy(), |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.source, Source::Local, "📁 Local files");
                ui.selectable_value(&mut self.source, Source::Tenant, "☁ Tenant");
            });
        });
        ui.add_space(8.0);
        match self.source {
            Source::Local => self.local_inputs(ui),
            Source::Tenant => self.tenant_inputs(ui),
        }
    }

    fn local_inputs(&mut self, ui: &mut egui::Ui) {
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

    fn tenant_inputs(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        ui.add_enabled_ui(!busy, |ui| {
            ui.label(RichText::new("01  WHICH MODEL").small().strong());
            ui.add_space(4.0);
            ui.label(RichText::new("Paste a link to the model or one of its reports:").weak());
            ui.add(
                TextEdit::singleline(&mut self.form.link)
                    .hint_text("https://app.powerbi.com/groups/…")
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
            ui.label(RichText::new("…or name them (leave blank to pick from a list):").weak());
            egui::Grid::new("fetch-names")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Workspace");
                    ui.add(
                        TextEdit::singleline(&mut self.form.workspace)
                            .hint_text("name or id")
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                    ui.label("Model");
                    ui.add(
                        TextEdit::singleline(&mut self.form.model)
                            .hint_text("name or id")
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(RichText::new("02  WHERE TO PUT IT").small().strong());
            ui.add_space(4.0);
            ui.label(
                RichText::new("The project is written to a folder named after the model in:")
                    .weak(),
            );
            match &self.form.parent {
                Some(parent) => path_label(ui, parent),
                None => {
                    ui.label(RichText::new("No folder chosen").weak());
                }
            }
            if ui.button("📁 Choose folder…").clicked()
                && let Some(path) = rfd::FileDialog::new().set_title("Fetch into").pick_folder()
            {
                self.form.parent = Some(path);
            }
            ui.checkbox(&mut self.form.force, "Replace a folder fetch did not write");

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(RichText::new("03  SIGN-IN").small().strong());
            ui.add_space(4.0);
            ComboBox::from_id_salt("auth")
                .selected_text(auth_label(self.form.auth))
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for mode in [AuthMode::Auto, AuthMode::Az, AuthMode::Env, AuthMode::Sp] {
                        ui.selectable_value(&mut self.form.auth, mode, auth_label(mode));
                    }
                });
            ui.label(RichText::new(auth_hint(self.form.auth)).small().weak());
            ui.checkbox(
                &mut self.form.verify,
                "Cross-check against each workspace's items",
            );
        });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        let ready = !busy && self.form.parent.is_some();
        ui.horizontal(|ui| {
            let button = egui::Button::new(RichText::new("Fetch & analyze  →").strong())
                .min_size(egui::vec2(140.0, 30.0));
            if ui.add_enabled(ready, button).clicked() {
                self.start_fetch(ui.ctx(), false);
            }
            if ui
                .add_enabled(ready, egui::Button::new("Forecast only"))
                .on_hover_text("Check which reports you can read; download nothing")
                .clicked()
            {
                self.start_fetch(ui.ctx(), true);
            }
        });
        if let Some(fetch) = &self.fetch {
            ui.add_space(4.0);
            // The status wraps in what the spinner leaves: an unwrapped row
            // would widen the resizable panel to fit, and it keeps that width.
            ui.horizontal_top(|ui| {
                ui.spinner();
                ui.add(egui::Label::new(fetch.status.as_str()).wrap());
            });
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "Downloads the model and every downstream report you can read. \
                 Reports you cannot read are listed first: objects only they use \
                 would show as unused.",
            )
            .small()
            .weak(),
        );
    }

    fn start_fetch(&mut self, ctx: &egui::Context, dry_run: bool) {
        let (events_tx, events) = channel();
        let (answers, answers_rx) = channel();
        let form = self.form.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            fetching::run(&form, dry_run, &events_tx, &answers_rx, &ctx);
        });
        self.fetch = Some(FetchJob {
            events,
            answers,
            status: "Starting…".into(),
            prompt: None,
        });
        self.fetch_log.clear();
        self.analysis = None;
        self.error = None;
        self.notice = None;
    }

    fn poll_fetch(&mut self) {
        let Some(fetch) = &mut self.fetch else { return };
        loop {
            match fetch.events.try_recv() {
                Ok(Event::Status(status)) => fetch.status = status,
                Ok(Event::Forecast(lines)) => self.fetch_log = lines,
                Ok(Event::Pick {
                    title,
                    prompt,
                    labels,
                }) => {
                    fetch.prompt = Some(Prompt::Pick {
                        title,
                        prompt,
                        labels,
                        filter: String::new(),
                    });
                }
                Ok(Event::ConfirmIncomplete { unreadable, total }) => {
                    fetch.prompt = Some(Prompt::Confirm { unreadable, total });
                }
                Ok(Event::Done(done)) => {
                    self.fetch = None;
                    self.finish_fetch(done);
                    return;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.fetch = None;
                    self.error = Some("Fetch worker stopped unexpectedly.".into());
                    return;
                }
            }
        }
    }

    fn finish_fetch(&mut self, done: Result<Box<fetching::Fetched>, String>) {
        let fetched = match done {
            Ok(fetched) => fetched,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        self.fetch_log.extend(fetched.summary);
        let Some(analysis) = fetched.analysis else {
            self.notice = Some("Forecast only: nothing was downloaded.".into());
            return;
        };
        if let Some(model) = fetched.model {
            self.model = Some(model);
            self.reports.clear();
        }
        self.notice = Some(format!(
            "Fetched into {}. Switch to Local files to re-analyze it later.",
            fetched.root.display()
        ));
        self.show(analysis);
    }

    /// The fetch's open question, as a modal; the answer unblocks the worker.
    fn prompt(&mut self, ctx: &egui::Context) {
        let Some(fetch) = &mut self.fetch else { return };
        let Some(prompt) = &mut fetch.prompt else {
            return;
        };
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::unique("fetch-prompt")).show(ctx, |ui| {
            ui.set_width(460.0);
            match prompt {
                Prompt::Pick {
                    title,
                    prompt,
                    labels,
                    filter,
                } => {
                    ui.heading(title.as_str());
                    ui.label(prompt.as_str());
                    ui.add_space(4.0);
                    ui.add(
                        TextEdit::singleline(filter)
                            .hint_text("Filter…")
                            .desired_width(f32::INFINITY),
                    )
                    .request_focus();
                    let needle = filter.to_lowercase();
                    ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for (i, text) in labels.iter().enumerate() {
                            if (needle.is_empty() || text.to_lowercase().contains(&needle))
                                && ui
                                    .add(
                                        egui::Button::selectable(false, text.as_str())
                                            .min_size(egui::vec2(ui.available_width(), 0.0)),
                                    )
                                    .clicked()
                            {
                                answer = Some(Answer::Pick(Some(i)));
                            }
                        }
                    });
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        answer = Some(Answer::Pick(None));
                    }
                }
                Prompt::Confirm { unreadable, total } => {
                    ui.heading("Some reports can't be read");
                    ui.label(format!(
                        "{unreadable} of {total} downstream reports can't be read. \
                         Objects only they use will show as unused."
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Fetch the readable reports").clicked() {
                            answer = Some(Answer::Confirm(true));
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Some(Answer::Confirm(false));
                        }
                    });
                }
            }
        });
        if answer.is_none() && modal.should_close() {
            answer = Some(match prompt {
                Prompt::Pick { .. } => Answer::Pick(None),
                Prompt::Confirm { .. } => Answer::Confirm(false),
            });
        }
        if let Some(answer) = answer {
            _ = fetch.answers.send(answer);
            fetch.prompt = None;
        }
    }

    fn set_model(&mut self, path: PathBuf) {
        self.model = Some(path);
        self.reports.clear();
        self.analysis = None;
        self.error = None;
        self.notice = None;
    }

    fn show(&mut self, analysis: Analysis) {
        self.analysis = Some(analysis);
        self.tab = Tab::Unused;
        self.query.clear();
        self.kind = None;
        self.selected = None;
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
        if !dropped.is_empty() {
            self.source = Source::Local;
        }
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
        self.fetch_log.clear();
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
            ui.label(RichText::new("●").color(hue(ui, Tone::Good)));
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
                Some(Tone::Good),
            );
            metric(
                &mut cols[2],
                "Unused",
                s.unused_raw,
                "Includes hidden date machinery",
                Some(Tone::Warn),
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
                bar(
                    ui,
                    reachable,
                    Tone::Good,
                    format!("{:.0}% reachable", reachable * 100.0),
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
                    bar(
                        ui,
                        *count as f32 / max as f32,
                        Tone::Warn,
                        format!("{}  ·  {count}", label(kind)),
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
                    ui.label(
                        RichText::new(label(f.kind))
                            .small()
                            .color(hue(ui, Tone::Warn)),
                    );
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
    ui.label(
        RichText::new(label(f.kind))
            .small()
            .color(hue(ui, Tone::Warn)),
    );
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
                ui.label(
                    RichText::new("ALSO UNUSED")
                        .small()
                        .color(hue(ui, Tone::Warn)),
                );
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
                ui.label(RichText::new(&b.target).strong().color(hue(ui, Tone::Bad)));
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
                ui.label(RichText::new(&b.id).strong().color(hue(ui, Tone::Bad)));
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
                    ui.label(
                        RichText::new(label(d.verdict))
                            .small()
                            .color(hue(ui, Tone::Warn)),
                    );
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

fn waiting(ui: &mut egui::Ui, status: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.25);
        ui.spinner();
        ui.add_space(8.0);
        ui.label(status);
    });
}

fn fetch_log(ui: &mut egui::Ui, lines: &[(Tone, String)]) {
    card(ui, |ui| {
        for (tone, text) in lines {
            let text = RichText::new(text);
            ui.label(match tone {
                Tone::Plain => text.strong(),
                Tone::Good | Tone::Warn | Tone::Bad => text.color(hue(ui, *tone)),
            });
        }
    });
    ui.add_space(8.0);
}

fn auth_label(mode: AuthMode) -> &'static str {
    match mode {
        AuthMode::Auto => "Automatic",
        AuthMode::Az => "Azure CLI (az login)",
        AuthMode::Env => "Token from environment",
        AuthMode::Sp => "Service principal",
    }
}

fn auth_hint(mode: AuthMode) -> &'static str {
    match mode {
        AuthMode::Auto => {
            "RIPBI_FABRIC_TOKEN if set, else a service principal from AZURE_* variables, \
             else your `az login`."
        }
        AuthMode::Az => "Uses the account you signed in with `az login`.",
        AuthMode::Env => "Reads RIPBI_FABRIC_TOKEN (and RIPBI_POWERBI_TOKEN, if set).",
        AuthMode::Sp => "Reads AZURE_TENANT_ID, AZURE_CLIENT_ID and AZURE_CLIENT_SECRET.",
    }
}

fn welcome(ui: &mut egui::Ui, busy: bool, source: Source) {
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
        ui.label(match source {
            Source::Local => "Pick a semantic model, then analyze to find unused objects. Reports beside it are found automatically.",
            Source::Tenant => "Point at a model in your Fabric or Power BI tenant. ripbi downloads it with every report you can read, then analyzes it.",
        });
        ui.add_space(20.0);
        ui.label(RichText::new("See what is really used  ·  Bring every report  ·  Find your next cleanup").weak());
    });
}

/// A labelled bar. Light mode's tones are dark for legible text, so there
/// the fill is a pale tint under ordinary text; its label runs across both
/// the fill and the track, and one text colour must read on each.
fn bar(ui: &mut egui::Ui, fraction: f32, tone: Tone, text: String) {
    let mut bar = ProgressBar::new(fraction);
    if ui.visuals().dark_mode {
        bar = bar.fill(hue(ui, tone)).text(text);
    } else {
        let tint = hue(ui, tone).gamma_multiply(0.3);
        let color = ui.visuals().strong_text_color();
        bar = bar.fill(tint).text(RichText::new(text).color(color));
    }
    ui.add(bar);
}

fn metric(ui: &mut egui::Ui, title: &str, value: usize, detail: &str, accent: Option<Tone>) {
    card(ui, |ui| {
        ui.label(RichText::new(title).small().weak());
        let mut number = RichText::new(value.to_string()).size(26.0).strong();
        if let Some(tone) = accent {
            number = number.color(hue(ui, tone));
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

/// A coloured message with a × to dismiss it; true once dismissed.
fn banner(ui: &mut egui::Ui, tone: Tone, text: &str) -> bool {
    let color = hue(ui, tone);
    let mut dismissed = false;
    egui::Frame::new()
        .fill(color.gamma_multiply(0.15))
        .stroke(egui::Stroke::new(1.0, color))
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // A top-aligned row: the text wraps in what the × leaves. (A
            // right-to-left layout here would claim the panel's full height.)
            ui.horizontal_top(|ui| {
                let close = egui::Button::new(RichText::new("×").size(16.0).color(color))
                    .frame(false)
                    .min_size(egui::vec2(18.0, 18.0));
                let room = ui.available_width() - 18.0 - ui.spacing().item_spacing.x;
                ui.allocate_ui(egui::vec2(room, 0.0), |ui| {
                    ui.set_width(room);
                    ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
                });
                dismissed = ui.add(close).on_hover_text("Dismiss").clicked();
            });
        });
    ui.add_space(6.0);
    dismissed
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

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    /// A long fetch status wraps instead of widening the inputs panel.
    #[test]
    fn fetch_status_keeps_the_inputs_panel_width() {
        let status = "Signed in with az login (tenant 00000000-0000-0000-0000-000000000000); \
                      resolving every downstream report of the model";
        let (_events_tx, events) = channel();
        let (answers, _answers_rx) = channel();
        let app = App {
            source: Source::Tenant,
            fetch: Some(FetchJob {
                events,
                answers,
                status: status.into(),
                prompt: None,
            }),
            ..App::default()
        };
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 740.0))
            .with_context_setup(|ctx| {
                // As eframe does for the real window: emoji and symbols.
                ctx.add_font_provider(std::sync::Arc::new(
                    egui_system_font_provider::SystemFontProvider::new(),
                ));
                styled(ctx);
            })
            .build_eframe(|_| app);
        // A spinner repaints forever, so step rather than run to rest.
        for _ in 0..5 {
            harness.step();
        }
        // The central pane's waiting screen shows it too; take the panel's.
        let right = harness
            .get_all_by_label(status)
            .map(|node| node.rect())
            .min_by(|a, b| a.left().total_cmp(&b.left()))
            .expect("the status shows")
            .right();
        assert!(right < 340.0, "status reaches x = {right}");
    }
}
