"""Regenerate the terminal recordings in docs/media/ from real ripbi runs.

Each recording is an animated SVG: the command is typed, then its real output
(colors included, via CLICOLOR_FORCE) appears. SVG renders on GitHub, in the
docs site, and anywhere an <img> works; it stays sharp at any zoom and diffs
as text. Viewers who prefer reduced motion see the final frame.

Uses only the Python 3.11+ standard library, plus `cargo` and `git`.

    python scripts/record_demos.py              # build release, record all
    python scripts/record_demos.py --binary PATH

Rerun it whenever a recorded command's output changes. Paths are shown with
forward slashes whatever OS records them, and git commits use fixed dates so
the output (and the SVGs) are byte-stable between runs.
"""

from __future__ import annotations

import argparse
import html
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MEDIA = ROOT / "docs" / "media"
SAMPLE = "AdventureWorks Sales"
PBIP = f"samples/{SAMPLE}.pbip"
MODEL = f"samples/{SAMPLE}.SemanticModel"

# --- Rendering ------------------------------------------------------------------

FONT_SIZE = 14
CHAR_W = 8.43  # monospace advance at FONT_SIZE (0.6em)
LINE_H = 20
PAD = 18
TITLE_H = 34
MAX_COLS = 100

TYPE_MS = 38  # per typed character
TYPE_CAP_MS = 1800  # a long command still types in under two seconds
AFTER_TYPE_MS = 450
LINE_MS = 28  # stagger between output lines
AFTER_STEP_MS = 1300
HOLD_MS = 5000  # final frame before the loop restarts

THEME = {
    "bg": "#1c1a17",
    "bar": "#26231f",
    "border": "#3a3630",
    "fg": "#d8d2c8",
    "bold": "#f2ede4",
    "dim": "#8a8378",
    "red": "#ff7b72",
    "green": "#7ee787",
    "yellow": "#e3b341",
    "prompt": "#8a9a5b",
}
SGR_COLORS = {"31": "red", "32": "green", "33": "yellow"}
ANSI = re.compile(r"\x1b\[([0-9;]*)m")


@dataclass
class Span:
    text: str
    color: str | None = None
    bold: bool = False
    dim: bool = False


@dataclass
class Step:
    display: str  # the command as shown after the prompt
    output: list[list[Span]] = field(default_factory=list)


def parse_ansi(text: str) -> list[list[Span]]:
    """Splits colored output into lines of styled spans, like a terminal would."""
    lines: list[list[Span]] = [[]]
    color, bold, dim = None, False, False
    pos = 0
    for match in [*ANSI.finditer(text), None]:
        end = match.start() if match else len(text)
        chunk = text[pos:end]
        for i, part in enumerate(chunk.split("\n")):
            if i:
                lines.append([])
            if part:
                lines[-1].append(Span(part, color, bold, dim))
        if match is None:
            break
        codes = (match.group(1) or "0").split(";")
        i = 0
        while i < len(codes):
            code = codes[i]
            if code in ("", "0"):
                color, bold, dim = None, False, False
            elif code == "1":
                bold = True
            elif code == "2":
                dim = True
            elif code in SGR_COLORS:
                color = SGR_COLORS[code]
            elif code == "38" and codes[i + 1 : i + 2] == ["2"]:
                r, g, b = (int(v) for v in codes[i + 2 : i + 5])
                color = f"#{r:02x}{g:02x}{b:02x}"
                i += 4
            elif code == "38" and codes[i + 1 : i + 2] == ["5"]:
                color = xterm_hex(int(codes[i + 2]))
                i += 2
            i += 1
        pos = match.end()
    while lines and not lines[-1]:
        lines.pop()
    return [wrapped for line in lines for wrapped in wrap(line)]


def xterm_hex(index: int) -> str:
    """The standard xterm-256 color for an index (cube and grey ramp)."""
    if index >= 232:
        level = 8 + (index - 232) * 10
        return f"#{level:02x}{level:02x}{level:02x}"
    if index >= 16:
        index -= 16
        steps = [0, 95, 135, 175, 215, 255]
        r, g, b = steps[index // 36], steps[index // 6 % 6], steps[index % 6]
        return f"#{r:02x}{g:02x}{b:02x}"
    return THEME["fg"]


def wrap(line: list[Span]) -> list[list[Span]]:
    """Soft-wraps a styled line at MAX_COLS, as a terminal that wide would."""
    out: list[list[Span]] = [[]]
    cols = 0
    for span in line:
        text = span.text
        while text:
            room = MAX_COLS - cols
            if room == 0:
                out.append([])
                cols, room = 0, MAX_COLS
            piece, text = text[:room], text[room:]
            out[-1].append(Span(piece, span.color, span.bold, span.dim))
            cols += len(piece)
    return out


def span_svg(span: Span) -> str:
    if span.color and span.color.startswith("#"):
        fill = span.color
    else:
        fill = THEME[span.color] if span.color else THEME["bold"] if span.bold else THEME["fg"]
    if span.dim and not span.color:
        fill = THEME["dim"]
    attrs = f' fill="{fill}"'
    if span.bold:
        attrs += ' font-weight="700"'
    if span.dim:
        attrs += ' opacity="0.8"'
    return f"<tspan{attrs}>{html.escape(span.text, quote=False)}</tspan>"


def render(title: str, steps: list[Step]) -> str:
    """One animated terminal window: each step types its command, then prints."""
    rows: list[tuple[str, int, int | None]] = []  # (svg text, appear ms, typed chars)
    t = 400
    for step in steps:
        typed = len(step.display)
        per_char = min(TYPE_MS, TYPE_CAP_MS // max(typed, 1))
        prompt = (
            f'<tspan fill="{THEME["prompt"]}">$ </tspan>'
            f'<tspan fill="{THEME["bold"]}">{html.escape(step.display, quote=False)}</tspan>'
        )
        rows.append((prompt, t, typed))
        t += typed * per_char + AFTER_TYPE_MS
        for line in step.output:
            rows.append(("".join(span_svg(span) for span in line), t, None))
            t += LINE_MS
        t += AFTER_STEP_MS
    total = t + HOLD_MS

    cols = max(
        [len(step.display) + 2 for step in steps]
        + [sum(len(span.text) for span in line) for step in steps for line in step.output]
    )
    width = round(PAD * 2 + min(cols, MAX_COLS) * CHAR_W)
    height = TITLE_H + PAD * 2 + len(rows) * LINE_H

    def pct(ms: float) -> str:
        return f"{ms / total * 100:.3f}%"

    styles, body = [], []
    for i, (text, appear, typed) in enumerate(rows):
        y = TITLE_H + PAD + (i + 1) * LINE_H - 5
        styles.append(
            f"@keyframes a{i}{{0%,{pct(appear)}{{opacity:0}}"
            f"{pct(appear + 1)},100%{{opacity:1}}}}"
            f".a{i}{{animation:a{i} {total}ms linear infinite both}}"
        )
        body.append(f'<text class="r a{i}" x="{PAD}" y="{y}">{text}</text>')
        if typed:
            # A background-colored cover slides off the command one character
            # at a time: the typing effect, without per-character elements.
            per_char = min(TYPE_MS, TYPE_CAP_MS // typed)
            start = appear
            end = appear + typed * per_char
            shift = round(typed * CHAR_W, 2)
            left = round(PAD + 2 * CHAR_W, 2)
            styles.append(
                f"@keyframes c{i}{{0%,{pct(start)}{{transform:translateX(0);"
                f"animation-timing-function:steps({typed},end)}}"
                f"{pct(end)},100%{{transform:translateX({shift}px)}}}}"
                f".c{i}{{animation:c{i} {total}ms linear infinite both}}"
            )
            body.append(
                f'<rect class="c{i}" x="{left}" y="{y - LINE_H + 5}" '
                f'width="{shift + CHAR_W:.2f}" height="{LINE_H}" fill="{THEME["bg"]}"/>'
            )

    dots = "".join(
        f'<circle cx="{PAD + i * 20}" cy="{TITLE_H / 2}" r="6" fill="{c}"/>'
        for i, c in enumerate(("#ff5f56", "#ffbd2e", "#27c93f"))
    )
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="{html.escape(title)}">
<title>{html.escape(title)}</title>
<style>
.r{{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace;font-size:{FONT_SIZE}px;white-space:pre;fill:{THEME["fg"]}}}
{"".join(styles)}
@media (prefers-reduced-motion:reduce){{[class]{{animation:none!important}}rect[class]{{display:none}}}}
</style>
<rect width="{width}" height="{height}" rx="8" fill="{THEME["bg"]}" stroke="{THEME["border"]}"/>
<path d="M0 8a8 8 0 0 1 8-8h{width - 16}a8 8 0 0 1 8 8v{TITLE_H - 8}H0z" fill="{THEME["bar"]}"/>
{dots}
<text x="{width / 2}" y="{TITLE_H / 2 + 4}" text-anchor="middle" font-family="system-ui,sans-serif" font-size="12" fill="{THEME["dim"]}">{html.escape(title)}</text>
{"".join(body)}
</svg>
"""


# --- Recording --------------------------------------------------------------------

GIT_ENV = {
    "GIT_AUTHOR_NAME": "ripbi",
    "GIT_AUTHOR_EMAIL": "demo@ripbi.invalid",
    "GIT_COMMITTER_NAME": "ripbi",
    "GIT_COMMITTER_EMAIL": "demo@ripbi.invalid",
    "GIT_AUTHOR_DATE": "2026-01-01T12:00:00Z",
    "GIT_COMMITTER_DATE": "2026-01-01T12:00:00Z",
}


class Recorder:
    def __init__(self, binary: Path, workdir: Path):
        self.binary = binary
        self.workdir = workdir
        self.env = {
            **os.environ,
            **GIT_ENV,
            "CLICOLOR_FORCE": "1",
            # The palette's exact 24-bit inks, not the xterm-256 fallback.
            "COLORTERM": "truecolor",
            "RIPBI_NO_UPDATE_CHECK": "1",
        }
        self.env.pop("NO_COLOR", None)

    def run(self, display: str, argv: list[str], cwd: Path) -> Step:
        """Runs one command for real; stdout and stderr interleave as on a TTY."""
        if argv[0] == "rib":
            argv = [str(self.binary), *argv[1:]]
        done = subprocess.run(
            argv,
            cwd=cwd,
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
        )
        text = done.stdout.replace("\\", "/").replace(str(self.workdir).replace("\\", "/"), "~")
        return Step(display, parse_ansi(text))

    def git(self, *args: str, cwd: Path) -> None:
        subprocess.run(["git", *args], cwd=cwd, env=self.env, check=True, capture_output=True)


def sample_repo(recorder: Recorder, name: str) -> Path:
    """A throwaway git repository holding one sample project on `main`."""
    repo = recorder.workdir / name
    samples = repo / "samples"
    samples.mkdir(parents=True)
    for item in (ROOT / "samples").iterdir():
        if item.name.startswith(SAMPLE):
            target = samples / item.name
            (shutil.copytree if item.is_dir() else shutil.copy2)(item, target)
    recorder.git("init", "-q", "-b", "main", cwd=repo)
    recorder.git("add", "-A", cwd=repo)
    recorder.git("commit", "-q", "-m", "AdventureWorks Sales", cwd=repo)
    return repo


def scan_demo(recorder: Recorder) -> tuple[str, list[Step]]:
    repo = sample_repo(recorder, "scan")
    return "ripbi scan", [
        recorder.run(f'rib scan "{PBIP}" --summary', ["rib", "scan", PBIP, "--summary"], repo),
    ]


def findings_demo(recorder: Recorder) -> tuple[str, list[Step]]:
    repo = sample_repo(recorder, "findings")
    return "ripbi scan --type measure", [
        recorder.run(
            f'rib scan "{PBIP}" --type measure',
            ["rib", "scan", PBIP, "--type", "measure"],
            repo,
        ),
    ]


def compare_demo(recorder: Recorder) -> tuple[str, list[Step]]:
    repo = sample_repo(recorder, "project")
    recorder.git("checkout", "-q", "-b", "add-sales-target", cwd=repo)
    tmdl = repo / MODEL / "definition" / "tables" / "Sales.tmdl"
    text = tmdl.read_text(encoding="utf-8")
    anchor = "\n\tcolumn "
    assert anchor in text, "Sales.tmdl layout changed; update the demo edit"
    text = text.replace(
        anchor,
        "\n\tmeasure 'Sales Target' = [Sales] * 1.1\n\t\tdisplayFolder: Core\n" + anchor,
        1,
    )
    tmdl.write_text(text, encoding="utf-8")
    recorder.git("commit", "-q", "-am", "Add a sales target measure", cwd=repo)
    return "ripbi scan --compare-root", [
        recorder.run("git diff --stat main", ["git", "diff", "--stat", "main"], repo),
        recorder.run(
            "git worktree add ../base main",
            ["git", "worktree", "add", "../base", "main"],
            repo,
        ),
        recorder.run(
            f'rib scan "{PBIP}" --compare-root ../base',
            ["rib", "scan", PBIP, "--compare-root", "../base"],
            repo,
        ),
    ]


def deps_demo(recorder: Recorder) -> tuple[str, list[Step]]:
    repo = sample_repo(recorder, "deps")
    target = "'Sales'[Profit %]"
    return "ripbi deps", [
        recorder.run(
            f'rib deps "{target}" --model "{MODEL}" --impact',
            ["rib", "deps", target, "--model", MODEL, "--impact"],
            repo,
        ),
    ]


DEMOS = {
    "scan": scan_demo,
    "scan-findings": findings_demo,
    "compare-root": compare_demo,
    "deps": deps_demo,
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", type=Path, help="a built rib/ripbi binary")
    parser.add_argument("demos", nargs="*", help=f"any of: {', '.join(DEMOS)} (default: all)")
    args = parser.parse_args()
    unknown = [name for name in args.demos if name not in DEMOS]
    if unknown:
        parser.error(f"unknown demo(s): {', '.join(unknown)}")

    binary = args.binary
    if binary is None:
        subprocess.run(["cargo", "build", "--release", "-q", "-p", "ripbi"], cwd=ROOT, check=True)
        suffix = ".exe" if os.name == "nt" else ""
        binary = ROOT / "target" / "release" / f"rib{suffix}"
    binary = binary.resolve()

    MEDIA.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ripbi-demo-") as tmp:
        recorder = Recorder(binary, Path(tmp))
        for name in args.demos or DEMOS:
            title, steps = DEMOS[name](recorder)
            out = MEDIA / f"{name}.svg"
            out.write_text(render(title, steps), encoding="utf-8", newline="\n")
            print(f"record_demos: wrote {out.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
