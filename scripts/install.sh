#!/usr/bin/env bash
# Install davimci: a release build if one exists for this platform, otherwise
# from source. libmlt is never bundled - it is LGPL-2.1 and dynamically linked,
# so it has to come from the system package manager either way.
set -euo pipefail

REPO=${DAVIMCI_REPO:-burneikis/davimci}
PREFIX=${DAVIMCI_PREFIX:-$HOME/.local}
VERSION=${DAVIMCI_VERSION:-latest}
FROM_SOURCE=0

usage() {
  cat <<'EOF'
Usage: install.sh [--from-source] [--version <tag>] [--prefix <dir>]

  --from-source   Build with cargo instead of downloading a release.
  --version TAG   Release tag to install (default: latest).
  --prefix DIR    Install root; the binary lands in DIR/bin (default: ~/.local).

Environment: DAVIMCI_REPO, DAVIMCI_PREFIX, DAVIMCI_VERSION.
EOF
}

while [ $# -gt 0 ]; do
  case $1 in
    --from-source) FROM_SOURCE=1 ;;
    --version) VERSION=$2; shift ;;
    --prefix)  PREFIX=$2; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

say()  { printf '\033[32m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[33mwarn\033[0m %s\n' "$1" >&2; }
die()  { printf '\033[31merror\033[0m %s\n' "$1" >&2; exit 1; }

# libmlt is a hard runtime requirement of every build, prebuilt or not.
check_runtime_deps() {
  local missing=()
  ldconfig -p 2>/dev/null | grep 'libmlt' >/dev/null || missing+=("libmlt")
  command -v ffmpeg >/dev/null 2>&1 || warn "ffmpeg not found - export presets that shell out will fail"
  if [ ${#missing[@]} -gt 0 ]; then
    die "missing: ${missing[*]}
  Arch:   sudo pacman -S --needed mlt ffmpeg
  Debian: sudo apt install libmlt-dev ffmpeg"
  fi
}

target_triple() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)
  case "$os-$arch" in
    Linux-x86_64)  echo x86_64-unknown-linux-gnu ;;
    Linux-aarch64) echo aarch64-unknown-linux-gnu ;;
    *) return 1 ;;
  esac
}

install_release() {
  local triple=$1 base url tmp asset
  asset="davimci-$triple.tar.gz"
  if [ "$VERSION" = latest ]; then
    base="https://github.com/$REPO/releases/latest/download"
  else
    base="https://github.com/$REPO/releases/download/$VERSION"
  fi
  url="$base/$asset"

  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' RETURN

  say "downloading $url"
  curl -fsSL --retry 2 -o "$tmp/$asset" "$url" || return 1
  # A release without its checksum is treated as no release at all.
  curl -fsSL --retry 2 -o "$tmp/$asset.sha256" "$url.sha256" || return 1
  ( cd "$tmp" && sha256sum -c "$asset.sha256" >/dev/null ) \
    || die "checksum mismatch for $asset - refusing to install"

  tar -xzf "$tmp/$asset" -C "$tmp"
  install -Dm755 "$tmp/davimci" "$PREFIX/bin/davimci"
}

install_from_source() {
  command -v cargo >/dev/null 2>&1 || die "cargo not found - install rust (pacman -S rust, or rustup.rs)"
  command -v clang >/dev/null 2>&1 || die "clang not found, needed by bindgen for the MLT FFI"
  local src
  if [ -f "$(dirname "$0")/../Cargo.toml" ]; then
    src=$(cd "$(dirname "$0")/.." && pwd)
  else
    src=$(mktemp -d)/davimci
    say "cloning $REPO"
    git clone --depth 1 "https://github.com/$REPO" "$src"
  fi
  say "building (release)"
  ( cd "$src" && cargo build --release -p davimci-cli )
  install -Dm755 "$src/target/release/davimci" "$PREFIX/bin/davimci"
}

check_runtime_deps

if [ "$FROM_SOURCE" -eq 1 ]; then
  install_from_source
elif triple=$(target_triple) && install_release "$triple"; then
  :
else
  warn "no release build for this platform or version; falling back to source"
  install_from_source
fi

say "installed $PREFIX/bin/davimci"
"$PREFIX/bin/davimci" --version || true

case ":$PATH:" in
  *":$PREFIX/bin:"*) ;;
  *) warn "$PREFIX/bin is not on PATH - add it to your shell rc" ;;
esac

cat <<EOF

Config: ~/.config/davimci/init.lua
Start:  davimci clip.mkv
EOF
