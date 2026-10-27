//! Desktop adapter over `ripbi_app::fetch`: the flow `ripbi fetch --scan`
//! runs, on a worker thread. The fetch's questions (which workspace, which
//! model, fetch an incomplete set?) arrive as [`Event`]s and block the worker
//! until the UI sends an [`Answer`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use eframe::egui;
use ripbi_app::ScanError;
use ripbi_app::fetch::{self, AuthMode, FetchOutcome, FetchRequest, Observer, ReportStatus};
use ripbi_service::Service;
use ripbi_service::downstream::{Access, Downstream, Verification};
use ripbi_service::http::UreqTransport;

use crate::analysis::{self, Analysis};

/// One request's budget, as the CLI's.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// What the fetch form asks for.
#[derive(Clone)]
pub struct Form {
    pub link: String,
    pub workspace: String,
    pub model: String,
    /// The folder the project folder goes into.
    pub parent: Option<PathBuf>,
    pub auth: AuthMode,
    pub verify: bool,
    pub force: bool,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            link: String::new(),
            workspace: String::new(),
            model: String::new(),
            parent: default_parent(),
            auth: AuthMode::Auto,
            verify: false,
            force: false,
        }
    }
}

/// `~/Documents/ripbi`, where it can be found.
fn default_parent() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join("Documents").join("ripbi"))
}

/// A forecast line and how it reads.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Good,
    Warn,
    Bad,
}

/// Worker → UI.
pub enum Event {
    Status(String),
    Forecast(Vec<(Tone, String)>),
    /// Answer with [`Answer::Pick`].
    Pick {
        title: String,
        prompt: String,
        labels: Vec<String>,
    },
    /// Answer with [`Answer::Confirm`].
    ConfirmIncomplete {
        unreadable: usize,
        total: usize,
    },
    Done(Result<Box<Fetched>, String>),
}

/// UI → worker.
pub enum Answer {
    /// `None` cancels the fetch.
    Pick(Option<usize>),
    Confirm(bool),
}

/// A fetched project and its analysis.
pub struct Fetched {
    /// The project folder.
    pub root: PathBuf,
    /// The fetched model folder, for re-analysis from the local tab.
    pub model: Option<PathBuf>,
    /// Closing notes: what was written, what is missing.
    pub summary: Vec<(Tone, String)>,
    /// `None` on a dry run.
    pub analysis: Option<Analysis>,
}

/// Runs the whole fetch and its scan; every outcome ends in [`Event::Done`].
pub fn run(
    form: &Form,
    dry_run: bool,
    events: &Sender<Event>,
    answers: &Receiver<Answer>,
    ctx: &egui::Context,
) {
    let mut observer = GuiObserver {
        events,
        answers,
        ctx,
    };
    let done = fetch_and_scan(form, dry_run, &mut observer).map(Box::new);
    _ = events.send(Event::Done(done));
    ctx.request_repaint();
}

fn fetch_and_scan(
    form: &Form,
    dry_run: bool,
    observer: &mut GuiObserver<'_>,
) -> Result<Fetched, String> {
    let parent = form
        .parent
        .clone()
        .ok_or("Choose a folder to fetch into.")?;
    let field = |text: &str| Some(text.trim().to_owned()).filter(|text| !text.is_empty());
    let request = FetchRequest {
        cwd: parent.clone(),
        link: field(&form.link),
        workspace: field(&form.workspace),
        model_name: field(&form.model),
        out: None,
        verify: form.verify,
        dry_run,
        allow_incomplete: false,
        force: form.force,
        interactive: true,
    };
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let tokens = fetch::token_source(form.auth, &env).map_err(|error| error.to_string())?;
    let service = Service::new(Box::new(UreqTransport::new(REQUEST_TIMEOUT)), tokens);
    observer.status("Signing in…");
    if !dry_run {
        std::fs::create_dir_all(&parent)
            .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
    }

    let outcome = fetch::run(&request, &service, observer).map_err(|error| error.to_string())?;
    let FetchOutcome::Written {
        root,
        written,
        refused,
    } = outcome
    else {
        return Ok(Fetched {
            root: parent,
            model: None,
            summary: Vec::new(),
            analysis: None,
        });
    };

    let manifest = &written.manifest;
    let downloaded = manifest
        .reports
        .iter()
        .filter(|report| report.status == ReportStatus::Downloaded)
        .count();
    let mut summary = vec![(
        Tone::Good,
        format!(
            "Fetched {} and {} into {}",
            manifest.model.name,
            count(downloaded, "report", "reports"),
            root.display()
        ),
    )];
    for report in manifest
        .reports
        .iter()
        .filter(|report| report.http_status.is_some() || report.status == ReportStatus::Failed)
    {
        let why = report
            .error
            .clone()
            .unwrap_or_else(|| "not readable (HTTP 404: item-level permissions)".into());
        summary.push((
            Tone::Warn,
            format!("'{}' in '{}': {why}", report.name, report.workspace_name),
        ));
    }
    let missing = manifest.unreadable_count();
    if missing > 0 {
        summary.push((
            Tone::Warn,
            format!(
                "{} missing — objects only they use will show as unused.",
                count(missing, "report is", "reports are")
            ),
        ));
    }
    if !written.config_written {
        summary.push((
            Tone::Warn,
            "Kept the existing ripbi.toml (not written by fetch); point its target and reports at the fetched folders.".into(),
        ));
    }
    if refused {
        return Err(format!(
            "More reports failed than the forecast predicted; the project is in {}, but it was not analyzed.",
            root.display()
        ));
    }

    let model = Some(root.join(&manifest.model.path)).filter(|path| path.exists());
    observer.status("Analyzing the fetched project…");
    let analysis = analysis::run_project(&root)?;
    Ok(Fetched {
        root,
        model,
        summary,
        analysis: Some(analysis),
    })
}

struct GuiObserver<'a> {
    events: &'a Sender<Event>,
    answers: &'a Receiver<Answer>,
    ctx: &'a egui::Context,
}

impl GuiObserver<'_> {
    fn send(&self, event: Event) {
        _ = self.events.send(event);
        self.ctx.request_repaint();
    }

    fn status(&self, text: impl Into<String>) {
        self.send(Event::Status(text.into()));
    }
}

impl Observer for GuiObserver<'_> {
    fn signed_in(&mut self, auth: &str, tenant: Option<&str>) -> Result<(), ScanError> {
        let tenant = tenant
            .map(|tenant| format!(" (tenant {tenant})"))
            .unwrap_or_default();
        self.status(format!(
            "Signed in via {auth}{tenant}. Finding downstream reports…"
        ));
        Ok(())
    }

    fn pick(&mut self, title: &str, prompt: &str, labels: &[String]) -> Result<usize, ScanError> {
        self.send(Event::Pick {
            title: title.to_owned(),
            prompt: prompt.to_owned(),
            labels: labels.to_vec(),
        });
        match self.answers.recv() {
            Ok(Answer::Pick(Some(index))) if index < labels.len() => Ok(index),
            _ => Err(ScanError::new("cancelled; nothing was downloaded")),
        }
    }

    fn forecast(
        &mut self,
        found: &Downstream,
        verification: Option<&Verification>,
    ) -> Result<(), ScanError> {
        self.send(Event::Forecast(forecast_lines(found, verification)));
        Ok(())
    }

    fn confirm_incomplete(&mut self, unreadable: usize, total: usize) -> Result<bool, ScanError> {
        self.send(Event::ConfirmIncomplete { unreadable, total });
        Ok(matches!(self.answers.recv(), Ok(Answer::Confirm(true))))
    }

    fn exporting(&mut self, done: usize, total: usize) {
        self.status(format!("Exporting definitions… {done}/{total}"));
    }

    fn writing(&mut self) {
        self.status("Writing the project…");
    }
}

/// The CLI's forecast block, as lines.
fn forecast_lines(found: &Downstream, verification: Option<&Verification>) -> Vec<(Tone, String)> {
    let mut lines = Vec::new();
    let copies = match found.app_copies.len() {
        0 => String::new(),
        n => format!(" (+{} skipped)", count(n, "app copy", "app copies")),
    };
    lines.push((
        Tone::Plain,
        format!(
            "{} · {} · {}{copies}",
            found.model.name,
            found.model.workspace_name,
            count(
                found.reports.len(),
                "downstream report",
                "downstream reports"
            )
        ),
    ));
    let readable: Vec<(&str, &str)> = found
        .reports
        .iter()
        .filter(|report| report.access == Access::Downloadable)
        .map(|report| (report.workspace_name.as_str(), ""))
        .collect();
    if !readable.is_empty() {
        lines.push((
            Tone::Good,
            format!(
                "✔ {} downloadable: {}",
                readable.len(),
                per_workspace(&readable)
            ),
        ));
    }
    let unreadable: Vec<(&str, &str)> = found
        .unreadable()
        .map(|report| {
            let why = match report.access {
                Access::Viewer => "viewer",
                _ => "no access",
            };
            (report.workspace_name.as_str(), why)
        })
        .collect();
    if !unreadable.is_empty() {
        lines.push((
            Tone::Bad,
            format!(
                "✖ {} not readable: {}",
                unreadable.len(),
                per_workspace(&unreadable)
            ),
        ));
    }
    if !found.other_items.is_empty() {
        let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
        for item in &found.other_items {
            *kinds.entry(item.kind.as_str()).or_default() += 1;
        }
        let kinds: Vec<String> = kinds
            .iter()
            .map(|(kind, n)| format!("{n} {kind}"))
            .collect();
        lines.push((
            Tone::Plain,
            format!(
                "{} not scanned: {}",
                count(
                    found.other_items.len(),
                    "other downstream item",
                    "other downstream items"
                ),
                kinds.join(", ")
            ),
        ));
    }
    for note in &found.notes {
        lines.push((Tone::Warn, note.clone()));
    }
    if let Some(verification) = verification {
        if verification.only_in_listing.is_empty() && verification.only_in_downstream.is_empty() {
            lines.push((
                Tone::Plain,
                format!(
                    "Verify: {} agree with the downstream list",
                    count(verification.workspaces_checked, "workspace", "workspaces")
                ),
            ));
        }
        for item in &verification.only_in_listing {
            lines.push((
                Tone::Warn,
                format!(
                    "Verify: '{}' in '{}' is bound to the model but missing downstream; it is not fetched",
                    item.name, item.workspace_name
                ),
            ));
        }
        for item in &verification.only_in_downstream {
            lines.push((
                Tone::Warn,
                format!(
                    "Verify: '{}' in '{}' is downstream but missing from its workspace listing",
                    item.name, item.workspace_name
                ),
            ));
        }
    }
    if found.model_access != Access::Downloadable {
        lines.push((
            Tone::Bad,
            format!(
                "The model cannot be downloaded: {}",
                fetch::access_phrase(found.model_access)
            ),
        ));
    } else if !unreadable.is_empty() {
        lines.push((
            Tone::Warn,
            format!(
                "Analysis will be incomplete: {} can't be read.",
                count(unreadable.len(), "report", "reports")
            ),
        ));
    }
    lines
}

/// `Marketing (9), HR (7)` — busiest first; a reason rides along when given.
fn per_workspace(entries: &[(&str, &str)]) -> String {
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for entry in entries {
        *counts.entry(*entry).or_default() += 1;
    }
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
        .iter()
        .map(|((workspace, why), n)| {
            let workspace = if workspace.is_empty() {
                "(unnamed workspace)"
            } else {
                workspace
            };
            if why.is_empty() {
                format!("{workspace} ({n})")
            } else {
                format!("{workspace} ({n}, {why})")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_workspace_orders_by_count_then_name() {
        let entries = [
            ("HR", ""),
            ("Marketing", ""),
            ("Marketing", ""),
            ("Finance", ""),
        ];
        assert_eq!(
            per_workspace(&entries),
            "Marketing (2), Finance (1), HR (1)"
        );
    }
}
