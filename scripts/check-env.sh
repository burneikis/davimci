#!/usr/bin/env bash
# Verify the vimci development environment. Exits non-zero if anything is missing.
set -uo pipefail

missing=0
ok()   { printf '  \033[32mok\033[0m    %s\n' "$1"; }
bad()  { printf '  \033[31mMISSING\033[0m %s\n' "$1"; missing=1; }
warn() { printf '  \033[33mwarn\033[0m  %s\n' "$1"; }

# need_bin <binary> <hint> [version-args...]
need_bin() {
  local bin=$1 hint=$2
  shift 2
  if command -v "$bin" >/dev/null 2>&1; then
    ok "$bin ($("$bin" "$@" 2>&1 | head -1))"
  else
    bad "$bin - $hint"
  fi
}

echo "vimci environment check"
echo
echo "Toolchain:"
need_bin cargo   "install: pacman -S rust  (or rustup.rs)" --version
need_bin rustc   "install: pacman -S rust" --version
need_bin rustfmt "install: pacman -S rust" --version
command -v clippy-driver >/dev/null 2>&1 \
  && ok "clippy" || bad "clippy - install: pacman -S rust"

echo
echo "Media:"
need_bin ffmpeg  "install: pacman -S ffmpeg" -version
need_bin ffprobe "install: pacman -S ffmpeg" -version

echo
echo "Backend:"
# MLT's pkg-config name is major-version suffixed (mlt-framework-7 on MLT 7.x).
mlt_pc=""
for pc in mlt-framework-7 mlt-framework mlt++-7 mlt++; do
  if pkg-config --exists "$pc" 2>/dev/null; then mlt_pc=$pc; break; fi
done
if [ -n "$mlt_pc" ]; then
  ok "libmlt ($mlt_pc $(pkg-config --modversion "$mlt_pc"))"
  hdr=$(pkg-config --cflags-only-I "$mlt_pc" | tr ' ' '\n' | sed 's/^-I//' | head -1)
  if [ -n "$hdr" ] && [ -f "$hdr/framework/mlt.h" ]; then
    ok "libmlt headers ($hdr/framework/mlt.h)"
  else
    bad "libmlt headers - bindgen needs framework/mlt.h"
  fi
else
  bad "libmlt - install: pacman -S mlt   (Debian: apt install libmlt-dev)"
fi
need_bin clang "needed by bindgen; install: pacman -S clang" --version

echo
echo "GPU (snapshot tests):"
if [ -n "$(ls /usr/share/vulkan/icd.d/ 2>/dev/null)" ]; then
  ok "vulkan ICD present: $(ls /usr/share/vulkan/icd.d/ | tr '\n' ' ')"
else
  warn "no Vulkan ICD - GUI/presenter snapshot tests will skip"
  warn "  software fallback: pacman -S vulkan-swrast"
fi

echo
echo "Notes:"
echo "  Lua is vendored by mlua (5.4). System Lua 5.5 is NOT usable - do not link it."
echo "  libmlt must be linked dynamically (LGPL-2.1); never vendor melt/melted (GPL-2)."

echo
if [ "$missing" -eq 0 ]; then
  printf '\033[32mEnvironment OK.\033[0m\n'
else
  printf '\033[31mEnvironment incomplete - see MISSING entries above.\033[0m\n'
fi
exit "$missing"
