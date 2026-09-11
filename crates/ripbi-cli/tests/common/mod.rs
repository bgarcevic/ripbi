//! Helpers shared by the CLI integration tests. Compiled into each test
//! binary via `mod common;` — integration tests cannot see the crate's
//! `pub(crate)` test support.

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_cli::cli::ScanArgs;
use ripbi_cli::scan::{self, Streams};

/// A unique scratch directory per test, removed again best-effort.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ripbi-cli-it-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().expect("has parent")).expect("create parent");
        fs::write(&path, contents).expect("write file");
        path
    }

    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(&path).expect("create dir");
        path
    }

    /// Copies a directory tree from `from` (its contents) into `self.0`.
    pub fn copy_tree(&self, from: &Path) {
        copy_recursive(from, &self.0);
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn copy_recursive(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("read source") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            fs::create_dir_all(&target).expect("create dir");
            copy_recursive(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// The mini PBIP fixture: one model, one report, one dead chain.
pub fn mini_pbip() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mini-pbip")
}

/// Copies the mini fixture into `dir` under `stem` (renaming its items), so
/// tests can build dedicated or multi-project layouts.
pub fn project_into(dir: &Path, stem: &str) {
    let source = mini_pbip();
    for suffix in [".pbip", ".SemanticModel", ".Report"] {
        let renamed = dir.join(format!("{stem}{suffix}"));
        if suffix == ".pbip" {
            fs::copy(source.join(format!("Mini{suffix}")), &renamed).expect("copy pbip");
        } else {
            let item_source = source.join(format!("Mini{suffix}"));
            fs::create_dir_all(&renamed).expect("create item dir");
            copy_recursive(&item_source, &renamed);
        }
    }
}

/// Runs `scan` with in-memory streams and returns (exit code, stdout, stderr).
/// Stdin is non-interactive.
pub fn run_scan(args: &ScanArgs, cwd: &Path, stdin: &str) -> (i32, String, String) {
    run_scan_tty(args, cwd, stdin, false)
}

/// Like [`run_scan`], but states whether stdin is interactive — the picker
/// only prompts when it is.
pub fn run_scan_tty(
    args: &ScanArgs,
    cwd: &Path,
    stdin: &str,
    stdin_is_tty: bool,
) -> (i32, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut input = stdin.as_bytes();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        input: &mut input,
        stdin_is_tty,
        stderr_is_tty: false,
    };
    let code = scan::run_in(args, cwd, &mut streams);
    (
        code,
        String::from_utf8(out).expect("stdout is utf-8"),
        String::from_utf8(err).expect("stderr is utf-8"),
    )
}

/// `scan` with default flags against an explicit PATH.
pub fn scan_path(path: &Path, cwd: &Path) -> (i32, String, String) {
    let args = ScanArgs {
        path: Some(path.to_path_buf()),
        ..ScanArgs::default()
    };
    run_scan(&args, cwd, "")
}
