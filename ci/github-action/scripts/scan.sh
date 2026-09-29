#!/usr/bin/env bash
# Runs one `rib scan` from the repository root, against a sibling checkout of
# the compared ref when there is one, and writes the step outputs: the
# counts, the SARIF and Markdown paths, the comparison checkout to remove, and
# the verdict the Gate step acts on. The step itself never fails on findings,
# so the SARIF upload and the comment still run.
set -euo pipefail

say() { printf 'ripbi-action: %s\n' "$1" >&2; }
output() { printf '%s=%s\n' "$1" "$2" >> "$GITHUB_OUTPUT"; }

compare="${INPUT_COMPARE:-}"
if [ -z "$compare" ]; then
  case "$GITHUB_EVENT_NAME" in
    pull_request | pull_request_target) compare=base ;;
    *) compare=none ;;
  esac
fi

args=(scan --no-input)
[ -n "${INPUT_PATH:-}" ] && args+=("$INPUT_PATH")

if [ "$compare" != none ]; then
  if [ "$compare" = base ]; then
    if [ -z "${GITHUB_BASE_REF:-}" ]; then
      echo "::error title=ripbi::compare: base needs a pull_request event; pass a ref or none"
      exit 1
    fi
    ref="refs/heads/$GITHUB_BASE_REF"
  else
    ref="$compare"
  fi
  # A depth-1 fetch of the ref alone: ripbi compares two folders, so no merge
  # base is needed, and it works under actions/checkout's default shallow
  # clone. The checkout is a sibling of the repository, outside the scan's
  # discovery, which is also what makes the summary read `../ripbi-base`.
  say "fetching $ref"
  git fetch --no-tags --depth=1 origin "$ref"
  base_dir=../ripbi-base
  git worktree remove --force "$base_dir" 2>/dev/null || rm -rf "$base_dir"
  git worktree prune
  git worktree add --detach "$base_dir" FETCH_HEAD
  output base-dir "$(cd "$base_dir" && pwd)"
  args+=(--compare-root "$base_dir")
fi

out="$RUNNER_TEMP/ripbi"
mkdir -p "$out"
rm -f "$out/ripbi.sarif" "$out/ripbi.json" "$out/ripbi.md"
args+=(--sarif-file "$out/ripbi.sarif" --json-file "$out/ripbi.json" --markdown-file "$out/ripbi.md")
if [ -n "${INPUT_ARGS:-}" ]; then
  read -r -a extra <<< "$INPUT_ARGS"
  args+=("${extra[@]}")
fi

say "rib ${args[*]}"
set +e
CLICOLOR_FORCE=1 rib "${args[@]}"
code=$?
set -e

if [ ! -f "$out/ripbi.json" ]; then
  # Exit 2 before any finding: usage, discovery, or ingestion.
  output verdict error
  exit 0
fi

# jq.exe ends its lines with CRLF on Windows.
count() { jq "$1" "$out/ripbi.json" | tr -d '\r'; }
new=$(count '.summary.findings')
fixed=$(count '.compare.fixed // [] | length')
existing=$(count '.compare.existing // 0')
output new-findings "$new"
output fixed-findings "$fixed"
output sarif-file "$out/ripbi.sarif"
output markdown-file "$out/ripbi.md"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  cat "$out/ripbi.md" >> "$GITHUB_STEP_SUMMARY"
fi

fail_on="${INPUT_FAIL_ON:-}"
[ -n "$fail_on" ] || fail_on=$([ "$compare" = none ] && echo any || echo new)
case "$fail_on:$code" in
  *:2) verdict=error ;; # --strict skips, or a file that could not be written
  never:*) verdict=pass ;;
  new:1) verdict=fail ;;
  new:*) verdict=pass ;;
  # Under a comparison, the existing findings are not in the exit code.
  any:*) verdict=$([ "$code" = 1 ] || [ "$existing" -gt 0 ] && echo fail || echo pass) ;;
  *)
    echo "::error title=ripbi::fail-on must be new, any, or never, not '$fail_on'"
    verdict=error
    ;;
esac
output verdict "$verdict"
