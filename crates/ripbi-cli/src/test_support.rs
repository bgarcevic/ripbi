//! Shared test helpers, compiled only for tests.

use std::fs;
use std::path::PathBuf;

/// A unique scratch directory per test, removed again on drop (best-effort).
pub(crate) struct TempDir(pub(crate) PathBuf);

impl TempDir {
    /// Creates `<tmp>/ripbi-cli-<pid>-<name>`.
    pub(crate) fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ripbi-cli-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    /// Writes a file under the directory, creating parents.
    pub(crate) fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().expect("has parent")).expect("create parent");
        fs::write(&path, contents).expect("write file");
        path
    }

    /// Creates a directory under the directory, including parents.
    pub(crate) fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(&path).expect("create dir");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
