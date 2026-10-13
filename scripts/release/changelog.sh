#!/usr/bin/env bash
# CHANGELOG.md helpers for the release automation.
#
#   changelog.sh unreleased          print the body of the [Unreleased] section
#   changelog.sh prepare <ver> <date> move [Unreleased] into "## [<ver>] - <date>"
#                                     and update the compare links
set -euo pipefail

file="${CHANGELOG:-CHANGELOG.md}"

case "${1:-}" in
  unreleased)
    awk '
      $0 == "## [Unreleased]" { found = 1; next }
      found && /^## \[/ { exit }
      found { print }
    ' "${file}" | sed '/./,$!d'
    ;;
  prepare)
    version="${2:?version required}"
    date="${3:?date required}"
    if grep -q "^## \[${version}\]" "${file}"; then
      echo "CHANGELOG.md already has a [${version}] section" >&2
      exit 1
    fi
    awk -v v="${version}" -v d="${date}" '
      $0 == "## [Unreleased]" { print; print ""; print "## [" v "] - " d; next }
      /^\[Unreleased\]: .*\/compare\/v[^.]+\.[^.]+\.[^.]+\.\.\.HEAD$/ {
        base = $0; sub(/^\[Unreleased\]: /, "", base); sub(/\/compare\/.*/, "", base)
        prev = $0; sub(/.*\/compare\//, "", prev); sub(/\.\.\.HEAD$/, "", prev)
        print "[Unreleased]: " base "/compare/v" v "...HEAD"
        print "[" v "]: " base "/compare/" prev "...v" v
        next
      }
      { print }
    ' "${file}" > "${file}.tmp"
    mv "${file}.tmp" "${file}"
    ;;
  *)
    echo "usage: changelog.sh unreleased | prepare <version> <date>" >&2
    exit 2
    ;;
esac
