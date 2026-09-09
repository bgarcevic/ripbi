#!/bin/sh
# ripbi installer for macOS and Linux.
#
# One line:
#   curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh | sh
#
# Or download, review, then run:
#   curl -fsSL https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.sh -o install.sh
#   sh install.sh [--version v0.1.0]
#
# The script downloads the release archive, verifies its sha256 against the
# published sha256sums.txt, and installs the binary into ~/.local/bin as both
# `ripbi` and its short alias `rib` (a hard link to the same binary).

set -eu
# `| sh` often means dash, which has no pipefail; enable it only where it exists.
# shellcheck disable=SC3040
if (set -o pipefail) 2>/dev/null; then
  # shellcheck disable=SC3040
  set -o pipefail
fi

REPO="https://github.com/bgarcevic/ripbi"
API="https://api.github.com/repos/bgarcevic/ripbi"

usage() {
  cat <<EOF
ripbi installer

Usage: install.sh [--version VERSION]

  --version VERSION   install a specific release ("v0.1.0" or "0.1.0")
  -h, --help          show this help

Environment:
  RIPBI_VERSION       same as --version
  RIPBI_VERBOSE=1     print every step
  RIPBI_INSTALL_DIR   install destination (default: ~/.local/bin)
EOF
}

info() {
  printf 'install.sh: %s\n' "$1" >&2
}

die() {
  printf 'install.sh: error: %s\n' "$1" >&2
  exit 1
}

if [ "${RIPBI_VERBOSE:-0}" = "1" ]; then
  set -x
fi

version=""
while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      [ $# -ge 2 ] || die "--version needs an argument"
      version="$2"
      shift 2
      ;;
    --version=*)
      version="${1#--version=}"
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument '$1' (see --help)"
      ;;
  esac
done

have() {
  command -v "$1" >/dev/null 2>&1
}

fetch() { # fetch URL > stdout
  if have curl; then
    curl -fsSL "$1"
  elif have wget; then
    wget -qO- "$1"
  else
    die "need curl or wget to download ripbi"
  fi
}

download() { # download URL DEST
  if have curl; then
    curl -fsSL "$1" -o "$2"
  elif have wget; then
    wget -qO "$2" "$1"
  else
    die "need curl or wget to download ripbi"
  fi
}

case "$(uname -s):$(uname -m)" in
  Linux:x86_64 | Linux:amd64)
    target=x86_64-unknown-linux-gnu
    ;;
  Darwin:arm64 | Darwin:aarch64)
    target=aarch64-apple-darwin
    ;;
  MINGW*:* | MSYS*:* | CYGWIN*:* | Windows*:*)
    die "on Windows, install with: irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 | iex"
    ;;
  *)
    die "no prebuilt ripbi for $(uname -s)/$(uname -m); pick an archive manually from ${REPO}/releases"
    ;;
esac

if [ -z "$version" ]; then
  version="${RIPBI_VERSION:-}"
fi
version="${version#v}"
if [ -z "$version" ]; then
  info "resolving the latest release"
  tag=$(fetch "${API}/releases/latest" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
  [ -n "$tag" ] || die "could not resolve the latest release from ${API}"
  version="${tag#v}"
fi
case "$version" in
  *[!0-9A-Za-z.-]*) die "'$version' does not look like a release version" ;;
esac

archive="ripbi-${version}-${target}.tar.gz"
base="${REPO}/releases/download/v${version}"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM

info "downloading ${archive}"
download "${base}/${archive}" "${tmp}/${archive}"
download "${base}/sha256sums.txt" "${tmp}/sha256sums.txt"

expected=$(awk -v a="$archive" '$2 == a { print $1 }' "${tmp}/sha256sums.txt")
[ -n "$expected" ] || die "${archive} is not listed in sha256sums.txt"

if have sha256sum; then
  actual=$(sha256sum "${tmp}/${archive}" | awk '{print $1}')
elif have shasum; then
  actual=$(shasum -a 256 "${tmp}/${archive}" | awk '{print $1}')
else
  die "need sha256sum or shasum to verify the download"
fi
if [ "$actual" != "$expected" ]; then
  die "checksum mismatch for ${archive}: expected ${expected}, got ${actual}"
fi
info "checksum ok"

tar -xzf "${tmp}/${archive}" -C "$tmp"
binary="${tmp}/ripbi-${version}/ripbi"
[ -f "$binary" ] || die "the archive did not contain the expected binary"

install_dir="${RIPBI_INSTALL_DIR:-$HOME/.local/bin}"
if ! mkdir -p "$install_dir" 2>/dev/null; then
  info "could not create ${install_dir}, falling back to $HOME/bin"
  install_dir="$HOME/bin"
  mkdir -p "$install_dir"
fi
mv "$binary" "${install_dir}/ripbi"
chmod +x "${install_dir}/ripbi"
# Same binary, shorter name. A hard link is enough; fall back to a copy on
# filesystems that refuse links.
if ! ln -f "${install_dir}/ripbi" "${install_dir}/rib" 2>/dev/null; then
  cp "${install_dir}/ripbi" "${install_dir}/rib"
fi

case ":$PATH:" in
  *":${install_dir}:"*) ;;
  *)
    info "${install_dir} is not on your PATH"
    # shellcheck disable=SC2016  # the hint must show a literal $PATH
    printf 'install.sh: add it to your shell profile, e.g.:\n  export PATH="%s:$PATH"\n' "$install_dir" >&2
    ;;
esac

printf 'installed ripbi %s to %s/ripbi (also usable as %s/rib)\n' "$version" "$install_dir" "$install_dir"
