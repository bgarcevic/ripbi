//! `ripbi stub-report` (issue #130): write a minimal report bound to a
//! model-only `.SemanticModel`, so Power BI Desktop can open the project,
//! refresh it, and save `.pbi/cache.abf` — the storage source `scan` picks up
//! automatically (issue #129). The file contents come from
//! [`ripbi_core::report_stub`]; this module resolves the model, picks the
//! output folder (a fresh temp folder by default, `--out` otherwise), writes,
//! opens the `.pbip` in Desktop when the run is interactive, and with `--wait`
//! watches for Desktop's save.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use ripbi_core::{ingest, report_stub};

use crate::cli::StubReportArgs;
use crate::error::ScanError;
use crate::render::format_bytes;
use crate::scan::{EXIT_CLEAN, EXIT_ERROR, Streams};
use crate::style::Palette;

const MODEL_SUFFIX: &str = ".SemanticModel";

/// How often `--wait` checks the cache file.
const POLL: Duration = Duration::from_secs(1);

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
/// Desktop is only launched when stdin and stderr are terminals, so in-memory
/// test streams never open anything.
#[must_use = "the return value is the process exit code"]
pub fn run_in(args: &StubReportArgs, cwd: &Path, streams: &mut Streams<'_>) -> i32 {
    let palette = Palette::detect(streams.stderr_is_tty, args.no_color);
    match stub(args, cwd, streams) {
        Ok(()) => EXIT_CLEAN,
        Err(error) => {
            if !args.quiet {
                let _ = writeln!(streams.err, "{} {}", palette.alert("error:"), error.message);
                if let Some(hint) = &error.hint {
                    let _ = writeln!(streams.err, "hint: {hint}");
                }
            }
            EXIT_ERROR
        }
    }
}

fn stub(args: &StubReportArgs, cwd: &Path, streams: &mut Streams<'_>) -> Result<(), ScanError> {
    let model = normalize(&match &args.path {
        Some(path) => model_folder(&cwd.join(path))?,
        None => sole_model_in(cwd)?,
    });
    let folder_name = file_name(&model);
    let stem = &folder_name[..folder_name.len() - MODEL_SUFFIX.len()];
    let name = match &args.name {
        Some(name) => valid_name(name)?,
        None => stem.to_string(),
    };

    let out = match &args.out {
        Some(dir) => {
            let dir = normalize(&cwd.join(dir));
            check_targets(&dir, &name, args.force)?;
            dir
        }
        None => temp_folder(&name)?,
    };
    let report = out.join(format!("{name}.Report"));
    let Some(dataset_path) = relative_path(&report, &model) else {
        if args.out.is_none() {
            let _ = fs::remove_dir_all(&out);
        }
        return Err(ScanError::new(format!(
            "{} and {} are on different drives; a report can only reach its model by a relative path",
            out.display(),
            model.display()
        ))
        .with_hint(format!(
            "write the stub on the model's drive, e.g. --out \"{}\"",
            model.parent().unwrap_or(&model).display()
        )));
    };

    if report.exists() {
        fs::remove_dir_all(&report).map_err(|error| {
            ScanError::new(format!("cannot replace {}: {error}", report.display()))
        })?;
    }
    for file in report_stub(&name, &dataset_path) {
        let path = out.join(&file.path);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, file.contents)
            .map_err(|error| ScanError::new(format!("cannot write {}: {error}", path.display())))?;
    }
    let pbip = out.join(format!("{name}.pbip"));
    let cache = model.join(".pbi").join("cache.abf");
    // The baseline is taken before Desktop can possibly open the stub, so
    // `--wait` only reports a save that happens after this run.
    let baseline = modified(&cache);

    // The path is the result, alone on stdout, so scripts can open it.
    if !args.quiet {
        writeln!(streams.out, "{}", pbip.display())?;
        writeln!(
            streams.err,
            "Created a report stub for {folder_name} (one empty page, no visuals)."
        )?;
        if !model.join("definition.pbism").is_file() {
            writeln!(
                streams.err,
                "Note: {folder_name} has no definition.pbism; Power BI Desktop needs one to open the project."
            )?;
        }
    }
    let opened = should_open(args, streams, std::env::var_os("CI"))
        && match open_in_desktop(&pbip) {
            Ok(()) => true,
            Err(error) => {
                if !args.quiet {
                    writeln!(streams.err, "Note: cannot open {}: {error}", pbip.display())?;
                }
                false
            }
        };
    if !args.quiet {
        let step = if opened {
            format!("Opening {name}.pbip in Power BI Desktop. Refresh, then save")
        } else {
            format!("Open {name}.pbip in Power BI Desktop, refresh, then save")
        };
        writeln!(
            streams.err,
            "{step}: Desktop writes {folder_name}/.pbi/cache.abf, which `ripbi scan` reads storage sizes from."
        )?;
        if args.out.is_some() {
            writeln!(
                streams.err,
                "Tip: .pbi/ is usually gitignored; keep the stub local too by adding \
                 \"{name}.Report/\" and \"{name}.pbip\" to .gitignore."
            )?;
        }
    }

    if args.wait {
        if !args.quiet {
            writeln!(
                streams.err,
                "Waiting for Desktop to save {folder_name}/.pbi/cache.abf (Ctrl-C to stop)…"
            )?;
        }
        let bytes = wait_for_save(&cache, baseline, POLL)?;
        if !args.quiet {
            let typed = args
                .path
                .as_ref()
                .map_or_else(|| folder_name.clone(), |path| path.display().to_string());
            writeln!(
                streams.err,
                "Saved {} ({}).\nNext: ripbi scan --model \"{typed}\"  \
                 (add --report <path> for reports kept elsewhere)",
                cache.display(),
                format_bytes(bytes)
            )?;
            if args.out.is_none() {
                writeln!(
                    streams.err,
                    "The stub in {} can be deleted once Desktop is closed.",
                    out.display()
                )?;
            }
        }
    }
    Ok(())
}

/// A fresh, kept folder under the system temp directory.
fn temp_folder(name: &str) -> Result<PathBuf, ScanError> {
    let prefix: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = tempfile::Builder::new()
        .prefix(&format!("ripbi-stub-{prefix}-"))
        .tempdir()
        .map_err(|error| ScanError::new(format!("cannot create a temp folder: {error}")))?;
    Ok(normalize(&dir.keep()))
}

/// `--out` never overwrites without `--force`, and never replaces a report
/// that is not a ripbi stub. Checked before writing, so a refusal writes nothing.
fn check_targets(out: &Path, name: &str, force: bool) -> Result<(), ScanError> {
    if out.exists() && !out.is_dir() {
        return Err(ScanError::new(format!(
            "--out {} is not a folder",
            out.display()
        )));
    }
    let report = out.join(format!("{name}.Report"));
    if report.exists() {
        if !is_stub(&report) {
            return Err(ScanError::new(format!(
                "{} already exists and is not a ripbi stub",
                report.display()
            ))
            .with_hint("choose another stem with --name; --force only replaces ripbi stubs"));
        }
        if !force {
            return Err(exists_error(&report));
        }
    }
    let pbip = out.join(format!("{name}.pbip"));
    if pbip.exists() && !force {
        return Err(exists_error(&pbip));
    }
    Ok(())
}

/// Desktop opens only in an interactive run: never under `--no-open` or
/// `--quiet`, off a terminal, in CI, or where Desktop cannot run.
fn should_open(args: &StubReportArgs, streams: &Streams<'_>, ci: Option<OsString>) -> bool {
    cfg!(windows)
        && !args.no_open
        && !args.quiet
        && streams.stdin_is_tty
        && streams.stderr_is_tty
        && ci.is_none_or(|value| value.is_empty())
}

/// Hands the `.pbip` to its registered application (Power BI Desktop).
#[cfg(windows)]
fn open_in_desktop(pbip: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    // `start` resolves the file association; the empty title keeps it from
    // reading the quoted path as a window title. Windows paths never contain
    // `"`, so quoting the path raw is safe.
    Command::new("cmd")
        .raw_arg(format!("/C start \"\" \"{}\"", pbip.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other("no application is registered for .pbip"))
            }
        })
}

#[cfg(not(windows))]
fn open_in_desktop(_pbip: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Power BI Desktop runs only on Windows",
    ))
}

/// Blocks until `cache` is written after `baseline` (or first appears) and
/// its size holds steady across two checks — Desktop writes it over several
/// seconds. Returns the final size.
fn wait_for_save(cache: &Path, baseline: Option<SystemTime>, poll: Duration) -> io::Result<u64> {
    let mut last_size = None;
    loop {
        std::thread::sleep(poll);
        let Ok(meta) = fs::metadata(cache) else {
            continue;
        };
        let changed = match (baseline, meta.modified().ok()) {
            (None, _) => true,
            (Some(before), Some(now)) => now > before,
            (Some(_), None) => false,
        };
        if !changed || meta.len() == 0 {
            continue;
        }
        if last_size == Some(meta.len()) {
            return Ok(meta.len());
        }
        last_size = Some(meta.len());
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
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

/// An absolute path in its long, canonical form when it exists (Windows' temp
/// folder is often an 8.3 short name like `BORISK~1`, which would make the
/// relative `byPath` climb needlessly), else lexically normalized.
fn normalize(path: &Path) -> PathBuf {
    match fs::canonicalize(path) {
        Ok(canonical) => strip_verbatim(canonical),
        Err(_) => lexical(path),
    }
}

/// Drops Windows' `\\?\` prefix from a drive path (`\\?\C:\…` → `C:\…`), the
/// form users type and Desktop reads; UNC and other verbatim paths are kept.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// An absolute, lexically normalized path: `.` dropped, `..` applied.
fn lexical(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `to` relative to the folder `from`, with forward slashes — the form PBIR
/// writes in `byPath`. `None` when no relative path exists (another drive).
/// Both paths must be absolute and normalized.
fn relative_path(from: &Path, to: &Path) -> Option<String> {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let same = |a: &Component, b: &Component| {
        if cfg!(windows) {
            a.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
        } else {
            a == b
        }
    };
    if !matches!((from.first(), to.first()), (Some(a), Some(b)) if same(a, b)) {
        return None;
    }
    let common = from.iter().zip(&to).take_while(|(a, b)| same(a, b)).count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];
    parts.extend(
        to[common..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streams_tty<'a>(
        out: &'a mut Vec<u8>,
        err: &'a mut Vec<u8>,
        input: &'a mut &'static [u8],
        tty: bool,
    ) -> Streams<'a> {
        Streams {
            out,
            err,
            input,
            stdin_is_tty: tty,
            stdout_is_tty: tty,
            stderr_is_tty: tty,
        }
    }

    #[test]
    fn relative_paths_climb_to_the_common_ancestor() {
        let root = normalize(Path::new("/"));
        let report = root.join("tmp/ripbi-stub-x/Sales.Report");
        let model = root.join("repos/bi/Sales.SemanticModel");
        assert_eq!(
            relative_path(&report, &model).as_deref(),
            Some("../../../repos/bi/Sales.SemanticModel")
        );
        let sibling = root.join("repos/bi/Sales.Report");
        assert_eq!(
            relative_path(&sibling, &model).as_deref(),
            Some("../Sales.SemanticModel")
        );
    }

    #[cfg(windows)]
    #[test]
    fn relative_paths_need_one_drive_and_ignore_case() {
        let model = Path::new(r"D:\bi\Sales.SemanticModel");
        assert_eq!(
            relative_path(Path::new(r"C:\tmp\Sales.Report"), model),
            None
        );
        assert_eq!(
            relative_path(Path::new(r"d:\BI\Sales.Report"), model).as_deref(),
            Some("../Sales.SemanticModel")
        );
    }

    #[test]
    fn normalization_applies_dot_segments() {
        let root = normalize(Path::new("/"));
        assert_eq!(lexical(&root.join("a/./b/../c")), root.join("a/c"));
    }

    #[test]
    fn verbatim_drive_prefixes_are_dropped() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\Users\x")),
            PathBuf::from(r"C:\Users\x")
        );
        let unc = PathBuf::from(r"\\?\UNC\server\share");
        assert_eq!(strip_verbatim(unc.clone()), unc);
    }

    #[test]
    fn opens_only_in_an_interactive_run_outside_ci() {
        let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
        let tty = streams_tty(&mut out, &mut err, &mut input, true);
        let args = StubReportArgs::default();
        assert_eq!(should_open(&args, &tty, None), cfg!(windows));
        assert!(!should_open(&args, &tty, Some("true".into())));
        let no_open = StubReportArgs {
            no_open: true,
            ..StubReportArgs::default()
        };
        assert!(!should_open(&no_open, &tty, None));
        let quiet = StubReportArgs {
            quiet: true,
            ..StubReportArgs::default()
        };
        assert!(!should_open(&quiet, &tty, None));

        let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
        let piped = streams_tty(&mut out, &mut err, &mut input, false);
        assert!(!should_open(&args, &piped, None));
    }

    #[test]
    fn waiting_returns_once_a_new_save_settles() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache.abf");
        fs::write(&cache, b"old").unwrap();
        let baseline = modified(&cache);
        let writer = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(60));
                fs::write(&cache, b"the whole cache").unwrap();
            })
        };
        let bytes = wait_for_save(&cache, baseline, Duration::from_millis(50)).unwrap();
        writer.join().unwrap();
        assert_eq!(bytes, b"the whole cache".len() as u64);
    }

    #[test]
    fn waiting_accepts_a_cache_that_did_not_exist_before() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache.abf");
        let writer = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                fs::write(&cache, b"fresh").unwrap();
            })
        };
        assert_eq!(
            wait_for_save(&cache, None, Duration::from_millis(20)).unwrap(),
            5
        );
        writer.join().unwrap();
    }
}
