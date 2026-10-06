#!/bin/sh
# owlpost installer — downloads a release tarball of `owl`, verifies its SHA-256 and installs it.
#
#   curl -fsSL https://raw.githubusercontent.com/Krab00/owl-post/main/scripts/install.sh | sh
#   sh scripts/install.sh [--version <tag>] [--prefix <dir> | --system]
#
# Flags
#   --version <tag>   release tag to install, e.g. v0.1.0 (default: the latest release)
#   --prefix <dir>    directory that receives `owl` (default: ~/.local/bin)
#   --system          shorthand for --prefix /usr/local/bin (needs a writable /usr/local/bin)
#   -h, --help        print this help
#
# Assets are fetched from https://github.com/Krab00/owl-post/releases/download/<tag>/ as
# `owl-<version>-<target>.tar.gz` (the tarball holds a single file, `owl`) plus `SHA256SUMS`;
# <version> is <tag> without its leading `v`. Without --version the `SHA256SUMS` of the latest
# release is read first and the version comes from the asset name listed there.
#
# Testability hooks (environment variables; `tests/install_sh.rs` drives every guard through them)
#   OWL_INSTALL_BASE_URL   replaces the download base; accepts an http(s) URL or a `file://<dir>`
#                          URL so a locally built tarball + SHA256SUMS can be served offline.
#   OWL_INSTALL_FAKE_SUM   when set to 1, corrupts the expected checksum right before the
#                          comparison so the mismatch path is exercised (must exit non-zero).
#   OWL_INSTALL_OS / OWL_INSTALL_ARCH   override `uname -s` / `uname -m` for the platform guard.
#   OWL_INSTALL_CURL       name of the curl binary to use (default `curl`) for the missing-curl guard.
#
# Exit codes: 0 installed, 2 usage or unsupported environment, 1 download / checksum / install
# failure. Every failure prints one `owl install: ...` line on stderr; nothing is left behind
# in <prefix> on any failure (an existing directory at <prefix>/owl is refused before download).
set -eu

REPO="Krab00/owl-post"
CURL="${OWL_INSTALL_CURL:-curl}"

usage() {
    cat <<'EOF'
owl install: download a release of owl, verify its SHA-256 and install it

usage: install.sh [--version <tag>] [--prefix <dir> | --system]
   or: curl -fsSL https://raw.githubusercontent.com/Krab00/owl-post/main/scripts/install.sh | sh

  --version <tag>   release tag to install, e.g. v0.1.0 (default: the latest release)
  --prefix <dir>    directory that receives owl (default: ~/.local/bin)
  --system          shorthand for --prefix /usr/local/bin
  -h, --help        print this help
EOF
}

die() {
    echo "owl install: $1" >&2
    exit "${2:-1}"
}

tag=""
prefix="${HOME}/.local/bin"
while [ $# -gt 0 ]; do
    case "$1" in
        --version)
            [ $# -ge 2 ] || die "--version needs a value (e.g. --version v0.1.0)" 2
            tag="$2"
            shift 2
            ;;
        --prefix)
            [ $# -ge 2 ] || die "--prefix needs a value (e.g. --prefix ~/.local/bin)" 2
            prefix="$2"
            shift 2
            ;;
        --system)
            prefix="/usr/local/bin"
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument '$1' (see --help)" 2
            ;;
    esac
done

# --- platform -> target triple ---------------------------------------------------------------
os="${OWL_INSTALL_OS:-$(uname -s)}"
arch="${OWL_INSTALL_ARCH:-$(uname -m)}"
case "$os/$arch" in
    Darwin/arm64 | Darwin/aarch64) target="aarch64-apple-darwin" ;;
    Darwin/x86_64) target="x86_64-apple-darwin" ;;
    Linux/x86_64 | Linux/amd64) target="x86_64-unknown-linux-gnu" ;;
    Linux/aarch64 | Linux/arm64) target="aarch64-unknown-linux-gnu" ;;
    *) die "unsupported platform ${os}/${arch} (supported: macOS and Linux on x86_64 or aarch64)" 2 ;;
esac

# --- tools -----------------------------------------------------------------------------------
command -v "$CURL" >/dev/null 2>&1 || die "curl is required but was not found on PATH" 2
command -v tar >/dev/null 2>&1 || die "tar is required but was not found on PATH" 2
if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    die "neither sha256sum nor shasum is available to verify the download" 2
fi

# --- prefix must be writable before anything is downloaded ----------------------------------
mkdir -p "$prefix" 2>/dev/null || true
[ -d "$prefix" ] && [ -w "$prefix" ] \
    || die "prefix ${prefix} is not writable (use --prefix <dir> you own, or sudo for --system)" 1
[ ! -d "$prefix/owl" ] || die "could not write ${prefix}/owl: is a directory" 1

# --- download --------------------------------------------------------------------------------
if [ -n "$tag" ]; then
    case "$tag" in v*) ;; *) tag="v${tag}" ;; esac
    version="${tag#v}"
    base="${OWL_INSTALL_BASE_URL:-https://github.com/${REPO}/releases/download/${tag}}"
else
    version=""
    base="${OWL_INSTALL_BASE_URL:-https://github.com/${REPO}/releases/latest/download}"
fi
base="${base%/}"

tmp="$(mktemp -d 2>/dev/null || mktemp -d -t owl-install)"
trap 'rm -rf "$tmp"' EXIT INT TERM

fetch() {
    # $1 = asset name, $2 = destination
    "$CURL" -fsSL --retry 2 -o "$2" "${base}/$1" 2>/dev/null \
        || die "download failed for ${base}/$1" 1
}

fetch SHA256SUMS "$tmp/SHA256SUMS"
if [ -z "$version" ]; then
    asset="$(grep -o "owl-[^ ]*-${target}\.tar\.gz" "$tmp/SHA256SUMS" | head -n 1 || true)"
    [ -n "$asset" ] || die "no asset for ${target} listed in ${base}/SHA256SUMS" 1
    version="${asset#owl-}"
    version="${version%-${target}.tar.gz}"
else
    asset="owl-${version}-${target}.tar.gz"
fi
expected="$(grep " ${asset}\$" "$tmp/SHA256SUMS" | head -n 1 | cut -d' ' -f1 || true)"
[ -n "$expected" ] || die "${asset} is not listed in SHA256SUMS" 1

echo "downloading ${base}/${asset}"
fetch "$asset" "$tmp/$asset"

# --- verify ----------------------------------------------------------------------------------
if [ "${OWL_INSTALL_FAKE_SUM:-0}" = "1" ]; then
    expected="0000000000000000000000000000000000000000000000000000000000000000"
fi
actual="$(sha256 "$tmp/$asset")"
[ "$actual" = "$expected" ] \
    || die "checksum mismatch for ${asset}: expected ${expected}, got ${actual}" 1

# --- install ---------------------------------------------------------------------------------
tar -xzf "$tmp/$asset" -C "$tmp" owl 2>/dev/null || die "could not extract owl from ${asset}" 1
cp "$tmp/owl" "$prefix/owl.tmp.$$" || die "could not write ${prefix}/owl" 1
chmod +x "$prefix/owl.tmp.$$"
mv -f "$prefix/owl.tmp.$$" "$prefix/owl" || die "could not write ${prefix}/owl" 1

echo "installed owl ${version} to ${prefix}/owl"
case ":${PATH}:" in
    *":${prefix}:"*) ;;
    *) echo "note: ${prefix} is not on your PATH; add it to your shell profile" ;;
esac
echo "next steps:"
echo "  owl init              # create your key and config, prints your fingerprint"
echo "  owl install           # register the owl daemon as a user service"
echo "  owl contact export    # your peer file, to be committed under .agents/peers/"
