#!/usr/bin/env bash
# Puts `rib` on the job's PATH: the folder in INPUT_BINARY when given (a
# locally built ripbi), else the INPUT_VERSION release, installed with the
# checksum-verifying install script of that same release tag.
set -euo pipefail

if [ -n "${INPUT_BINARY:-}" ]; then
  dir=$(cd "$INPUT_BINARY" && pwd)
else
  version="${INPUT_VERSION:-latest}"
  if [ "$version" = latest ]; then
    # gh authenticates with the job token: an anonymous API call shares the
    # runner IP's 60-requests-an-hour limit with every other job on it.
    tag=$(gh release view --repo bgarcevic/ripbi --json tagName --jq .tagName)
  else
    tag="v${version#v}"
  fi
  dir="$RUNNER_TEMP/ripbi-bin"
  script="https://raw.githubusercontent.com/bgarcevic/ripbi/${tag}"
  if [ "$RUNNER_OS" = Windows ]; then
    curl -fsSL "$script/install.ps1" -o "$RUNNER_TEMP/ripbi-install.ps1"
    RIPBI_INSTALL_DIR="$dir" pwsh -NoProfile -File "$RUNNER_TEMP/ripbi-install.ps1" -Version "$tag"
  else
    curl -fsSL "$script/install.sh" | RIPBI_INSTALL_DIR="$dir" sh -s -- --version "$tag"
  fi
fi

if [ ! -x "$dir/rib" ] && [ ! -x "$dir/rib.exe" ]; then
  echo "::error title=ripbi::no rib binary in $dir"
  exit 1
fi
if [ "$RUNNER_OS" = Windows ]; then
  # GITHUB_PATH takes a Windows path; bash's /d/a/... form is not one.
  cygpath -w "$dir" >> "$GITHUB_PATH"
else
  echo "$dir" >> "$GITHUB_PATH"
fi
"$dir/rib" --version

# The action writes every output from one scan; releases before these flags
# would need one scan per format.
if ! "$dir/rib" scan --help | grep -q -- --markdown-file; then
  echo "::error title=ripbi::this ripbi release predates scan --markdown-file; set version to a newer release or latest"
  exit 1
fi
