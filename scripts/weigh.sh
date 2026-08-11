#!/usr/bin/env bash
# What each build profile links, against its budget.
#
# Lightness is checked, not hoped for: a dependency that arrives without a use
# case in `docs/plugins.md` belongs to a plugin. The window is the only heavy
# profile and has to stay the only one, so each budget is a ceiling a change
# has to argue past rather than a number that drifts up.
set -uo pipefail

cd "$(dirname "$0")/.."

# profile:budget:cargo flags
PROFILES=(
  "driver:90:--no-default-features --features driver-only"
  "tui:135:--no-default-features --features tui"
  "window:210:"
)

fail=0
for entry in "${PROFILES[@]}"; do
  name=${entry%%:*}
  rest=${entry#*:}
  budget=${rest%%:*}
  flags=${rest#*:}

  # shellcheck disable=SC2086
  count=$(cargo tree -p davimci-cli --prefix none --edges normal $flags 2>/dev/null |
    awk 'NF {print $1}' | sort -u | grep -c .)
  if [ "$count" -eq 0 ]; then
    printf '  \033[31mFAIL\033[0m  %-7s cargo tree failed\n' "$name"
    fail=1
  elif [ "$count" -gt "$budget" ]; then
    printf '  \033[31mOVER\033[0m  %-7s %3d crates (budget %d)\n' "$name" "$count" "$budget"
    fail=1
  else
    printf '  \033[32mok\033[0m    %-7s %3d crates (budget %d)\n' "$name" "$count" "$budget"
  fi
done

if [ "$fail" -ne 0 ]; then
  echo
  echo "A profile grew past its budget. Either the dependency earns its place in"
  echo "docs/plugins.md's core list, or the feature that pulled it in is a plugin."
fi
exit "$fail"
