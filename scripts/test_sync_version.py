"""Exercise version bumps in isolated copies of the real release metadata."""

import json
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
            "Cargo.toml", "Cargo.lock", "desktop/src-tauri/Cargo.toml",
            "desktop/src-tauri/Cargo.lock", "desktop/package.json",
            "desktop/package-lock.json", "desktop/src-tauri/tauri.conf.json",
        ):
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, destination)

    def test_bump_synchronizes_every_local_package_and_is_idempotent(self):
        version, _ = planned_updates(self.root)
        manifest = self.root / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace(f'version = "{version}"', 'version = "9.8.7"', 1))
        version, updates = planned_updates(self.root, "v9.8.7")
        self.assertEqual(len(updates), 7)
        for path, content in updates.items():
            path.write_text(content, encoding="utf-8")
        self.assertEqual(planned_updates(self.root), ("9.8.7", {}))
        lock = json.loads((self.root / "desktop/package-lock.json").read_text())
        self.assertEqual(lock["packages"]["node_modules/vite"], json.loads((ROOT / "desktop/package-lock.json").read_text())["packages"]["node_modules/vite"])

    def test_wrong_tag_fails_without_writing(self):
        before = (self.root / "Cargo.toml").read_bytes()
        with self.assertRaisesRegex(ValueError, "must match"):
            planned_updates(self.root, "v999.0.0")
        self.assertEqual((self.root / "Cargo.toml").read_bytes(), before)

    def test_detects_npm_lock_root_drift(self):
        path = self.root / "desktop/package-lock.json"
        data = json.loads(path.read_text())
        data["packages"][""]["version"] = "0.0.0"
        path.write_text(json.dumps(data))
        _, updates = planned_updates(self.root)
        self.assertIn(path, updates)

    def test_check_is_read_only_and_write_repairs_drift(self):
        path = self.root / "desktop/package.json"
        data = json.loads(path.read_text())
        data["version"] = "0.0.0"
        path.write_text(json.dumps(data))
        before = path.read_bytes()
        with patch("sync_version.ROOT", self.root), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            with patch("sys.argv", ["sync_version.py", "--check"]):
                self.assertEqual(main(), 1)
            self.assertEqual(path.read_bytes(), before)
            with patch("sys.argv", ["sync_version.py", "--write"]):
                self.assertEqual(main(), 0)
        self.assertEqual(planned_updates(self.root)[1], {})


if __name__ == "__main__":
    unittest.main()
