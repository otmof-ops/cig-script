#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
#
# Make the engine run of 2026-11-25 and render the two documents and the
# README's paragraphs. Five groups of passes, two passes each (seeds 1 and 2):
#
#   main            the compute programs, on one CPU: each engine, the default,
#                   and (with LUA, LUAJIT and RUSTC) the other languages
#   start           the three start scripts, two CPUs for each measured process
#   start-one-cpu   the three start scripts, one CPU
#   before          as start, with the cig built before the default lowered a
#                   script on its second run (BEFORE_CIG)
#   before-one-cpu  as start-one-cpu, with that cig
#
# then report.py for each (it writes numbers.json and renders the templates)
# and check.py (it recomputes every number and renders every template again,
# and exits non-zero on any difference). The results folder gets numbers.json,
# the pass tsvs under pass1/ and pass2/, and the rendered readme.md beside each
# template; the rendered documents land in docs/.
#
# Usage:
#   [CIGO_SCRIPT=/path/to/orchestrate.cig] [BEFORE_CIG=/path/to/cig] \
#   [LUA=/path/to/lua LUAJIT=/path/to/luajit RUSTC=/path/to/rustc] \
#       bench/results/2026-11-25-engines/run.sh CIG WORK
#
#   CIG         the cig binary to measure.
#   WORK        a scratch folder OUTSIDE the repository: the cig home of each
#               pass (removed after the pass), the start scripts and the log.
#   BEFORE_CIG  the cig the two `before` groups measure. Without it they are
#               not made, and the documents render only if their passes are
#               already in the results folder.
#   LUA, LUAJIT, RUSTC
#               the Lua 5.4, LuaJIT and rustc the main group runs the same
#               programs on, beside CPython (`python3`) and Node (`node`) from
#               /usr/bin. Without all three the main group times cig alone,
#               and the main document does not render.
#   PASSES      which groups to make (default: all five that can be made);
#               PASSES=none makes none and only renders.

set -euo pipefail

if [ "$#" -ne 2 ]; then
  printf 'usage: %s CIG WORK\n' "$0" >&2
  exit 2
fi

CIG=$1
WORK=$2
if [ ! -x "$CIG" ]; then
  printf 'run.sh: %s is not an executable file\n' "$CIG" >&2
  exit 2
fi
case "$WORK" in
  /*) : ;;
  *)
    printf 'run.sh: WORK must be an absolute path (got %s)\n' "$WORK" >&2
    exit 2
    ;;
esac
CIG=$(readlink -f -- "$CIG")
BEFORE=${BEFORE_CIG:-}
if [ -n "$BEFORE" ]; then
  if [ ! -x "$BEFORE" ]; then
    printf 'run.sh: BEFORE_CIG %s is not an executable file\n' "$BEFORE" >&2
    exit 2
  fi
  BEFORE=$(readlink -f -- "$BEFORE")
fi

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
REPO=$(cd -- "$HERE/../../.." && pwd)
RESULTS=$HERE
STARTSRC=$WORK/start-src
LOG=$WORK/run.log
# Where the two rendered documents land: the repository's docs folder, or a
# scratch folder for a trial run.
DOCS=${DOCS:-$REPO/docs}
mkdir -p -- "$DOCS"

mkdir -p "$WORK"
: >"$LOG"
say() { printf '[run.sh] %s\n' "$*" | tee -a "$LOG" >&2; }

# The three scripts the start passes time: hello (the smallest benchmark
# program), the 500-line script (bench/engines/lines-500.cig), and cigo's
# orchestrate.cig. cigo's script is not in this repository, so its expected
# output is captured once from a tree-walker run of the cig being measured
# and every timed run is checked against it.
say "preparing start-src in $STARTSRC"
rm -rf -- "$STARTSRC"
mkdir -p "$STARTSRC"
cp -- "$REPO/bench/compute/hello.cig" "$STARTSRC/hello.cig"
cp -- "$REPO/bench/compute/hello.expect" "$STARTSRC/hello.expect"
cp -- "$REPO/bench/engines/lines-500.cig" "$STARTSRC/lines-500.cig"
cp -- "$REPO/bench/engines/lines-500.expect" "$STARTSRC/lines-500.expect"

# Where cigo's script is, when it is to be measured: CIGO_SCRIPT names
# cig-orchestrator's orchestrate.cig. Without it the start passes have two
# scripts.
CIGO=${CIGO_SCRIPT:-}
if [ -n "$CIGO" ] && [ -f "$CIGO" ]; then
  cp -- "$CIGO" "$STARTSRC/orchestrate.cig"
  # The script with no arguments prints its help. A scratch cig home for
  # this one run, so nothing lands in the user's own tin.
  EXPECT_HOME=$WORK/expect-home
  rm -rf -- "$EXPECT_HOME"
  mkdir -p "$EXPECT_HOME"
  env -i PATH=/usr/bin:/bin HOME="$EXPECT_HOME" \
    CIGSCRIPT_HOME="$EXPECT_HOME/.cigscript" \
    XDG_CONFIG_HOME="$EXPECT_HOME/.config" \
    XDG_DATA_HOME="$EXPECT_HOME/.local/share" \
    XDG_CACHE_HOME="$EXPECT_HOME/.cache" \
    XDG_STATE_HOME="$EXPECT_HOME/.local/state" \
    CIG_SEAL_KEY=file CIG_MAX_STEPS=0 CIG_REPORTS=never CIG_DOCTOR=0 NO_COLOR=1 \
    timeout 60 "$CIG" run --engine tree "$STARTSRC/orchestrate.cig" \
    >"$STARTSRC/orchestrate.expect"
  rm -rf -- "$EXPECT_HOME"
  say "orchestrate.expect captured from $CIG"
else
  say "note: CIGO_SCRIPT names no file; the start passes run without orchestrate.cig"
fi

# The compute programs: each engine, and the default with its module kept.
# With the other languages, `cig` is `cig run` on the default once more, in
# the home the benchmark paper's harness gives it: the side its tables set
# against them.
LANGS=()
if [ -n "${LUA:-}" ] && [ -n "${LUAJIT:-}" ] && [ -n "${RUSTC:-}" ]; then
  LANGS=(--lua "$LUA" --luajit "$LUAJIT" --rustc "$RUSTC")
  SIDES=${SIDES:-cig-tree,cig-vm,cig-vm-cold,cig-auto,cig,lua,luajit-off,luajit,python,node,rust}
else
  SIDES=${SIDES:-cig-tree,cig-vm,cig-vm-cold,cig-auto}
fi
# The start scripts: those, and the default on a script's first and second run.
START_SIDES=${START_SIDES:-cig-tree,cig-vm,cig-vm-cold,cig-auto,cig-auto-cold,cig-auto-second}
# The cig before the change has no second-run case to time: its first run
# starts the lowering, and its second is on the VM.
BEFORE_SIDES=${BEFORE_SIDES:-cig-tree,cig-vm,cig-vm-cold,cig-auto,cig-auto-cold}
ROUNDS=${ROUNDS:-21}
CPUS=${CPUS:-2}
# The compute programs: a rest after each run as long as the run, so a short
# run is not timed on a core a long one has just heated.
REST=${REST:-1}
# Every round of every pass waits for the CPU package to be below this.
COOL=${COOL:-70}
# The start scripts run for a few milliseconds, so they get more rounds, and
# each timed run starts from a CPU the harness has kept busy for START_WARM
# milliseconds: a run that short, started on an idle core, is timed as much
# by that core's clock as by the program. They are timed with two CPUs for
# each measured process (START_CPUS), so a `cig __lower` left behind has one
# to run on, as on any machine with more than one; and held to one (CPUS).
START_ROUNDS=${START_ROUNDS:-101}
START_CPUS=${START_CPUS:-2,3}
START_WARM=${START_WARM:-5}

if [ -n "$BEFORE" ]; then
  PASSES=${PASSES:-main start start-one-cpu before before-one-cpu}
else
  PASSES=${PASSES:-main start start-one-cpu}
fi
if [ "$PASSES" = none ]; then
  PASSES=
fi

one_pass() {
  # one_pass GROUP SEED  -> writes results/GROUP/pass<SEED>/.
  local name=$1 seed=$2
  local out=$RESULTS/$name/pass$seed
  local cig=$CIG sides=$SIDES rounds=$ROUNDS cpus=$CPUS
  local how=(--rest "$REST")
  local extra=()
  case "$name" in
    main) extra=("${LANGS[@]}") ;;
    start | start-one-cpu | before | before-one-cpu)
      extra=(--compute "$STARTSRC")
      rounds=$START_ROUNDS
      how=(--warm "$START_WARM")
      sides=$START_SIDES
      ;;
    *)
      printf 'run.sh: no pass group is named %s\n' "$name" >&2
      exit 2
      ;;
  esac
  case "$name" in
    start | before) cpus=$START_CPUS ;;
  esac
  case "$name" in
    before | before-one-cpu)
      if [ -z "$BEFORE" ]; then
        printf 'run.sh: the group %s needs BEFORE_CIG\n' "$name" >&2
        exit 2
      fi
      cig=$BEFORE
      sides=$BEFORE_SIDES
      ;;
  esac
  rm -rf -- "$out"
  mkdir -p "$out"
  local cighome=$WORK/$name-pass$seed-home
  rm -rf -- "$cighome"
  mkdir -p "$cighome"
  say "pass $name seed $seed: compare.py --cig $cig --cpus $cpus --rounds $rounds ${how[*]} --cool $COOL"
  env -i PATH=/usr/bin:/bin HOME="$cighome" CIGSCRIPT_HOME="$cighome/.cigscript" \
    XDG_CONFIG_HOME="$cighome/.config" XDG_DATA_HOME="$cighome/.local/share" \
    XDG_CACHE_HOME="$cighome/.cache" XDG_STATE_HOME="$cighome/.local/state" \
    CIG_SEAL_KEY=file \
    timeout 7200 python3 "$REPO/bench/compare/compare.py" \
      --out "$out" --seed "$seed" --rounds "$rounds" --cpus "$cpus" \
      "${how[@]}" --cool "$COOL" \
      --cig "$cig" --sides "$sides" "${extra[@]}" 2>&1 | tee -a "$LOG"
  # The pass's conditions.txt has what it needs of the home; the tins can
  # get large.
  rm -rf -- "$cighome"
  # What compare.py leaves that the results folder does not keep.
  rm -f -- "$out/spec.json"
  rm -rf -- "$out/work"
}

for group in $PASSES; do
  one_pass "$group" 1
  one_pass "$group" 2
done

numbers() {
  # numbers GROUP  -> results/GROUP/numbers.json, made and made again.
  local d=$RESULTS/$1
  say "numbers $1: report.py, check.py"
  python3 "$REPO/bench/compare/report.py" \
    --pass "$d/pass1" --pass "$d/pass2" --numbers "$d/numbers.json" 2>&1 | tee -a "$LOG"
  python3 "$REPO/bench/compare/check.py" \
    --pass "$d/pass1" --pass "$d/pass2" --numbers "$d/numbers.json" 2>&1 | tee -a "$LOG"
}

OTHERS=()
render() {
  # render GROUP TEMPLATE DOCNAME  -> report.py writes numbers.json and the
  # document, then check.py recomputes and renders again. With CHECK_README=1
  # (the default) check.py also checks that the rendered readme.md is in the
  # repository's README.md exactly once; a run made before the README has
  # its paragraphs sets CHECK_README=0.
  local name=$1 tpl=$2 docname=$3
  local d=$RESULTS/$name
  local doc=$DOCS/$docname
  local extra_check=()
  if [ "${CHECK_README:-1}" = 1 ]; then
    extra_check=(--contains "$REPO/README.md" "$d/readme.md")
  fi
  say "render $name: report.py"
  python3 "$REPO/bench/compare/report.py" \
    --pass "$d/pass1" --pass "$d/pass2" --numbers "$d/numbers.json" \
    --render "$d/$tpl" "$doc" \
    --render "$d/readme.md.in" "$d/readme.md" "${OTHERS[@]}" 2>&1 | tee -a "$LOG"
  say "render $name: check.py"
  python3 "$REPO/bench/compare/check.py" \
    --pass "$d/pass1" --pass "$d/pass2" --numbers "$d/numbers.json" \
    --render "$d/$tpl" "$doc" \
    --render "$d/readme.md.in" "$d/readme.md" "${OTHERS[@]}" "${extra_check[@]}" 2>&1 | tee -a "$LOG"
}

# The main document sets `cig run` against the other languages, so it
# renders only from passes that ran them.
if grep -q "$(printf '^side\tlua\t')" "$RESULTS/main/pass1/conditions.txt" 2>/dev/null; then
  render main engines.md.in ENGINES-2026-11-25.md
else
  say "note: the main passes have no other languages; the main document is not rendered"
fi

# The first-runs document quotes the other three groups of start passes
# beside its own, each by its numbers.json.
for group in start-one-cpu before before-one-cpu; do
  numbers "$group"
  OTHERS+=(--earlier "$group" "$RESULTS/$group/numbers.json")
done
render start first-runs.md.in ENGINES-FIRST-RUNS-2026-11-25.md

say "done"
