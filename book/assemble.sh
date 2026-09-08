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

# Fail loudly if a source moved or was renamed — a silent gap would ship a
# book missing a chapter.
for f in \
  "$MANUAL/SUMMARY.md" \
  "$MANUAL/introduction.md" \
  "$MANUAL/installation.md" \
  CHANGELOG.md \
  crates/ripbi-cli/docs/output.md \
  crates/ripbi-core/docs/graph.md \
  crates/ripbi-core/docs/formats.md \
  crates/ripbi-core/docs/semantic-model.md \
  crates/ripbi-core/docs/report-model.md \
  crates/ripbi-core/docs/dax-lexing.md \
  crates/ripbi-core/docs/name-resolution.md \
; do
  [ -f "$f" ] || { echo "book/assemble.sh: missing $f" >&2; exit 1; }
done

rm -rf "$SRC"
mkdir -p "$SRC"

cp "$MANUAL"/*.md "$SRC"/
cp CHANGELOG.md "$SRC"/changelog.md

# The canonical docs link repo files relatively — the cli doc's golden test
# baselines and the core docs' source definitions ([`BindingRef`] et al).
# The book ships without the repo, so repoint those links at GitHub.
sed "s|](\.\./tests/fixtures/|]($FIXTURES/|g" \
  crates/ripbi-cli/docs/output.md > "$SRC"/output.md

for doc in crates/ripbi-core/docs/*.md; do
  sed -e "s|](\.\./src/|]($CORE_SRC/|g" \
      -e "s|]: \.\./src/|]: $CORE_SRC/|g" \
    "$doc" > "$SRC/$(basename "$doc")"
done

echo "book/assemble.sh: assembled $SRC from the canonical docs"
