"""Synchronize release metadata from Cargo.toml's workspace.package.version.

Uses only Python 3.11+ standard library. Checks are read-only; --write updates
known local packages without resolving or changing third-party dependencies.
"""

import argparse
import json
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def replace_once(text: str, pattern: str, version: str) -> str:
    result, count = re.subn(pattern, lambda m: m[1] + version + m[2], text, flags=re.M)
    if count != 1:
        raise ValueError(f"Expected exactly one version field matching {pattern!r}")
    return result


def planned_updates(root: Path, tag: str | None = None) -> tuple[str, dict[Path, str]]:
    manifest_path = root / "Cargo.toml"
    manifest = manifest_path.read_text(encoding="utf-8")
    version = tomllib.loads(manifest)["workspace"]["package"]["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version):
        raise ValueError(f"Invalid workspace version: {version}")
    if tag is not None and tag != f"v{version}":
        raise ValueError(f"Tag {tag!r} must match workspace version v{version}")

    updates = {}

    def remember(path: Path, old: str, new: str) -> None:
        if old != new:
            updates[path] = new

    synced = replace_once(
        manifest, r'^(ripbi-core\s*=\s*\{[^\n]*version\s*=\s*")[^"]+("[^\n]*\})$', version
    )
    remember(manifest_path, manifest, synced)

    desktop_manifest = root / "desktop/src-tauri/Cargo.toml"
    old = desktop_manifest.read_text(encoding="utf-8")
    remember(desktop_manifest, old, replace_once(old, r'^(version\s*=\s*")[^"]+("\s*)$', version))

    for relative, names in (
        ("Cargo.lock", ("ripbi", "ripbi-core")),
        ("desktop/src-tauri/Cargo.lock", ("ripbi", "ripbi-core", "ripbi-desktop")),
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

    for relative in ("desktop/package.json", "desktop/package-lock.json", "desktop/src-tauri/tauri.conf.json"):
        path = root / relative
        old = path.read_text(encoding="utf-8")
        data = json.loads(old)
        original = json.loads(old)
        data["version"] = version
        if relative.endswith("package-lock.json"):
            data["packages"][""]["version"] = version
        if data != original:
            updates[path] = json.dumps(data, indent=2, ensure_ascii=False) + "\n"
    return version, updates


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="Fail on version drift without writing")
    mode.add_argument("--write", action="store_true", help="Synchronize metadata with the workspace version")
    parser.add_argument("--tag", help="Also require this exact release tag (e.g. v0.4.1)")
    args = parser.parse_args()
    try:
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
