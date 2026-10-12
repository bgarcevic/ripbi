#!/usr/bin/env bash
# Prints "<version> <bump>" for the next stable release, derived from the
# conventional-commit subjects (and BREAKING CHANGE footers) merged since the
# last stable tag. Prints nothing when there is nothing to release.
#
#   breaking (type!: / BREAKING CHANGE)  -> major; minor while major is 0
#   feat                                 -> minor
#   anything else                        -> patch
#
# The 0.x rule mirrors release-please's bump-minor-pre-major: SemVer allows
# breaking changes in 0.x minors, so a stray "!" can never produce 1.0.0.
#
# A "Release-As: X.Y.Z" footer on any commit since the last tag overrides the
# computed version (release-please's convention; the newest footer wins). It
# covers what titles cannot say: a deliberate 1.0.0, or a minor whose commits
# predate conventional titles.
set -euo pipefail

last_tag="$(git describe --tags --abbrev=0 --match 'v[0-9]*' --exclude '*-*' 2>/dev/null || echo v0.0.0)"
range="${last_tag}..HEAD"
[[ "${last_tag}" == "v0.0.0" ]] && range="HEAD"

bump=""
release_as=""
while IFS= read -r -d $'\x1e' entry; do
  entry="${entry#$'\n'}"
  subject="${entry%%$'\x1f'*}"
  body="${entry#*$'\x1f'}"
  [[ -z "${subject}" ]] && continue
  # The release commit itself carries no change.
  [[ "${subject}" =~ ^chore\(release\) ]] && continue
  if [[ -z "${release_as}" ]]; then
    release_as="$(grep -ioE '^release-as: *[0-9]+\.[0-9]+\.[0-9]+ *$' <<<"${body}" | head -n 1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true)"
  fi
  if [[ "${subject}" =~ ^[a-z]+(\([^\)]*\))?!: ]] || grep -q '^BREAKING[ -]CHANGE:' <<<"${body}"; then
    bump="major"
  elif [[ "${subject}" =~ ^feat(\([^\)]*\))?: ]]; then
    [[ "${bump}" != major ]] && bump="minor"
  else
    [[ -z "${bump}" ]] && bump="patch"
  fi
done < <(git log --format='%s%x1f%b%x1e' "${range}")

[[ -z "${bump}" ]] && exit 0

if [[ -n "${release_as}" ]]; then
  echo "${release_as} release-as"
  exit 0
fi

IFS=. read -r major minor patch <<<"${last_tag#v}"
if [[ "${bump}" == major && "${major}" -eq 0 ]]; then
  bump="minor"
fi
case "${bump}" in
  major) echo "$((major + 1)).0.0 ${bump}" ;;
  minor) echo "${major}.$((minor + 1)).0 ${bump}" ;;
  patch) echo "${major}.${minor}.$((patch + 1)) ${bump}" ;;
esac
