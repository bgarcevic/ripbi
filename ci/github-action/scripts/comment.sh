#!/usr/bin/env bash
# Posts the scan's Markdown summary as one pull request comment, updated in
# place on every run. A hidden marker (per scanned path, so two scans in one
# workflow keep two comments) finds the previous one. Never fails the job: a
# fork's pull request gets a read-only token, and the summary is on the job
# page regardless.
set -euo pipefail

pr=$(jq -r '.pull_request.number // empty' "$GITHUB_EVENT_PATH" | tr -d '\r')
if [ -z "$pr" ]; then
  echo "::notice title=ripbi::comment: true needs a pull_request event; skipped the comment"
  exit 0
fi

marker="<!-- ripbi-action path=${INPUT_PATH:-.} -->"
body="$marker"$'\n'"$(cat "$MARKDOWN_FILE")"
api="repos/$GITHUB_REPOSITORY/issues"

id=$(gh api --paginate "$api/$pr/comments" \
  --jq ".[] | select(.body | startswith(\"$marker\")) | .id" | head -n 1 | tr -d '\r') || id=""

if [ -n "$id" ]; then
  gh api --method PATCH "$api/comments/$id" -f body="$body" > /dev/null || failed=1
else
  gh api --method POST "$api/$pr/comments" -f body="$body" > /dev/null || failed=1
fi
if [ -n "${failed:-}" ]; then
  echo "::warning title=ripbi::could not comment on the pull request (a fork's token is read-only; the job needs pull-requests: write)"
fi
