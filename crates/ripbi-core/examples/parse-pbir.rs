//! Eyeball a parsed PBIR report from the terminal, until the real CLI (#8)
//! lands. This is a binary target, so printing here is fine — the library
//! itself stays silent and returns its findings as data.
//!
//! Usage:
//!
//! ```text
//! cargo run -p ripbi-core --example parse-pbir -- <folder> [--bindings]
//! ```
//!
//! `<folder>` is a `.Report` item folder or its `definition/` directory. Exit
//! codes: `0` parsed with no skips, `1` parsed with skips, `2` usage error,
//! `3` parse error.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use ripbi_core::ingest::{SkipNotice, report};
use ripbi_core::report::{BindingKind, ReportModel};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(path) = args.iter().find(|arg| !arg.starts_with("--")) else {
        eprintln!("usage: parse-pbir <.Report folder | definition folder> [--bindings]");
        return ExitCode::from(2);
    };
    let dump_bindings = args.iter().any(|arg| arg == "--bindings");

    let ingested = match report(Path::new(path)) {
        Ok(ingested) => ingested,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(3);
        }
    };

    print_summary(&ingested.value);
    print_skips(&ingested.skips);
    if dump_bindings {
        print_bindings(&ingested.value);
    }

    if ingested.skips.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn print_summary(report: &ReportModel) {
    let wells: usize = report
        .pages
        .iter()
        .flat_map(|page| &page.visuals)
        .map(|visual| visual.wells.len())
        .sum();
    let visuals = report
        .pages
        .iter()
        .map(|page| page.visuals.len())
        .sum::<usize>();

    println!("Report: {}", report.name.as_deref().unwrap_or("(unnamed)"));
    println!(
        "  dataset: {}  pages: {}  visuals: {}  wells: {}  filters: {}  bookmarks: {}  report measures: {}",
        match &report.dataset {
            ripbi_core::DatasetReference::ByPath { path } => format!("byPath ({path})"),
            ripbi_core::DatasetReference::ByConnection { connection_string } => {
                format!("byConnection ({connection_string})")
            }
            ripbi_core::DatasetReference::Unresolved => "unresolved".to_string(),
        },
        report.pages.len(),
        visuals,
        wells,
        report.filters.len(),
        report.bookmarks.len(),
        report.measures.len(),
    );
    println!();
    println!("Pages (in folder order):");
    for page in &report.pages {
        println!(
            "  {:30} {:?}{}",
            page.name.as_str(),
            page.display_name.as_deref().unwrap_or("(unnamed)"),
            if page.is_hidden { " [hidden]" } else { "" },
        );
    }
}

fn print_skips(skips: &[SkipNotice]) {
    println!();
    println!("Skips ({}):", skips.len());
    // Details are written to be self-describing, so no kind label is needed.
    for skip in skips {
        let location = skip
            .location
            .as_deref()
            .map_or(String::new(), |location| format!(" {location}"));
        println!("  {}{}: {}", skip.path.display(), location, skip.detail);
    }
}

fn print_bindings(report: &ReportModel) {
    println!();
    println!("Bindings ({}):", report.bindings().len());
    for binding in &report.bindings() {
        let site = match (binding.page, binding.visual, binding.bookmark) {
            (Some(page), Some(visual), bookmark) => {
                format!(
                    "{page} / {visual}{}",
                    bookmark.map_or(String::new(), |bookmark| format!(" [bookmark {bookmark}]"))
                )
            }
            (Some(page), None, bookmark) => format!(
                "{page}{}",
                bookmark.map_or(String::new(), |bookmark| format!(" [bookmark {bookmark}]"))
            ),
            (None, None, Some(bookmark)) => format!("{bookmark} [bookmark]"),
            (None, None, None) => "report".to_string(),
            _ => "report".to_string(),
        };
        let kind = match binding.kind {
            BindingKind::FieldWell { role } => format!("well {role}"),
            BindingKind::Filter => "filter".to_string(),
            BindingKind::Sort => "sort".to_string(),
            BindingKind::Drillthrough => "drillthrough".to_string(),
            BindingKind::ConditionalFormatting => "conditional formatting".to_string(),
        };
        println!("  {:22} {:28} {}", kind, site, binding.target);
    }
}
