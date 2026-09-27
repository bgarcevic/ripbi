//! `ripbi stub-report` (issue #130): write a minimal report bound to a
//! model-only `.SemanticModel`, so Power BI Desktop can open the project,
//! refresh it, and save `.pbi/cache.abf` — the storage source `scan` picks up
//! automatically (issue #129). The file contents come from
//! [`ripbi_core::report_stub`]; this module resolves the target, guards
//! against overwriting, writes, and prints the next steps.

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_core::{ingest, report_stub};

use crate::cli::StubReportArgs;
use crate::error::ScanError;
use crate::scan::{EXIT_CLEAN, EXIT_ERROR, Streams};
use crate::style::Palette;

const MODEL_SUFFIX: &str = ".SemanticModel";

/// Runs `stub-report` from the process working directory.
#[must_use = "the return value is the process exit code"]
pub fn run(args: &StubReportArgs, streams: &mut Streams<'_>) -> i32 {
    match std::env::current_dir() {
        Ok(cwd) => run_in(args, &cwd, streams),
        Err(error) => {
            if !args.quiet {
                let _ = writeln!(
                    streams.err,
                    "error: cannot determine the working directory: {error}"
                );
            }
            EXIT_ERROR
        }
    }
}

/// Runs `stub-report` against an explicit working directory (tests pass one).
#[must_use = "the return value is the process exit code"]
pub fn run_in(args: &StubReportArgs, cwd: &Path, streams: &mut Streams<'_>) -> i32 {
    let palette = Palette::detect(streams.stderr_is_tty, args.no_color);
    match stub(args, cwd, streams) {
        Ok(()) => EXIT_CLEAN,
        Err(error) => {
            if !args.quiet {
                let _ = writeln!(streams.err, "{} {}", palette.red("error:"), error.message);
                if let Some(hint) = &error.hint {
                    let _ = writeln!(streams.err, "hint: {hint}");
                }
            }
            EXIT_ERROR
        }
    }
}

fn stub(args: &StubReportArgs, cwd: &Path, streams: &mut Streams<'_>) -> Result<(), ScanError> {
    let model = match &args.path {
        Some(path) => model_folder(&cwd.join(path))?,
        None => sole_model_in(cwd)?,
    };
    let folder_name = file_name(&model);
    let stem = &folder_name[..folder_name.len() - MODEL_SUFFIX.len()];
    let name = match &args.name {
        Some(name) => valid_name(name)?,
        None => stem.to_string(),
    };
    let parent = model.parent().unwrap_or(cwd);
    let report = parent.join(format!("{name}.Report"));
    let pbip = parent.join(format!("{name}.pbip"));

    // Check both targets before touching either, so a refusal writes nothing.
    let replace_report = report.exists();
    if replace_report {
        if !is_stub(&report) {
            return Err(ScanError::new(format!(
                "{} already exists and is not a ripbi stub",
                report.display()
            ))
            .with_hint("choose another stem with --name; --force only replaces ripbi stubs"));
        }
        if !args.force {
            return Err(exists_error(&report));
        }
    }
    if pbip.exists() && !args.force {
        return Err(exists_error(&pbip));
    }

    if replace_report {
        fs::remove_dir_all(&report).map_err(|error| {
            ScanError::new(format!("cannot replace {}: {error}", report.display()))
        })?;
    }
    for file in report_stub(&name, &folder_name) {
        let path = parent.join(&file.path);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, file.contents)
            .map_err(|error| ScanError::new(format!("cannot write {}: {error}", path.display())))?;
    }

    if args.quiet {
        return Ok(());
    }
    writeln!(
        streams.out,
        "Created {name}.pbip and {name}.Report (one empty page, no visuals) beside {folder_name}."
    )?;
    if !model.join("definition.pbism").is_file() {
        writeln!(
            streams.err,
            "Note: {folder_name} has no definition.pbism; Power BI Desktop needs one to open the project."
        )?;
    }
    writeln!(
        streams.err,
        "Next:\n  \
         1. Open {name}.pbip in Power BI Desktop and refresh.\n  \
         2. Save. Desktop writes {folder_name}/.pbi/cache.abf.\n  \
         3. ripbi scan --model \"{folder_name}\" --report <reports> reads storage sizes from it.\n\
         Tip: .pbi/ is usually gitignored; keep the stub local too by adding \
         \"{name}.Report/\" and \"{name}.pbip\" to .gitignore."
    )?;
    Ok(())
}

/// Resolves PATH to a `.SemanticModel` folder, accepting its `definition/`
/// child as a convenience.
fn model_folder(path: &Path) -> Result<PathBuf, ScanError> {
    if !path.exists() {
        return Err(ScanError::new(format!("{} does not exist", path.display())));
    }
    let candidate = if is_model_folder(path) {
        Some(path)
    } else {
        path.parent()
            .filter(|parent| is_definition(path) && is_model_folder(parent))
    };
    candidate.map(Path::to_path_buf).ok_or_else(|| {
        ScanError::new(format!("{} is not a .SemanticModel folder", path.display())).with_hint(
            "stub-report binds a PBIP semantic-model folder (e.g. Sales.SemanticModel); \
             PBIX, PBIT, and model.bim inputs need no stub",
        )
    })
}

/// The one `.SemanticModel` folder directly under `cwd`.
fn sole_model_in(cwd: &Path) -> Result<PathBuf, ScanError> {
    let mut models: Vec<PathBuf> = fs::read_dir(cwd)?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| is_model_folder(path))
        .collect();
    models.sort();
    match models.len() {
        1 => Ok(models.remove(0)),
        0 => Err(
            ScanError::new(format!("no .SemanticModel folder in {}", cwd.display()))
                .with_hint("pass one: ripbi stub-report <path/to/Name.SemanticModel>"),
        ),
        _ => {
            let names: Vec<String> = models.iter().map(|path| file_name(path)).collect();
            Err(ScanError::new(format!(
                "{} .SemanticModel folders in {}: {}",
                models.len(),
                cwd.display(),
                names.join(", ")
            ))
            .with_hint("pass the one to stub: ripbi stub-report <Name.SemanticModel>"))
        }
    }
}

fn is_model_folder(path: &Path) -> bool {
    let name = file_name(path);
    path.is_dir()
        && name.len() > MODEL_SUFFIX.len()
        && name
            .to_ascii_lowercase()
            .ends_with(&MODEL_SUFFIX.to_ascii_lowercase())
}

fn is_definition(path: &Path) -> bool {
    path.is_dir() && file_name(path).eq_ignore_ascii_case("definition")
}

/// Whether the existing report folder is one `stub-report` wrote.
fn is_stub(report: &Path) -> bool {
    ingest::report(report).is_ok_and(|ingested| ingested.value.stub)
}

/// `--name` becomes a file stem: no separators or characters Windows rejects.
fn valid_name(name: &str) -> Result<String, ScanError> {
    let trimmed = name.trim();
    let bad = |c: char| {
        matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
    };
    if trimmed.is_empty() || trimmed.contains(bad) || trimmed.ends_with('.') {
        return Err(ScanError::new(format!("invalid --name '{name}'"))
            .with_hint("use a plain file stem, e.g. --name Sales-stats"));
    }
    Ok(trimmed.to_string())
}

fn exists_error(path: &Path) -> ScanError {
    ScanError::new(format!("{} already exists", path.display()))
        .with_hint("pass --force to replace it, or --name to write a stub under another stem")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
