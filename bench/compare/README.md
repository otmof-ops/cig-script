# `bench/compare`: cig against other languages

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The programs of [`bench/compute`](../compute/), timed on cig and on the same programs written in other languages ([`bench/ref`](../ref/)), with every output checked and every number of the published papers made from the raw samples by a script and made again by a second script written apart from it.

| file | what it does |
|---|---|
| `compare.py` | runs the comparison: every program on every side, every run's output checked, rounds in a random order, every sample kept |
| `report.py` | computes every number from a run's samples, writes `numbers.json`, and renders a document's template |
| `check.py` | computes every number again and renders every template again, sharing no code with `report.py`; the documents are right when the two agree byte for byte |
| `SPEC.md` | what `report.py` and `check.py` are both written to: every number, table and format, defined |

## Run it

```sh
# one pass: 21 rounds, every side whose tool is installed (a missing one is named and left out)
python3 bench/compare/compare.py --out pass1 --seed 1 \
  --cig ./cig --cigcompile ./cigcompile \
  --lua lua5.4 --luajit luajit --python python3 --node node --rustc rustc

# the numbers and a document, from one pass or more
python3 bench/compare/report.py --pass pass1 --pass pass2 --numbers numbers.json \
  --render paper.md.in paper.md

# the same, made again and compared
python3 bench/compare/check.py --pass pass1 --pass pass2 --numbers numbers.json \
  --render paper.md.in paper.md
```

`--earlier NAME numbers.json` (both programs) names the `numbers.json` of another run, so a document can quote that run's numbers exactly (`{{earlier:NAME:KEY}}`) and set this run beside it (`{{table:since:NAME}}`: each side's median time in this run over its median in that one). Whether two runs can be set side by side is for the document to say: the same machine, the same core and the same builds of the other languages make their columns a yardstick.

`--only fib,nbody` and `--sides cig,python` narrow a run. `--runtime PATH` passes a cig to every `cigcompile build` as `--runtime PATH` and sets `CIGCOMPILE_RUNTIME` for it, so the programs are built on the cig named rather than the `--cig` one (the default is `--cig`, so every cig side is one cig). `--cpus LIST` pins every measured process to a `taskset -c`-style list of CPUs (`2`, `0-5`, `0-3,8`); the pinning is done with `os.sched_setaffinity` in the child before `exec`, so the harness is not in the measurement, and the builds are not pinned. Without `--cpus` no process is pinned and `conditions.txt` says `(not pinned)`; the list, when given, is recorded in `conditions.txt` as the `cpus` line. A pass writes `samples.tsv` (every timed run, with the CPU time it used), `checks.tsv` (each program and side: `ok`, `WRONG` or a failed build), `memory.tsv` (three readings of each pair's peak memory), `rounds.tsv` (each round's start, clock, temperature and cig's run history), `inputs.tsv` (the sha256 of every program, expected output, reference and built program), `native.tsv` (for each program built at `--stage c`: the stage it came out as, how many of its functions got native code and whether its top level did) and `conditions.txt` (the machine, its power settings, whether the measured processes were pinned to CPUs, every tool's version and sha256, and what starting a process costs there). A pass with no stage `c` build has no `native.tsv`, which is a valid pass without the new file or the new sides.

`--rest R` rests after each timed run for R times the run's own time (at most two seconds), and `--cool C` holds each round until the CPU package is below C degrees (at most a minute). Neither is timed. They are for a pass that mixes runs of very different lengths on one pinned CPU: a run of most of a second at full load leaves the core hot and its clock low, and a run of two milliseconds timed straight after it can take several times as long. `--warm W` keeps the first measured CPU busy for W milliseconds before each timed run and starts the run from there. It is for a pass of very short runs: a run of two milliseconds started on an idle core is timed mostly by how far that core's clock had fallen, and its samples come out in two groups, one several times the other. All three are off unless asked for, and a pass made with one says so in `conditions.txt`.

## How a number is made

- **A whole process.** From just before a process is started until it has exited and been reaped: the time a person waits. An interpreter's start-up is part of it; nothing is subtracted. The wait blocks on the process's pidfd: `Popen.wait(timeout)` polls with sleeps that grow from 0.5 to 50 ms, which would land on every run.
- **Every run checked.** A run counts only if it exits 0, prints exactly the program's `.expect` file and writes nothing to stderr. A program and side that fails once, in any round, gets no time at all.
- **A folder per program and side**, at the same path for every run, as a project is. The script is copied in once; before a run the files the last run left are removed. When that changes the disk, the disk is synced before the clock starts, so no run pays for the harness's writes. A run's output is caught in memory (`/dev/shm`).
- **Rounds in a random order.** In each round every program runs once on every side, all of them in an order drawn from a seeded generator, so a machine that drifts falls on every program and every side alike, and cig's run history, which grows as cig runs and slows only cig's sides, falls on each of their programs alike.
- **The same program in every language.** Each reference does the same work as its CigScript program, statement for statement: a loop where the CigScript program loops, the language's standard library call where it calls a builtin, the same data structures. Where the usual way to write a step in that language does it in native code, the idiomatic version is in `bench/ref/idiom/` and is measured beside the translation. Python's references keep their code in a `main()` (CPython runs a module's top level more slowly), and are run as `main.py`, since `csv.py` and `json.py` would otherwise import themselves.

The sides: `cig run`; `--dry-run` of the same (no run record); a stage `a`, a stage `b` and a stage `c` program built by `cigcompile` (and their dry runs); `stage-c-off`, the stage `c` program run with `CIG_NATIVE=0` in its environment, so it runs its bytecode and never loads its native code (what the native code itself adds); Lua; LuaJIT with its compiler off (`-joff`) and on; CPython; Node; Rust (`-C opt-level=3`, what `cargo build --release` uses). A cig whose `cig run` takes `--engine` adds three more: `cig-tree` and `cig-vm` (`cig run --engine tree` and `--engine vm`), and `cig-vm-cold`, the VM with nothing in the tin, so every run lowers its script; each of the three keeps a cig home of its own, so one's tin is not another's and all carry the same run history. A cig whose `--engine` takes `auto` (the default; cig-script#259) adds three more, each a bare `cig run SCRIPT.cig` with a home of its own: `cig-auto`, with the module kept (after the checked run the script is run once more, not timed, so a cig that lowers a script after its second run has kept the module too, and every timed run is on the VM); `cig-auto-cold`, with the modules folder removed before every timed run, so each is the first run of a script the tin has not seen; and `cig-auto-second`, with the folder put back before every timed run as the script's first run left it (a copy the harness keeps from the checked run), so each timed run is a script's second. **The measured time of any of them** is the wall time of the `cig run` process from just before it is started until it has exited and been reaped: where that run leaves a `cig __lower` behind, starting it is in that time (it happens before `cig` returns), and the lowering after `cig` has exited is not. The harness waits between runs until no `cig __lower` is running, and that wait is not part of any timed run. On such a cig `cig run` itself (the sides `cig`, `cig-dry` and `cig-first`) is timed with the script's module kept: each is run once more before the rounds and the lowering waited for.

A build at `--stage c` whose script has no function that compiles to native code exits 0 and comes out as a stage `b` program with a note on stderr (or `warnings` in `--json`); the program is still timed on the `stage-c` (and `-dry` and `-off`) sides, which is what a person who asked for stage `c` gets. The `native` table of the paper lists each program's stage-c-ness and its ratio to stage `b`.
