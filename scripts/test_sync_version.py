"""Exercise version bumps in isolated copies of the real release metadata."""

import contextlib
import io
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

from sync_version import ROOT, main, planned_updates


class VersionSyncTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for relative in (
            "Cargo.toml", "Cargo.lock", "desktop-egui/Cargo.toml",
            "desktop-egui/Cargo.lock", "book/manual/installation.md",
        ):
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, destination)

    def test_bump_synchronizes_every_local_package_and_is_idempotent(self):
        version, _ = planned_updates(self.root)
        manifest = self.root / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace(f'version = "{version}"', 'version = "9.8.7"', 1))
        version, updates = planned_updates(self.root, "v9.8.7")
        self.assertEqual(len(updates), 5)
        for path, content in updates.items():
            path.write_text(content, encoding="utf-8")
        self.assertEqual(planned_updates(self.root), ("9.8.7", {}))
        manifest_text = manifest.read_text()
        self.assertIn('ripbi-core = { path = "crates/ripbi-core", version = "9.8.7" }', manifest_text)
        self.assertIn('ripbi-xpress9 = { path = "crates/ripbi-xpress9", version = "9.8.7" }', manifest_text)
        self.assertIn('ripbi-app = { path = "crates/ripbi-app", version = "9.8.7" }', manifest_text)
        self.assertIn('ripbi-service = { path = "crates/ripbi-service", version = "9.8.7" }', manifest_text)
        install_doc = (self.root / "book/manual/installation.md").read_text(encoding="utf-8")
        self.assertIn("RIPBI_VERSION=v9.8.7 sh", install_doc)
        self.assertIn("$env:RIPBI_VERSION = 'v9.8.7'", install_doc)
        desktop_lock = (self.root / "desktop-egui/Cargo.lock").read_text(encoding="utf-8")
        self.assertIn('name = "ripbi-desktop-egui"\nversion = "9.8.7"', desktop_lock)
        self.assertIn('name = "ripbi-app"\nversion = "9.8.7"', desktop_lock)

    def test_wrong_tag_fails_without_writing(self):
        before = (self.root / "Cargo.toml").read_bytes()
        with self.assertRaisesRegex(ValueError, "must match"):
            planned_updates(self.root, "v999.0.0")
        self.assertEqual((self.root / "Cargo.toml").read_bytes(), before)

    def test_check_is_read_only_and_write_repairs_drift(self):
        version, _ = planned_updates(self.root)
        path = self.root / "Cargo.lock"
        path.write_text(
            path.read_text().replace(f'name = "ripbi-app"\nversion = "{version}"', 'name = "ripbi-app"\nversion = "0.0.0"', 1)
        )
        before = path.read_bytes()
        self.assertIn(path, planned_updates(self.root)[1])
        with patch("sync_version.ROOT", self.root), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            with patch("sys.argv", ["sync_version.py", "--check"]):
                self.assertEqual(main(), 1)
            self.assertEqual(path.read_bytes(), before)
            with patch("sys.argv", ["sync_version.py", "--write"]):
                self.assertEqual(main(), 0)
        self.assertEqual(planned_updates(self.root)[1], {})

    def test_set_bumps_the_workspace_version_and_synchronizes(self):
        with patch("sync_version.ROOT", self.root), contextlib.redirect_stdout(io.StringIO()):
            with patch("sys.argv", ["sync_version.py", "--set", "9.8.7"]):
                self.assertEqual(main(), 0)
        self.assertEqual(planned_updates(self.root, "v9.8.7"), ("9.8.7", {}))
        self.assertIn('name = "ripbi"\nversion = "9.8.7"', (self.root / "Cargo.lock").read_text())

    def test_set_rejects_an_invalid_version_without_writing(self):
        before = (self.root / "Cargo.toml").read_bytes()
        with patch("sync_version.ROOT", self.root), contextlib.redirect_stderr(io.StringIO()):
            with patch("sys.argv", ["sync_version.py", "--set", "v9.8"]):
                self.assertEqual(main(), 1)
        self.assertEqual((self.root / "Cargo.toml").read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
