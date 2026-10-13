"""Synchronize release metadata from Cargo.toml's workspace.package.version.

Uses only Python 3.11+ standard library. Checks are read-only; --write updates
known local packages without resolving or changing third-party dependencies;
--set X.Y.Z changes the workspace version first (the release PR's bump).
"""

import argparse
import json
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
VERSION_PATTERN = r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?"


def replace_once(text: str, pattern: str, version: str) -> str:
    result, count = re.subn(pattern, lambda m: m[1] + version + m[2], text, flags=re.M)
    if count != 1:
        raise ValueError(f"Expected exactly one version field matching {pattern!r}")
    return result


def set_workspace_version(root: Path, version: str) -> None:
    """Rewrite [workspace.package].version, the source --write syncs from."""
    if not re.fullmatch(VERSION_PATTERN, version):
        raise ValueError(f"Invalid version: {version}")
    manifest_path = root / "Cargo.toml"
    manifest = manifest_path.read_text(encoding="utf-8")
    manifest = replace_once(
        manifest, r'(\[workspace\.package\]\n(?:[^\[\n][^\n]*\n|\n)*?version\s*=\s*")[^"]+(")', version
    )
    manifest_path.write_text(manifest, encoding="utf-8", newline="\n")


def planned_updates(root: Path, tag: str | None = None) -> tuple[str, dict[Path, str]]:
    manifest_path = root / "Cargo.toml"
    manifest = manifest_path.read_text(encoding="utf-8")
    version = tomllib.loads(manifest)["workspace"]["package"]["version"]
    if not re.fullmatch(VERSION_PATTERN, version):
        raise ValueError(f"Invalid workspace version: {version}")
    if tag is not None and tag != f"v{version}":
        raise ValueError(f"Tag {tag!r} must match workspace version v{version}")

    updates = {}

    def remember(path: Path, old: str, new: str) -> None:
        if old != new:
            updates[path] = new

    synced = manifest
    for name in ("ripbi-core", "ripbi-xpress9"):
        synced = replace_once(
            synced, rf'^({re.escape(name)}\s*=\s*\{{[^\n]*version\s*=\s*")[^"]+("[^\n]*\}})$', version
        )
    remember(manifest_path, manifest, synced)

    # The pinned-install examples (sh and PowerShell) name the latest release.
    install_doc = root / "book/manual/installation.md"
    old = install_doc.read_text(encoding="utf-8")
    new, count = re.subn(
        r"(RIPBI_VERSION\s*=\s*'?v)[0-9][^\s']*", lambda m: m[1] + version, old
    )
    if count != 2:
        raise ValueError(f"Expected two RIPBI_VERSION pins in {install_doc.relative_to(root)}")
    remember(install_doc, old, new)

    desktop_manifest = root / "desktop/src-tauri/Cargo.toml"
    old = desktop_manifest.read_text(encoding="utf-8")
    remember(desktop_manifest, old, replace_once(old, r'^(version\s*=\s*")[^"]+("\s*)$', version))

    for relative, names in (
        ("Cargo.lock", ("ripbi", "ripbi-core", "ripbi-xpress9")),
        ("desktop/src-tauri/Cargo.lock", ("ripbi", "ripbi-core", "ripbi-xpress9", "ripbi-desktop")),
    ):
        path = root / relative
        old = path.read_text(encoding="utf-8")
        new = old
        packages = tomllib.loads(old)["package"]
        for name in names:
            matching = [p for p in packages if p["name"] == name]
            if len(matching) != 1 or "source" in matching[0]:
                raise ValueError(f"Expected one local {name} package in {relative}")
            new = replace_once(
                new, rf'(\[\[package\]\]\nname = "{re.escape(name)}"\nversion = ")[^"]+("\n)', version
            )
        remember(path, old, new)

    # The JSON files are edited as text so their hand formatting (tauri.conf.json's
    # inline arrays) survives; the parse afterwards proves the right fields moved.
    for relative, fields in (
        ("desktop/package.json", 1),
        ("desktop/package-lock.json", 2),
        ("desktop/src-tauri/tauri.conf.json", 1),
    ):
        path = root / relative
        old = path.read_text(encoding="utf-8")
        expected = json.loads(old)
        expected["version"] = version
        if fields == 2:
            expected["packages"][""]["version"] = version
        new = re.sub(
            r'("version"\s*:\s*")[^"]*(")', lambda m: m[1] + version + m[2], old, count=fields
        )
        if json.loads(new) != expected:
            raise ValueError(f"Expected the first {fields} version field(s) in {relative} to be the local package's")
        remember(path, old, new)
    return version, updates


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="Fail on version drift without writing")
    mode.add_argument("--write", action="store_true", help="Synchronize metadata with the workspace version")
    mode.add_argument("--set", metavar="VERSION", help="Set the workspace version, then synchronize (as --write)")
    parser.add_argument("--tag", help="Also require this exact release tag (e.g. v0.4.1)")
    args = parser.parse_args()
    try:
        if args.set:
            set_workspace_version(ROOT, args.set)
        version, updates = planned_updates(ROOT, args.tag)
        if updates and args.check:
            print("Version drift; run python scripts/sync_version.py --write:", file=sys.stderr)
            for path in updates:
                print(f"  {path.relative_to(ROOT)}", file=sys.stderr)
            return 1
        for path, contents in updates.items():
            path.write_text(contents, encoding="utf-8", newline="\n")
        print(f"Release metadata synchronized at {version} ({len(updates)} files updated).")
        return 0
    except (OSError, ValueError, KeyError) as error:
        print(f"Version validation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
