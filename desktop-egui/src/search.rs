//! The Search tab: find text in every DAX and Power Query expression of the
//! analyzed model and its reports — `ripbi search`, as you type.
//!
//! `ripbi_app::search::load` ingests the model once per analysis on a worker
//! thread; each change to the pattern or its options then asks the loaded
//! `Searcher`, which stays in memory and answers within a frame.

use std::ops::Range;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Align, Color32, ComboBox, FontId, Label, Layout, RichText, ScrollArea, Sense, TextEdit,
    TextFormat, TextStyle,
};
use ripbi_app::deps::DepsRequest;
use ripbi_app::search::{SearchOutput, SearchRequest, Searcher};

use crate::fetching::Tone;

/// Which expressions a search reads.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Scope {
    #[default]
    Both,
    Dax,
    PowerQuery,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Scope::Both => "DAX and Power Query",
            Scope::Dax => "DAX only",
            Scope::PowerQuery => "Power Query only",
        }
    }
}

/// What the user asked for; the rows are recomputed when it changes.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
struct Query {
    pattern: String,
    regex: bool,
    case_sensitive: bool,
    scope: Scope,
}

/// One line of the result list.
#[derive(Debug, PartialEq, Eq)]
enum Row {
    /// The object owning the expression the lines below came from.
    Owner {
        /// Display id, e.g. `'Sales'[Total]`.
        id: String,
        /// Its type, the property when not the main expression, and a report
        /// measure's report.
        detail: String,
    },
    /// One matching line.
    Line {
        /// The owner's display id, for the lineage jump.
        id: String,
        /// `line:column`.
        position: String,
        /// The line without its indentation.
        text: String,
        /// The match within `text`.
        span: Range<usize>,
    },
}

/// The tab's state for one analysis.
pub struct Search {
    base: SearchRequest,
    loading: Option<Receiver<Result<Searcher, String>>>,
    searcher: Option<Searcher>,
    error: Option<String>,
    query: Query,
    /// The query `rows` and `status` answer.
    shown: Option<Query>,
    rows: Vec<Row>,
    /// The count line, or why the pattern cannot be searched.
    status: Result<String, String>,
}

impl Search {
    /// A search over the same target as the analysis's `ripbi deps` request.
    pub fn new(deps: &DepsRequest) -> Self {
        Self {
            base: SearchRequest {
                cwd: deps.cwd.clone(),
                model: deps.model.clone(),
                reports: deps.reports.clone(),
                no_input: true,
                ..SearchRequest::default()
            },
            loading: None,
            searcher: None,
            error: None,
            query: Query::default(),
            shown: None,
            rows: Vec::new(),
            status: Ok(String::new()),
        }
    }

    /// Starts ingesting the model on a worker thread, once.
    fn ensure_loaded(&mut self, ctx: &egui::Context) {
        if self.searcher.is_some() || self.loading.is_some() || self.error.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let base = self.base.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            _ = tx.send(load(&base));
            ctx.request_repaint();
        });
        self.loading = Some(rx);
    }

    pub fn poll(&mut self) {
        let Some(loading) = &self.loading else { return };
        match loading.try_recv() {
            Ok(Ok(searcher)) => {
                self.searcher = Some(searcher);
                self.loading = None;
            }
            Ok(Err(error)) => {
                self.error = Some(error);
                self.loading = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.error = Some("Search worker stopped unexpectedly.".into());
                self.loading = None;
            }
        }
    }

    /// Recomputes the rows when the query changed since they were built.
    fn refresh(&mut self) {
        let Some(searcher) = &self.searcher else {
            return;
        };
        if self.shown.as_ref() == Some(&self.query) {
            return;
        }
        self.shown = Some(self.query.clone());
        self.rows.clear();
        if self.query.pattern.is_empty() {
            self.status = Ok(String::new());
            return;
        }
        let request = SearchRequest {
            pattern: self.query.pattern.clone(),
            regex: self.query.regex,
            case_sensitive: self.query.case_sensitive,
            dax: self.query.scope == Scope::Dax,
            m: self.query.scope == Scope::PowerQuery,
            ..SearchRequest::default()
        };
        match searcher.find(&request) {
            Ok(output) => {
                self.status = Ok(count_line(&output));
                self.rows = rows(&output);
            }
            Err(error) => self.status = Err(error.message),
        }
    }

    /// Draws the tab. Returns the display id of an object to show in Lineage
    /// when one was double-clicked.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        self.ensure_loaded(ui.ctx());
        ui.horizontal(|ui| {
            ui.add(
                TextEdit::singleline(&mut self.query.pattern)
                    .hint_text("Search DAX and Power Query…")
                    .desired_width(320.0),
            );
            ui.checkbox(&mut self.query.case_sensitive, "Match case");
            ui.checkbox(&mut self.query.regex, "Regex");
            ComboBox::from_id_salt("search-scope")
                .selected_text(self.query.scope.label())
                .show_ui(ui, |ui| {
                    for scope in [Scope::Both, Scope::Dax, Scope::PowerQuery] {
                        ui.selectable_value(&mut self.query.scope, scope, scope.label());
                    }
                });
        });
        if let Some(error) = &self.error {
            ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
            return None;
        }
        if self.searcher.is_none() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the model's expressions…");
            });
            return None;
        }
        self.refresh();
        match &self.status {
            Ok(text) if text.is_empty() => {
                ui.label(
                    RichText::new(
                        "Type to search measures, calculated columns and tables, calculation \
                         items, RLS filters, partitions, and report-level measures.",
                    )
                    .weak(),
                );
            }
            Ok(text) => {
                ui.label(RichText::new(text).weak());
            }
            Err(problem) => {
                ui.label(RichText::new(problem).color(crate::hue(ui, Tone::Bad)));
            }
        }
        ui.add_space(4.0);

        let mut trace = None;
        let mono = TextStyle::Monospace.resolve(ui.style());
        let row_height = ui.text_style_height(&TextStyle::Body).max(mono.size) + 4.0;
        let hit = crate::hue(ui, Tone::Warn);
        let weak = ui.visuals().weak_text_color();
        let text_color = ui.visuals().text_color();
        ScrollArea::vertical().auto_shrink(false).show_rows(
            ui,
            row_height,
            self.rows.len(),
            |ui, range| {
                for row in &self.rows[range] {
                    let (job, id) = match row {
                        Row::Owner { id, detail } => (owner_job(ui, id, detail, weak), id),
                        Row::Line {
                            id,
                            position,
                            text,
                            span,
                        } => (
                            line_job(&mono, position, text, span, text_color, weak, hit),
                            id,
                        ),
                    };
                    let response = ui
                        .allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), row_height),
                            Layout::left_to_right(Align::Center),
                            |ui| ui.add(Label::new(job).truncate().sense(Sense::click())),
                        )
                        .inner
                        .on_hover_text("Double-click for the object's lineage");
                    if response.double_clicked() {
                        trace = Some(id.clone());
                    }
                }
            },
        );
        trace
    }
}

fn load(base: &SearchRequest) -> Result<Searcher, String> {
    ripbi_app::search::load(base, &mut ()).map_err(|error| error.to_string())
}

/// `12 matching lines in 5 objects`, or that nothing matched.
fn count_line(output: &SearchOutput) -> String {
    let plural = |count: usize, noun: &str| {
        if count == 1 {
            format!("{count} {noun}")
        } else {
            format!("{count} {noun}s")
        }
    };
    if output.hits.is_empty() {
        "No matches".to_owned()
    } else {
        format!(
            "{} in {} · double-click a row for the object's lineage",
            plural(output.hits.len(), "matching line"),
            plural(output.objects, "object")
        )
    }
}

/// The hits as list rows: an owner row whenever the expression changes, then
/// its matching lines.
fn rows(output: &SearchOutput) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut previous: Option<(&str, Option<&str>, &str)> = None;
    for hit in &output.hits {
        let block = (hit.id.as_str(), hit.report.as_deref(), hit.property);
        if previous != Some(block) {
            previous = Some(block);
            // Ids of everything but columns and measures already lead with
            // the type (`calculation item 'Time'[YoY]`).
            let kind = hit.kind.replace('_', " ");
            let mut parts = Vec::new();
            if !hit.id.starts_with(&kind) {
                parts.push(kind);
            }
            if hit.property != "expression" {
                parts.push(hit.property.replace('_', " "));
            }
            if hit.language == "m" {
                parts.push("Power Query".to_owned());
            }
            if let Some(report) = &hit.report {
                parts.push(crate::file_name(std::path::Path::new(report)));
            }
            rows.push(Row::Owner {
                id: hit.id.clone(),
                detail: parts.join(" · "),
            });
        }
        let indent = hit.text.len() - hit.text.trim_start().len();
        let text = hit.text[indent..].trim_end();
        let start = hit.span.start.saturating_sub(indent).min(text.len());
        let end = hit.span.end.saturating_sub(indent).clamp(start, text.len());
        rows.push(Row::Line {
            id: hit.id.clone(),
            position: format!("{}:{}", hit.line, hit.column),
            text: text.to_owned(),
            span: start..end,
        });
    }
    rows
}

fn owner_job(ui: &egui::Ui, id: &str, detail: &str, weak: Color32) -> LayoutJob {
    let body = TextStyle::Body.resolve(ui.style());
    let mut job = LayoutJob::default();
    job.append(
        id,
        0.0,
        TextFormat::simple(body.clone(), ui.visuals().strong_text_color()),
    );
    job.append(
        detail,
        12.0,
        TextFormat::simple(FontId::new(body.size * 0.9, body.family), weak),
    );
    job
}

/// `   3:5  CALCULATE(…)` in the monospace font, the match picked out.
fn line_job(
    mono: &FontId,
    position: &str,
    text: &str,
    span: &Range<usize>,
    color: Color32,
    weak: Color32,
    hit: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        &format!("{position:>6}"),
        8.0,
        TextFormat::simple(mono.clone(), weak),
    );
    // A span off a character boundary (never produced, but the text is
    // user data) shows the line without a highlight rather than panicking.
    let (before, matched, after) = match (
        text.get(..span.start),
        text.get(span.clone()),
        text.get(span.end..),
    ) {
        (Some(before), Some(matched), Some(after)) => (before, matched, after),
        _ => (text, "", ""),
    };
    job.append(before, 12.0, TextFormat::simple(mono.clone(), color));
    job.append(
        matched,
        0.0,
        TextFormat {
            font_id: mono.clone(),
            color: hit,
            background: hit.gamma_multiply(0.18),
            ..TextFormat::default()
        },
    );
    job.append(after, 0.0, TextFormat::simple(mono.clone(), color));
    job
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn mini() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../crates/ripbi-cli/tests/fixtures/mini-pbip")
            .canonicalize()
            .expect("the mini fixture exists")
    }

    /// A tab over the mini fixture, loaded without the worker thread.
    fn loaded() -> Search {
        let root = mini();
        let mut search = Search::new(&DepsRequest {
            cwd: root.clone(),
            model: Some(root.join("Mini.SemanticModel")),
            no_input: true,
            ..DepsRequest::default()
        });
        search.searcher = Some(load(&search.base).expect("the fixture loads"));
        search
    }

    #[test]
    fn typing_lists_each_owner_above_its_matching_lines() {
        let mut search = loaded();
        search.query.pattern = "sum(".into();
        search.refresh();
        assert_eq!(
            search.rows,
            [
                Row::Owner {
                    id: "'Sales'[Total]".into(),
                    detail: "measure".into(),
                },
                Row::Line {
                    id: "'Sales'[Total]".into(),
                    position: "1:1".into(),
                    text: "SUM('Sales'[Amount])".into(),
                    span: 0..4,
                },
                Row::Owner {
                    id: "'Sales'[Legacy Total]".into(),
                    detail: "measure".into(),
                },
                Row::Line {
                    id: "'Sales'[Legacy Total]".into(),
                    position: "1:1".into(),
                    text: "SUM('Sales'[Legacy])".into(),
                    span: 0..4,
                },
            ]
        );
        assert!(
            search
                .status
                .as_ref()
                .is_ok_and(|text| text.starts_with("2 matching lines in 2 objects"))
        );
    }

    #[test]
    fn the_scope_and_case_options_narrow_the_rows() {
        let mut search = loaded();
        search.query.pattern = "#table".into();
        search.refresh();
        assert!(matches!(
            search.rows.first(),
            Some(Row::Owner { detail, .. }) if detail == "Power Query"
        ));
        // Indentation is dropped and the span follows the text.
        let Some(Row::Line { text, span, .. }) = search.rows.get(1) else {
            panic!("a matching line follows its owner: {:?}", search.rows);
        };
        assert_eq!(&text[span.clone()], "#table");

        search.query.scope = Scope::Dax;
        search.refresh();
        assert!(search.rows.is_empty());
        assert_eq!(search.status, Ok("No matches".into()));

        search.query = Query {
            pattern: "sum(".into(),
            case_sensitive: true,
            ..Query::default()
        };
        search.refresh();
        assert!(search.rows.is_empty());
    }

    #[test]
    fn a_bad_regex_is_a_message_not_a_result() {
        let mut search = loaded();
        search.query = Query {
            pattern: "(".into(),
            regex: true,
            ..Query::default()
        };
        search.refresh();
        assert!(search.rows.is_empty());
        assert!(
            search
                .status
                .as_ref()
                .is_err_and(|problem| problem.contains("not a valid regular expression"))
        );

        // An empty box asks nothing.
        search.query = Query::default();
        search.refresh();
        assert_eq!(search.status, Ok(String::new()));
    }
}
