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

`--only fib,nbody` and `--sides cig,python` narrow a run. A pass writes `samples.tsv` (every timed run, with the CPU time it used), `checks.tsv` (each program and side: `ok`, `WRONG` or a failed build), `memory.tsv` (three readings of each pair's peak memory), `rounds.tsv` (each round's start, clock, temperature and cig's run history), `inputs.tsv` (the sha256 of every program, expected output, reference and built program) and `conditions.txt` (the machine, its power settings, every tool's version and sha256, and what starting a process costs there).

## How a number is made

- **A whole process.** From just before a process is started until it has exited and been reaped: the time a person waits. An interpreter's start-up is part of it; nothing is subtracted. The wait blocks on the process's pidfd: `Popen.wait(timeout)` polls with sleeps that grow from 0.5 to 50 ms, which would land on every run.
- **Every run checked.** A run counts only if it exits 0, prints exactly the program's `.expect` file and writes nothing to stderr. A program and side that fails once, in any round, gets no time at all.
- **A folder per program and side**, at the same path for every run, as a project is. The script is copied in once; before a run the files the last run left are removed. When that changes the disk, the disk is synced before the clock starts, so no run pays for the harness's writes. A run's output is caught in memory (`/dev/shm`).
- **Rounds in a random order.** In each round every program runs once on every side, all of them in an order drawn from a seeded generator, so a machine that drifts, and cig's run history, which grows as cig runs, fall on every program and every side alike.
- **The same program in every language.** Each reference does the same work as its CigScript program, statement for statement: a loop where the CigScript program loops, the language's standard library call where it calls a builtin, the same data structures. Where the usual way to write a step in that language does it in native code, the idiomatic version is in `bench/ref/idiom/` and is measured beside the translation. Python's references keep their code in a `main()` (CPython runs a module's top level more slowly), and are run as `main.py`, since `csv.py` and `json.py` would otherwise import themselves.

The sides: `cig run`; `--dry-run` of the same (no run record); a stage `a` and a stage `b` program built by `cigcompile` (and their dry runs); Lua; LuaJIT with its compiler off (`-joff`) and on; CPython; Node; Rust (`-C opt-level=3`, what `cargo build --release` uses).
