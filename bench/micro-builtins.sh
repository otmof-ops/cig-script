#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
#
# Run each bulk built-in's pair of programs (cig-script#255) on both
# engines: the loop a script wrote before, and the built-in that
# replaces it. Prints the whole-process time and the peak memory of
# each run, and fails when the two programs of a pair print different
# results.
#
#   bench/micro-builtins.sh [CIG]
#
# CIG defaults to target/release/cig (`cargo build --release`). The
# step budget is off so the loops finish, and every run has a scratch
# home. Times come from GNU time (`/usr/bin/time`). To count machine
# instructions for one program, where the system lets you:
#
#   perf stat -e instructions:u CIG run --engine vm bench/micro/NAME.cig
set -euo pipefail

cig=${1:-target/release/cig}
if [ ! -x "$cig" ]; then
  echo "no cig at $cig: build one (cargo build --release) or name it" >&2
  exit 1
fi
cig=$(cd "$(dirname "$cig")" && pwd)/$(basename "$cig")
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "${work:?}"' EXIT

export CIG_MAX_STEPS=0
status=0
for p in list-repeat counts map-inc list-count string-count list-extend; do
  echo
  echo "## $p"
  for e in tree vm; do
    for kind in loop builtin; do
      h=$work/home
      rm -rf "${h:?}"
      mkdir -m 700 "$h"
      HOME=$h CIGSCRIPT_HOME=$h/.cigscript \
        XDG_CONFIG_HOME=$h/.config XDG_DATA_HOME=$h/.local/share \
        XDG_CACHE_HOME=$h/.cache XDG_STATE_HOME=$h/.local/state \
        CIG_SEAL_KEY=file CIG_DOCTOR=0 NO_COLOR=1 \
        /usr/bin/time -f '%e s %M KB' -o "$work/time" \
        "$cig" run --engine "$e" "$root/bench/micro/$p-$kind.cig" > "$work/out.$kind" 2>&1
      printf '  %-5s %-8s %s  out=%s\n' "$e" "$kind" "$(cat "$work/time")" "$(tr -d '\n' < "$work/out.$kind")"
    done
    if ! cmp -s "$work/out.loop" "$work/out.builtin"; then
      echo "  $e: the loop and the built-in print different results" >&2
      status=1
    fi
  done
done
exit "$status"
