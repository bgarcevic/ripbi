#!/bin/sh
# Assembles book/src, the mdBook source tree, as a projection of the canonical
# docs that live next to the code. The canonical docs never move — AGENTS.md
# routes to them where they are — so the book is built by copying them here.
# book/src is generated: never edit or commit it.
#
# Run from anywhere:  book/assemble.sh

set -eu

cd "$(dirname "$0")/.."

MANUAL=book/manual
SRC=book/src
FIXTURES="https://github.com/bgarcevic/ripbi/blob/main/crates/ripbi-cli/tests/fixtures"
CORE_SRC="https://github.com/bgarcevic/ripbi/blob/main/crates/ripbi-core/src"

SITE='https://bgarcevic\.github\.io/ripbi/'

# Fail loudly if a source moved or was renamed — a silent gap would ship a
# book missing a chapter.
for f in \
  "$MANUAL/SUMMARY.md" \
  "$MANUAL/introduction.md" \
  "$MANUAL/installation.md" \
  "$MANUAL/ci.md" \
  "$MANUAL/cleanup.md" \
  "$MANUAL/unused.md" \
  "$MANUAL/inputs.md" \
  CHANGELOG.md \
  docs/media/ripbi-mark.svg \
  desktop-egui/assets/ripbi.png \
  crates/ripbi-cli/docs/scan/overview.md \
  crates/ripbi-cli/docs/scan/flags.md \
  crates/ripbi-cli/docs/scan/human-output.md \
  crates/ripbi-cli/docs/scan/text-formats.md \
  crates/ripbi-cli/docs/scan/json.md \
  crates/ripbi-cli/docs/scan/ci-formats.md \
  crates/ripbi-cli/docs/scan/boundaries.md \
  crates/ripbi-cli/docs/scan/compare.md \
  crates/ripbi-cli/docs/scan/keep.md \
  crates/ripbi-cli/docs/scan/queries.md \
  crates/ripbi-cli/docs/scan/storage.md \
  crates/ripbi-cli/docs/scan/tx-rm.md \
  crates/ripbi-cli/docs/deps.md \
  crates/ripbi-cli/docs/report.md \
  crates/ripbi-cli/docs/fetch.md \
  crates/ripbi-cli/docs/config.md \
  crates/ripbi-core/docs/graph.md \
  crates/ripbi-core/docs/formats.md \
  crates/ripbi-core/docs/semantic-model.md \
  crates/ripbi-core/docs/report-model.md \
  crates/ripbi-core/docs/dax-lexing.md \
  crates/ripbi-core/docs/m-lexing.md \
  crates/ripbi-core/docs/name-resolution.md \
; do
  [ -f "$f" ] || { echo "book/assemble.sh: missing $f" >&2; exit 1; }
done

rm -rf "$SRC"
mkdir -p "$SRC/scan"

cp "$MANUAL"/*.md "$SRC"/
# Terminal recordings (scripts/record_demos.py). The canonical docs reach them
# at ../../../docs/media/; the book serves them from media/.
cp -R docs/media "$SRC"/media
cp CHANGELOG.md "$SRC"/changelog.md

# The brand mark as the site favicon. mdBook reads it from book/theme/, which
# is generated like book/src.
mkdir -p book/theme
cp docs/media/ripbi-mark.svg book/theme/favicon.svg
cp desktop-egui/assets/ripbi.png book/theme/favicon.png

# The canonical docs link repo files relatively — the cli docs' golden test
# baselines and the core docs' source definitions ([`BindingRef`] et al).
# The book ships without the repo, so repoint those links at GitHub. Links
# across crates are absolute site URLs, since no relative path works from both
# crates; in the book they become relative again, so the link check covers them.
for doc in crates/ripbi-cli/docs/*.md; do
  sed -e "s|](\.\./tests/fixtures/|]($FIXTURES/|g" \
      -e "s|](\.\./\.\./\.\./docs/media/|](media/|g" \
      -e "s|]($SITE\([^)#]*\)\.html|](\1.md|g" \
    "$doc" > "$SRC/$(basename "$doc")"
done

for doc in crates/ripbi-cli/docs/scan/*.md; do
  sed -e "s|](\.\./\.\./tests/fixtures/|]($FIXTURES/|g" \
      -e "s|](\.\./\.\./\.\./\.\./docs/media/|](../media/|g" \
      -e "s|]($SITE\([^)#]*\)\.html|](../\1.md|g" \
    "$doc" > "$SRC/scan/$(basename "$doc")"
done

for doc in crates/ripbi-core/docs/*.md; do
  sed -e "s|](\.\./src/|]($CORE_SRC/|g" \
      -e "s|]: \.\./src/|]: $CORE_SRC/|g" \
      -e "s|]($SITE\([^)#]*\)\.html|](\1.md|g" \
    "$doc" > "$SRC/$(basename "$doc")"
done

echo "book/assemble.sh: assembled $SRC from the canonical docs"
