# Benchmarks, 2026-11-25: a faster VM, and native code for lists and records

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The 25 programs of [`bench/compute`](../bench/compute/), each written in CigScript and again in Python (all 25), JavaScript (24: all but `csv`), Lua (21) and Rust (8), timed side by side on one machine, a day after [the first run with stage `c`](BENCHMARKS-2026-11-24.md). Two things changed in that day, and this run measures both:

- **cig's VM runs bytecode faster.** A call between two functions none of whose variables a closure captures is made beside the VM's fast loop, without leaving it for the general path; the loop reads instructions of four bytes; and a run reads what was added to the run index since the last one, not all of it ([#252](https://github.com/otmof-ops/cig-script/issues/252)).
- **Stage `c` compiles more of a script.** [cigcompile](https://github.com/otmof-ops/cig-compile) gives native code to a loop over a list, to `push` and to a record's fields, which it reads and writes where they lie; and an instruction it has no machine code for costs one call of the runtime, not a trip through the interpreter's loop.

**This is a run of cig's source, not of a release.** None of it is in a release yet, so cig is built here from commit `ed7ad3a` and cigcompile from commit `b0a287b`, cig as its release workflow builds it for Linux (`cargo build --release --target x86_64-unknown-linux-musl`). It is the machine of the run of 2026-11-24, on the same core, with the same harness, the same programs and the same builds of Lua, LuaJIT, CPython, Node and Rust, so [one section](#since-the-last-paper) sets the two runs side by side. Within this one, every side ran in the same rounds.

Every number of the comparison is made from the samples in [`bench/results/2026-11-25/`](../bench/results/2026-11-25/) by [`bench/compare/report.py`](../bench/compare/report.py), and made again by [`bench/compare/check.py`](../bench/compare/check.py), a second program written from the [specification](../bench/compare/SPEC.md) alone; the two agree on every one.

## In short

- **A stage `c` program takes 0.98 times Lua 5.4's time** at the geometric mean of the 18 compute programs both have: faster on 8, slower on 10. A day earlier it took 1.37 times. Against CPython 3.12 it takes 0.48 of the time (it took 0.66; faster on 20 of 21 programs), 1.14 times LuaJIT's interpreter's (1.60), 1.84 times LuaJIT's with its compiler on (2.54), and 3.26 times Rust's over the 6 programs Rust has (4.99). Node 18 takes 49.6 ms to start on this machine, which decides its row (0.17).
- **The bytecode alone got faster.** A stage `b` program takes 1.36 times Lua 5.4's time, where it took 1.83, and 0.63 of CPython's, where it took 0.84.
- **Records and lists are native.** `nbody`, whose loops read and write records, takes 0.56 of its stage `b` time, where it took 1.00 (it had no native code); `sieve`, whose loops walk a list, 0.53, where it took 0.78. The loops of arithmetic were native already: `mandel` 0.24, `fib` 0.30, `fannkuch` 0.32 and `loop` 0.54, each against a stage `b` program that is itself faster than it was.
- **Over all 21 compute programs a stage `c` program takes 0.76 of stage `b`'s time**: faster on 9, level (within 3%) on 11, slower on 1. 14 programs have native code now (8 had); the ones it does not speed up are loops of strings, of dictionary lookups and of calls, where one instruction's work is the runtime's either way. The one that comes out slower, `json-deep`, has no native code at all: its two programs are the same file ([below](#stage-c-against-stage-b)).
- **A stage `b` program takes 0.48 of `cig run`'s time**, and a stage `c` program 0.36.
- **Start-up is half what it was.** `hello` takes 2.2 ms as a stage `b` program, 2.2 as a stage `c` one and 2.7 under `cig run`, where it took 4.1, 4.0 and 5.5. CPython takes 8.2 ms, Lua 1.7 and Node 49.6.

## Since the last paper

Each side's median time in this run divided by its median in [the run of 2026-11-24](BENCHMARKS-2026-11-24.md). Under 1, the side took less time than it did a day earlier.

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 0.91 | 0.74 | 0.74 | 0.97 | 1.03 | 0.99 | 0.99 | 1.01 | – |
| `csv` | 0.90 | 0.94 | 0.94 | – | – | – | 1.00 | – | – |
| `deltablue` | 0.60 | 0.60 | 0.66 | 0.88 | 1.00 | 0.97 | 0.98 | 1.01 | – |
| `dispatch` | 0.93 | 0.73 | 0.73 | 1.01 | 0.99 | 0.99 | 0.99 | 1.01 | – |
| `fannkuch` | 0.98 | 0.75 | 0.80 | 1.00 | 0.99 | 1.00 | 1.00 | 1.01 | – |
| `fib` | 0.90 | 0.58 | 0.65 | 0.98 | 0.98 | 0.91 | 0.99 | 1.01 | 0.87 |
| `json` | 0.95 | 0.94 | 0.93 | – | – | – | 0.99 | 1.01 | – |
| `json-deep` | 0.57 | 0.64 | 0.68 | – | – | – | 0.99 | 1.01 | – |
| `lists` | 0.86 | 0.86 | 0.83 | 1.00 | 0.99 | 0.98 | 0.99 | 1.02 | – |
| `loop` | 0.92 | 0.61 | 0.65 | 0.99 | 0.97 | 0.89 | 1.00 | 1.02 | 0.93 |
| `mandel` | 0.98 | 0.66 | 0.85 | 0.99 | 0.99 | 0.92 | 0.99 | 1.01 | 0.95 |
| `maps` | 0.89 | 0.91 | 0.91 | 0.99 | 1.02 | 1.01 | 1.00 | 1.01 | – |
| `nbody` | 0.98 | 0.76 | 0.42 | 1.00 | 1.00 | 1.01 | 1.00 | 1.00 | 1.02 |
| `regex-ish` | 0.89 | 0.88 | 0.85 | 0.99 | 1.00 | 0.99 | 1.00 | 1.01 | – |
| `richards` | 0.78 | 0.70 | 0.60 | 1.01 | 1.04 | 0.97 | 1.01 | 1.02 | – |
| `sieve` | 0.97 | 0.81 | 0.56 | 1.01 | 1.02 | 0.97 | 0.99 | 1.02 | 0.96 |
| `spectral` | 0.97 | 0.62 | 0.62 | 1.00 | 1.01 | 0.98 | 1.00 | 1.01 | – |
| `strbuild` | 0.55 | 0.63 | 0.66 | 0.97 | 0.97 | 1.00 | 0.99 | 1.02 | 0.95 |
| `strings` | 0.86 | 0.83 | 0.81 | 0.99 | 0.99 | 1.00 | 1.01 | 1.01 | – |
| `trees` | 0.99 | 0.73 | 0.73 | 1.01 | 0.99 | 1.00 | 1.00 | 1.01 | – |
| `words` | 0.88 | 0.89 | 0.89 | 0.97 | 0.98 | 0.99 | 1.01 | 1.01 | – |
| `hello` | 0.48 | 0.54 | 0.55 | 1.03 | 0.96 | 0.96 | 0.99 | 1.01 | 0.88 |
| `startup-0` | 0.46 | 0.56 | 0.55 | 1.03 | 1.03 | 0.91 | 0.99 | 1.02 | 1.03 |
| `startup-1` | 0.48 | 0.56 | 0.62 | 0.96 | 0.89 | 0.92 | 1.00 | 1.01 | – |
| `startup-10` | 0.46 | 0.55 | 0.55 | – | – | – | 1.00 | 1.01 | – |
| **compute programs, geometric mean** | 0.86 | 0.74 | 0.73 | 0.98 | 1.00 | 0.98 | 1.00 | 1.01 | 0.95 |

- **The other languages are the yardstick.** Lua, LuaJIT, CPython, Node and Rust ran from the same binaries on the same core in both runs, and nothing about them changed: their columns say how far two runs of one thing differ on this machine: within four hundredths wherever the program takes five milliseconds or more, and by more than a tenth on some that take two. Their geometric means are in the last row. What cig's columns show beyond that is cig's.
- **Start-up.** The four start-up programs take about half the time on every cig side: `hello` under `cig run` took 5.5 ms and takes 2.7. Until now a run read the whole run index before it started; it reads what was appended since the last run.
- **`cig run`** runs its script on the tree-walker, which did not change: its column is that start-up alone, about three milliseconds, which is most of a small program's time and nothing in a long one's (`trees` 181.0 ms, where it took 183.3).
- **Calls and the fast loop** are the rest of what the stage `b` column gained: `fib`, `spectral`, `trees`, `closures` and `dispatch` are calls, and the loops of `mandel`, `loop` and `fannkuch` run on the four-byte instructions.
- **Stage `c`** has the VM's gains under it and its own on top: `nbody` and `richards` for records, `sieve` for a loop over a list. `mandel`, `fib`, `loop` and `fannkuch` were native already, and most of what they gained is the start-up.

## Stage c against stage b

Each program's median time as a stage `b` program and as a stage `c` program, the second divided by the first, and what of the program cigcompile gave native code: its top level, so many of its functions, or nothing. The last two rows are the geometric mean of the ratios over every compute program and over the ones with native code, with how many were faster, level (within 3%) and slower.

| program | native | stage b, ms | stage c, ms | stage c ÷ stage b |
|---|---|---:|---:|---:|
| `closures` | none | 15.5 | 15.4 | 1.00 |
| `csv` | none | 21.0 | 21.0 | 1.00 |
| `deltablue` | the top level | 2.6 | 2.6 | 1.02 |
| `dispatch` | none | 13.8 | 13.7 | 0.99 |
| `fannkuch` | the top level | 30.5 | 9.8 | 0.32 |
| `fib` | 1 of 1 functions | 9.7 | 2.9 | 0.30 |
| `json` | none | 31.1 | 31.0 | 1.00 |
| `json-deep` | none | 3.1 | 3.4 | 1.10 |
| `lists` | the top level, 0 of 2 functions | 14.5 | 14.3 | 0.99 |
| `loop` | the top level | 5.7 | 3.0 | 0.54 |
| `mandel` | the top level | 26.5 | 6.3 | 0.24 |
| `maps` | the top level | 12.7 | 12.6 | 0.99 |
| `nbody` | the top level, 2 of 2 functions | 61.0 | 34.0 | 0.56 |
| `regex-ish` | the top level | 11.6 | 10.9 | 0.94 |
| `richards` | the top level | 5.5 | 4.8 | 0.88 |
| `sieve` | the top level | 26.6 | 14.2 | 0.53 |
| `spectral` | the top level, 0 of 3 functions | 24.2 | 24.9 | 1.03 |
| `strbuild` | none | 3.3 | 3.4 | 1.01 |
| `strings` | the top level | 7.0 | 6.8 | 0.97 |
| `trees` | none | 81.3 | 81.3 | 1.00 |
| `words` | the top level | 10.9 | 10.8 | 0.99 |
| **all compute programs** | – | 21 | – | 0.76 (9/11/1) |
| **programs with native code** | – | 14 | – | 0.66 (9/5/0) |

**What gets native code.** `cigcompile build --stage c` compiles a script's bytecode loop by loop to machine code that runs on the same registers cig's VM uses, so native code can sit beside anything: what it has no machine code for, the runtime runs.

- **A loop of arithmetic** on ints, floats and bools, with a list's elements read and written by an int: `mandel`, `loop`, `fannkuch`, `sieve`.
- **A loop over a list** (`for x in xs`), and `push`: the list's elements are read where they lie, and an element that owns memory (a string, a list, a record) is put in its register by the runtime.
- **A record's fields.** A map a script writes as a record has a shape, the runtime keeps for each field instruction the shape it last saw and the field's place, and machine code compares one number and reads or writes the value there: `nbody` 0.56, `richards` 0.88.
- **A function on numbers alone**, compiled whole for the argument types it is called with: `fib`.
- **One instruction the runtime runs for it**: a string built or compared, a builtin called, a dictionary read by a computed key. Machine code calls the runtime for that one instruction and goes on, where it used to hand a run of instructions to the interpreter's loop. Such a call costs about what the interpreter takes for the instruction, so a loop made of them is compiled and comes out level or a little ahead: `strings` 0.97, `words` 0.99, `regex-ish` 0.94, `maps` 0.99, `lists` 0.99.
- **Not yet:** a loop whose work is calls (`closures`, `dispatch`, `trees`, and the inner loops of `spectral`, whose three functions stay bytecode under a native top level: 1.03). A call from native code goes through the VM, which costs more than the interpreter making the call itself, so the compiler leaves such a loop to the bytecode. Nor a type for what a field or an element holds: every `+` on a value read from one still tests its operands.

**None is slower by more than the run can tell.** By its own figures a stage `c` program is never slower than its stage `b` program. One came out past the 3% line here: `json-deep`, at 1.10. It has no native code: its stage `b` and stage `c` programs are one file (one sha256 in [`inputs.tsv`](../bench/results/2026-11-25/pass1/inputs.tsv)), run in the same rounds, so its ratio says what two copies of one program of three milliseconds can differ by in this run. Timed alone with `deltablue` for 201 rounds under each of two other seeds ([`alone/`](../bench/results/2026-11-25/alone/)), the same two files come out at 0.97, the other way round, and `deltablue`, whose native top level saves it less than a run of two milliseconds can show, at 1.00 (1.02 here).

**What the native code itself adds:** the same stage `c` programs run with `CIG_NATIVE=0`, which makes them run their bytecode and never load their native code, take 1.32 times as long at the geometric mean (stage `c` ÷ the same program without its native code: 0.76).

## Against each language

A cell is cig's time divided by the other language's: the geometric mean, over the programs both have, of the ratio of their median times. Under 1, cig took less time. In brackets: how many of those programs cig was faster on, level with (within 3%) and slower on.

| time ÷ theirs | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 3.15 (3/1/14) | 3.64 (3/1/14) | 5.91 (2/0/16) | 1.32 (7/1/13) | 0.48 (14/0/6) | 23.3 (0/0/6) |
| stage a | 3.07 (4/0/14) | 3.55 (4/0/14) | 5.76 (2/0/16) | 1.28 (7/3/11) | 0.46 (14/0/6) | 22.7 (0/0/6) |
| stage b | 1.36 (4/0/14) | 1.57 (5/0/13) | 2.55 (3/1/14) | 0.63 (18/1/2) | 0.22 (18/0/2) | 6.88 (0/0/6) |
| stage c | 0.98 (8/0/10) | 1.14 (9/0/9) | 1.84 (3/1/14) | 0.48 (20/0/1) | 0.17 (19/0/1) | 3.26 (0/0/6) |

- **`cig run`** is cig's tree-walking interpreter, as `cig run script.cig` runs a script.
- **A stage `a` program** is what `cigcompile build` makes by default: cig itself with the script inside it, run by the same interpreter.
- **A stage `b` program** (`cigcompile build --stage b`) runs the script's bytecode on cig's VM.
- **A stage `c` program** (`cigcompile build --stage c`) is a stage `b` program with native code beside its bytecode.

These are the 21 programs that are not start-up tests; start-up has [its own section](#start-up).

## Every program

The median time of each program on each side, in milliseconds, over 21 rounds in each of 2 passes:

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 22.8 | 15.5 | 15.4 | 7.1 | 6.3 | 6.1 | 17.1 | 54.8 | – |
| `csv` | 23.8 | 21.0 | 21.0 | – | – | – | 38.3 | – | – |
| `deltablue` | 4.7 | 2.6 | 2.6 | 1.9 | 1.9 | 1.9 | 8.7 | 49.9 | – |
| `dispatch` | 30.9 | 13.8 | 13.7 | 5.8 | 5.8 | 3.9 | 19.8 | 58.3 | – |
| `fannkuch` | 156.8 | 30.5 | 9.8 | 22.4 | 17.6 | 9.4 | 33.1 | 56.0 | – |
| `fib` | 28.9 | 9.7 | 2.9 | 4.3 | 4.1 | 2.1 | 12.4 | 50.9 | 1.7 |
| `json` | 33.1 | 31.1 | 31.0 | – | – | – | 32.9 | 74.7 | – |
| `json-deep` | 3.9 | 3.1 | 3.4 | – | – | – | 13.2 | 50.7 | – |
| `lists` | 15.5 | 14.5 | 14.3 | 15.8 | 16.5 | 13.7 | 14.9 | 62.0 | – |
| `loop` | 28.5 | 5.7 | 3.0 | 4.3 | 3.2 | 2.1 | 21.7 | 54.7 | 1.8 |
| `mandel` | 180.4 | 26.5 | 6.3 | 16.0 | 11.8 | 2.9 | 52.6 | 53.4 | 2.7 |
| `maps` | 17.3 | 12.7 | 12.6 | 21.0 | 13.9 | 12.8 | 19.0 | 67.2 | – |
| `nbody` | 231.1 | 61.0 | 34.0 | 28.9 | 18.8 | 3.7 | 82.9 | 55.7 | 2.4 |
| `regex-ish` | 15.8 | 11.6 | 10.9 | 31.4 | 28.9 | 25.1 | 18.7 | 104.0 | – |
| `richards` | 11.0 | 5.5 | 4.8 | 2.7 | 2.5 | 1.9 | 10.6 | 52.5 | – |
| `sieve` | 90.2 | 26.6 | 14.2 | 11.2 | 9.4 | 4.6 | 25.0 | 68.0 | 2.4 |
| `spectral` | 97.2 | 24.2 | 24.9 | 11.6 | 8.9 | 2.3 | 35.4 | 57.3 | – |
| `strbuild` | 3.8 | 3.3 | 3.4 | 2.8 | 6.3 | 6.1 | 8.1 | 50.8 | 1.5 |
| `strings` | 10.5 | 7.0 | 6.8 | 13.7 | 10.8 | 8.8 | 12.8 | 60.0 | – |
| `trees` | 181.0 | 81.3 | 81.3 | 55.4 | 28.6 | 19.9 | 55.1 | 62.4 | – |
| `words` | 16.6 | 10.9 | 10.8 | 8.5 | 7.4 | 6.0 | 14.8 | 63.0 | – |
| `hello` | 2.7 | 2.2 | 2.2 | 1.7 | 1.7 | 1.8 | 8.2 | 49.6 | 1.7 |
| `startup-0` | 2.5 | 2.3 | 2.2 | 1.8 | 1.8 | 1.7 | 7.8 | 49.5 | 1.7 |
| `startup-1` | 2.7 | 2.2 | 2.5 | 1.7 | 1.7 | 1.6 | 7.8 | 49.8 | – |
| `startup-10` | 2.5 | 2.2 | 2.2 | – | – | – | 13.7 | 54.6 | – |

From the 10th to the 90th percentile of the same samples:

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 22.4–23.7 | 15.0–16.7 | 14.9–16.7 | 6.8–8.4 | 5.8–7.1 | 5.8–7.4 | 16.8–17.9 | 54.3–59.4 | – |
| `csv` | 22.1–28.2 | 20.0–26.1 | 20.1–23.9 | – | – | – | 37.5–44.6 | – | – |
| `deltablue` | 4.3–5.7 | 2.3–3.3 | 2.2–3.4 | 1.6–2.7 | 1.6–2.7 | 1.6–2.6 | 8.2–10.2 | 49.2–50.8 | – |
| `dispatch` | 30.2–32.3 | 13.4–14.9 | 13.3–15.2 | 5.4–9.7 | 5.4–6.5 | 3.6–4.7 | 19.5–23.2 | 57.5–61.1 | – |
| `fannkuch` | 155.4–163.4 | 30.0–31.8 | 9.5–10.9 | 22.0–23.6 | 16.4–23.2 | 9.1–10.5 | 32.5–34.3 | 55.3–61.3 | – |
| `fib` | 28.4–30.5 | 9.3–10.8 | 2.6–4.2 | 3.9–5.0 | 3.7–7.1 | 1.8–2.9 | 12.1–13.3 | 50.0–57.9 | 1.6–2.6 |
| `json` | 32.8–36.6 | 30.5–32.5 | 30.2–37.1 | – | – | – | 32.1–34.0 | 73.9–79.1 | – |
| `json-deep` | 3.3–6.1 | 2.8–4.1 | 2.8–4.3 | – | – | – | 12.7–14.3 | 50.0–52.4 | – |
| `lists` | 14.5–16.6 | 14.1–19.9 | 14.0–15.4 | 15.3–17.6 | 16.2–17.8 | 13.2–15.1 | 14.2–20.2 | 60.6–63.0 | – |
| `loop` | 27.9–36.2 | 5.3–6.5 | 2.6–4.5 | 4.1–5.3 | 3.0–4.3 | 1.9–3.1 | 21.0–28.5 | 53.9–59.0 | 1.6–2.5 |
| `mandel` | 179.3–188.7 | 26.0–33.4 | 6.0–7.2 | 15.4–17.3 | 11.5–12.7 | 2.6–3.7 | 51.9–53.9 | 52.3–59.2 | 2.4–3.4 |
| `maps` | 16.8–18.2 | 12.1–14.5 | 12.2–14.7 | 20.5–22.0 | 13.4–19.0 | 12.5–14.3 | 18.5–24.0 | 66.4–73.7 | – |
| `nbody` | 229.5–238.6 | 60.3–64.0 | 33.6–35.4 | 27.7–32.0 | 18.3–20.1 | 3.3–4.7 | 80.8–89.6 | 54.8–60.9 | 1.9–3.0 |
| `regex-ish` | 15.1–17.0 | 11.1–14.8 | 10.5–11.5 | 30.9–32.6 | 28.3–29.5 | 24.6–26.5 | 18.2–19.8 | 102.9–110.4 | – |
| `richards` | 10.6–11.9 | 5.2–6.7 | 4.4–6.7 | 2.3–3.9 | 2.2–3.6 | 1.7–2.5 | 10.2–12.2 | 51.3–57.6 | – |
| `sieve` | 89.3–97.6 | 26.2–27.3 | 13.6–15.0 | 10.8–13.7 | 8.8–11.9 | 4.3–5.4 | 24.5–27.3 | 67.0–71.6 | 2.1–3.2 |
| `spectral` | 96.3–103.2 | 23.7–25.8 | 24.2–25.9 | 11.0–12.2 | 8.6–10.4 | 2.0–3.0 | 34.8–36.2 | 56.1–60.4 | – |
| `strbuild` | 3.5–5.2 | 3.1–5.6 | 3.0–4.3 | 2.5–3.5 | 5.9–7.4 | 5.7–6.8 | 7.6–12.3 | 49.8–55.4 | 1.4–2.0 |
| `strings` | 9.9–12.9 | 6.7–11.0 | 6.4–9.1 | 13.4–14.4 | 10.4–11.6 | 8.2–13.1 | 12.2–19.5 | 58.8–65.3 | – |
| `trees` | 180.3–184.2 | 80.3–82.5 | 80.4–86.6 | 54.6–61.2 | 28.1–29.9 | 19.4–21.7 | 54.3–59.6 | 61.4–68.6 | – |
| `words` | 16.1–17.4 | 10.4–12.1 | 10.3–12.9 | 8.2–9.4 | 7.2–8.6 | 5.7–8.2 | 14.0–16.0 | 62.4–66.0 | – |
| `hello` | 2.2–3.4 | 1.9–2.9 | 1.8–3.0 | 1.4–2.4 | 1.4–2.7 | 1.5–2.5 | 7.5–12.7 | 48.0–54.4 | 1.4–2.5 |
| `startup-0` | 2.3–4.1 | 1.9–2.9 | 1.9–3.0 | 1.4–2.4 | 1.5–2.4 | 1.4–2.4 | 7.4–8.7 | 48.5–54.8 | 1.4–2.2 |
| `startup-1` | 2.3–3.7 | 1.9–3.0 | 1.9–3.2 | 1.4–2.3 | 1.5–2.6 | 1.4–2.5 | 7.5–8.7 | 48.6–53.2 | – |
| `startup-10` | 2.2–3.3 | 1.9–3.1 | 1.9–2.8 | – | – | – | 13.2–14.6 | 53.7–58.6 | – |

## Start-up

| time ÷ theirs | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 1.56 (0/0/3) | 1.52 (0/0/3) | 1.55 (0/0/3) | 0.29 (4/0/0) | 0.05 (4/0/0) | 1.57 (0/0/2) |
| stage a | 1.26 (0/0/3) | 1.22 (0/0/3) | 1.25 (0/0/3) | 0.24 (4/0/0) | 0.04 (4/0/0) | 1.27 (0/0/2) |
| stage b | 1.32 (0/0/3) | 1.28 (0/0/3) | 1.31 (0/0/3) | 0.25 (4/0/0) | 0.04 (4/0/0) | 1.36 (0/0/2) |
| stage c | 1.34 (0/0/3) | 1.30 (0/0/3) | 1.33 (0/0/3) | 0.25 (4/0/0) | 0.04 (4/0/0) | 1.31 (0/0/2) |

The four start-up programs, each side's time divided by the other's as above. Their median times are in [Every program](#every-program). `cig run` of `hello` in a new folder each time takes 2.6 ms.

## cig's own sides

Every program on each of cig's sides, a run and a dry run (`--dry-run` writes no run record):

| program | `cig run` | `cig run --dry-run` | stage a | stage a, `--dry-run` | stage b | stage b, `--dry-run` | stage c | stage c, `--dry-run` |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 22.8 | 22.6 | 22.3 | 22.2 | 15.5 | 15.0 | 15.4 | 15.1 |
| `csv` | 23.8 | – | 23.4 | – | 21.0 | – | 21.0 | – |
| `deltablue` | 4.7 | 4.4 | 4.4 | 4.1 | 2.6 | 2.4 | 2.6 | 2.4 |
| `dispatch` | 30.9 | 30.4 | 30.8 | 30.4 | 13.8 | 13.7 | 13.7 | 14.0 |
| `fannkuch` | 156.8 | 156.2 | 156.1 | 156.2 | 30.5 | 30.3 | 9.8 | 9.7 |
| `fib` | 28.9 | 28.7 | 28.2 | 28.0 | 9.7 | 9.3 | 2.9 | 2.7 |
| `json` | 33.1 | 32.8 | 32.9 | 32.6 | 31.1 | 30.3 | 31.0 | 30.3 |
| `json-deep` | 3.9 | 3.5 | 3.2 | 3.2 | 3.1 | 3.0 | 3.4 | 2.9 |
| `lists` | 15.5 | 15.1 | 14.5 | 14.4 | 14.5 | 14.3 | 14.3 | 14.3 |
| `loop` | 28.5 | 27.8 | 27.5 | 27.3 | 5.7 | 5.6 | 3.0 | 2.6 |
| `mandel` | 180.4 | 179.8 | 179.9 | 179.7 | 26.5 | 26.2 | 6.3 | 6.0 |
| `maps` | 17.3 | 16.8 | 17.1 | 16.7 | 12.7 | 12.2 | 12.6 | 12.5 |
| `nbody` | 231.1 | 230.6 | 231.9 | 231.4 | 61.0 | 61.0 | 34.0 | 34.0 |
| `regex-ish` | 15.8 | 15.1 | 15.4 | 15.1 | 11.6 | 11.2 | 10.9 | 10.8 |
| `richards` | 11.0 | 10.8 | 10.6 | 10.3 | 5.5 | 5.4 | 4.8 | 4.4 |
| `sieve` | 90.2 | 89.9 | 88.7 | 88.5 | 26.6 | 26.1 | 14.2 | 13.6 |
| `spectral` | 97.2 | 96.5 | 96.4 | 96.4 | 24.2 | 24.4 | 24.9 | 24.6 |
| `strbuild` | 3.8 | 3.6 | 3.5 | 3.2 | 3.3 | 3.5 | 3.4 | 3.2 |
| `strings` | 10.5 | 10.1 | 10.2 | 10.0 | 7.0 | 7.0 | 6.8 | 6.7 |
| `trees` | 181.0 | 180.1 | 180.8 | 180.4 | 81.3 | 81.3 | 81.3 | 81.1 |
| `words` | 16.6 | 16.3 | 16.0 | 16.0 | 10.9 | 10.7 | 10.8 | 10.6 |
| `hello` | 2.7 | 2.5 | 2.1 | 1.9 | 2.2 | 2.0 | 2.2 | 2.0 |
| `startup-0` | 2.5 | 2.2 | 2.2 | 2.3 | 2.3 | 1.9 | 2.2 | 2.0 |
| `startup-1` | 2.7 | 2.3 | 2.2 | 1.9 | 2.2 | 1.9 | 2.5 | 2.0 |
| `startup-10` | 2.5 | 2.4 | 2.3 | 2.0 | 2.2 | 2.0 | 2.2 | 2.3 |

`cig run` on each engine: the tree-walker, the VM with the script's module already in the tin, and the VM lowering the script first:

| program | `cig run --engine tree` | `cig run --engine vm` | `cig run --engine vm`, empty tin | vm ÷ tree | vm, empty tin ÷ tree |
|---|---:|---:|---:|---:|---:|
| `closures` | 23.1 | 15.5 | 15.5 | 0.67 | 0.67 |
| `csv` | 23.9 | 20.9 | 21.9 | 0.88 | 0.92 |
| `deltablue` | 4.8 | 3.0 | 3.6 | 0.63 | 0.75 |
| `dispatch` | 30.6 | 13.6 | 14.2 | 0.44 | 0.46 |
| `fannkuch` | 156.8 | 30.8 | 31.9 | 0.20 | 0.20 |
| `fib` | 28.8 | 10.3 | 10.0 | 0.36 | 0.35 |
| `json` | 33.1 | 31.5 | 31.4 | 0.95 | 0.95 |
| `json-deep` | 3.8 | 3.5 | 3.9 | 0.94 | 1.04 |
| `lists` | 15.5 | 15.5 | 16.0 | 1.00 | 1.04 |
| `loop` | 28.2 | 6.3 | 6.3 | 0.22 | 0.22 |
| `mandel` | 179.9 | 26.7 | 27.1 | 0.15 | 0.15 |
| `maps` | 17.7 | 13.5 | 14.0 | 0.76 | 0.79 |
| `nbody` | 231.4 | 60.2 | 60.8 | 0.26 | 0.26 |
| `regex-ish` | 15.5 | 11.5 | 11.4 | 0.74 | 0.74 |
| `richards` | 11.2 | 6.0 | 6.5 | 0.54 | 0.58 |
| `sieve` | 90.3 | 27.3 | 27.7 | 0.30 | 0.31 |
| `spectral` | 97.3 | 24.7 | 24.5 | 0.25 | 0.25 |
| `strbuild` | 3.9 | 4.0 | 4.0 | 1.02 | 1.03 |
| `strings` | 10.7 | 7.6 | 7.8 | 0.71 | 0.73 |
| `trees` | 180.6 | 79.5 | 78.3 | 0.44 | 0.43 |
| `words` | 16.6 | 11.5 | 12.1 | 0.69 | 0.73 |
| `hello` | 2.5 | 2.7 | 2.7 | 1.09 | 1.10 |
| `startup-0` | 2.7 | 2.6 | 2.8 | 0.96 | 1.06 |
| `startup-1` | 2.7 | 2.6 | 2.9 | 0.96 | 1.07 |
| `startup-10` | 2.6 | 2.7 | 2.9 | 1.03 | 1.10 |

## The programs in the other languages

Each program in [`bench/ref`](../bench/ref/) is held to one rule: the same program as its CigScript twin, statement for statement. The rule, how the set was checked against it, and what is still not the same are in the [paper of 2026-11-15](BENCHMARKS-2026-11-15.md#the-programs-in-the-other-languages); the references have not changed since. Where the usual way to write a step is native code, the idiomatic version is measured beside the translation:

| program | language | translation, ms | idiomatic, ms | idiomatic ÷ translation | stage b ÷ idiomatic |
|---|---|---:|---:|---:|---:|
| `fannkuch` | CPython | 33.1 | 32.7 | 0.99 | 0.93 |
| `sieve` | CPython | 25.0 | 20.8 | 0.83 | 1.28 |
| `words` | CPython | 14.8 | 14.8 | 1.00 | 0.74 |
| `lists` | Node | 62.0 | 58.5 | 0.94 | 0.25 |
| `maps` | Node | 67.2 | 81.1 | 1.21 | 0.16 |
| `sieve` | Node | 68.0 | 59.9 | 0.88 | 0.44 |
| `sieve` | Rust | 2.4 | 2.1 | 0.87 | 12.8 |

## Memory

The peak resident memory of each program, in MB, read as it exits (the median of every reading, three in each pass). It counts the pages of the program's own files with its allocations. A stage `c` program maps its native code from its own file, read and execute; none of its memory is ever both writable and executable.

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 8.1 | 13.6 | 13.7 | 2.4 | 2.5 | 2.7 | 10.1 | 55.7 | – |
| `csv` | 28.9 | 34.5 | 34.5 | – | – | – | 22.6 | – | – |
| `deltablue` | 8.1 | 13.6 | 16.0 | 2.4 | 2.5 | 2.6 | 10.1 | 49.5 | – |
| `dispatch` | 8.1 | 13.6 | 13.6 | 2.4 | 2.5 | 2.6 | 10.1 | 53.9 | – |
| `fannkuch` | 8.1 | 13.6 | 16.0 | 2.4 | 2.5 | 2.7 | 10.2 | 56.0 | – |
| `fib` | 8.3 | 13.7 | 13.8 | 2.4 | 2.5 | 2.6 | 10.0 | 53.1 | 2.2 |
| `json` | 66.5 | 72.3 | 72.3 | – | – | – | 27.2 | 66.9 | – |
| `json-deep` | 9.2 | 14.8 | 14.8 | – | – | – | 11.9 | 49.9 | – |
| `lists` | 27.3 | 24.4 | 26.6 | 4.2 | 3.4 | 3.6 | 14.1 | 55.9 | – |
| `loop` | 8.1 | 13.6 | 13.8 | 2.4 | 2.4 | 2.6 | 10.0 | 53.7 | 2.2 |
| `mandel` | 8.1 | 13.6 | 16.0 | 2.6 | 2.5 | 2.7 | 10.1 | 53.5 | 2.2 |
| `maps` | 22.1 | 28.4 | 29.5 | 10.4 | 7.9 | 8.1 | 16.3 | 63.3 | – |
| `nbody` | 8.4 | 13.8 | 18.4 | 2.8 | 2.6 | 2.9 | 10.6 | 54.3 | 2.2 |
| `regex-ish` | 30.5 | 36.2 | 34.5 | 15.5 | 15.8 | 16.1 | 23.9 | 87.2 | – |
| `richards` | 8.1 | 13.6 | 16.0 | 2.4 | 2.5 | 2.7 | 10.1 | 53.2 | – |
| `sieve` | 44.0 | 47.5 | 49.6 | 10.5 | 6.4 | 6.6 | 12.3 | 64.7 | 2.4 |
| `spectral` | 8.3 | 13.8 | 13.9 | 2.8 | 2.7 | 2.8 | 10.4 | 55.1 | – |
| `strbuild` | 8.1 | 13.8 | 13.8 | 2.8 | 2.6 | 2.8 | 10.0 | 49.9 | 2.2 |
| `strings` | 25.8 | 27.6 | 28.1 | 9.7 | 7.6 | 7.8 | 14.8 | 61.9 | – |
| `trees` | 10.0 | 15.7 | 15.7 | 6.1 | 7.2 | 7.7 | 11.5 | 61.6 | – |
| `words` | 21.6 | 27.5 | 30.2 | 4.8 | 3.8 | 3.9 | 14.6 | 60.4 | – |
| `hello` | 8.1 | 13.6 | 13.6 | 2.2 | 2.4 | 2.5 | 10.0 | 49.5 | 2.2 |
| `startup-0` | 8.1 | 13.6 | 13.6 | 2.2 | 2.5 | 2.5 | 10.0 | 49.5 | 2.2 |
| `startup-1` | 8.1 | 13.6 | 13.6 | 2.2 | 2.5 | 2.4 | 10.2 | 49.5 | – |
| `startup-10` | 8.3 | 13.6 | 13.6 | – | – | – | 15.5 | 51.2 | – |

## How steady the numbers are

The geometric means of the compute set, pass by pass:

| compute set | `cig run` ÷, pass 1 | `cig run` ÷, pass 2 | stage b ÷, pass 1 | stage b ÷, pass 2 | stage c ÷, pass 1 | stage c ÷, pass 2 |
|---|---:|---:|---:|---:|---:|---:|
| Lua | 3.15 | 3.14 | 1.36 | 1.36 | 0.99 | 0.98 |
| LuaJIT `-joff` | 3.60 | 3.67 | 1.56 | 1.58 | 1.14 | 1.14 |
| LuaJIT | 5.93 | 5.88 | 2.57 | 2.54 | 1.87 | 1.83 |
| CPython | 1.32 | 1.32 | 0.63 | 0.63 | 0.48 | 0.48 |
| Node | 0.48 | 0.47 | 0.22 | 0.22 | 0.17 | 0.16 |
| Rust | 22.8 | 23.8 | 6.79 | 7.05 | 3.28 | 3.32 |

Each side, pass against pass and within each pass ("last third ÷ first third" compares the median of a pair's last third of rounds with its first), and how many programs had a coefficient of variation over 5%:

| side | programs | pass 2 ÷ pass 1 | last third ÷ first third, pass 1 | last third ÷ first third, pass 2 | CV over 5%, pass 1 | CV over 5%, pass 2 |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 25 | 1.00 | 1.00 | 1.00 | 16 | 19 |
| stage a | 25 | 0.99 | 0.99 | 0.99 | 15 | 18 |
| stage b | 25 | 0.98 | 1.00 | 1.00 | 22 | 22 |
| stage c | 25 | 0.98 | 1.02 | 1.01 | 22 | 24 |
| `cig run --dry-run` | 24 | 1.00 | 1.02 | 1.01 | 14 | 17 |
| stage a, `--dry-run` | 24 | 0.99 | 0.99 | 1.01 | 13 | 15 |
| stage b, `--dry-run` | 24 | 0.99 | 1.01 | 1.00 | 19 | 21 |
| stage c, `--dry-run` | 24 | 1.00 | 0.99 | 0.99 | 20 | 22 |
| stage c, `CIG_NATIVE=0` | 25 | 0.99 | 1.03 | 0.98 | 22 | 20 |
| `cig run`, new folder | 1 | 0.98 | 1.05 | 1.12 | 1 | 1 |
| `cig run --engine tree` | 25 | 0.99 | 1.00 | 0.98 | 18 | 18 |
| `cig run --engine vm` | 25 | 0.99 | 1.03 | 1.01 | 20 | 21 |
| `cig run --engine vm`, empty tin | 25 | 0.99 | 1.02 | 1.00 | 21 | 17 |
| Lua | 21 | 0.99 | 0.99 | 1.00 | 18 | 17 |
| LuaJIT `-joff` | 21 | 0.97 | 1.06 | 0.96 | 19 | 20 |
| LuaJIT | 21 | 0.99 | 1.01 | 1.03 | 21 | 21 |
| CPython | 25 | 0.99 | 1.01 | 0.99 | 18 | 19 |
| Node | 24 | 1.00 | 1.00 | 1.01 | 6 | 5 |
| Rust | 8 | 0.94 | 0.97 | 1.02 | 8 | 8 |
| CPython, idiomatic | 3 | 1.00 | 1.00 | 0.98 | 3 | 2 |
| Node, idiomatic | 3 | 1.01 | 1.01 | 0.99 | 1 | 0 |
| Rust, idiomatic | 1 | 1.06 | 1.00 | 1.37 | 1 | 1 |

What moved during the run, and what no longer does:

- **The machine is a laptop**, and its package reached 98 °C in pass 1 and 100 °C in pass 2. The other languages still repeated where their programs are long: pass 2 against pass 1, Lua 0.99 and CPython 0.99. Rust's eight programs take about two milliseconds each, and came out at 0.94.
- **cig's sides no longer slow as its run history grows.** In the run of 2026-11-24 every cig run read the whole run index first, and pass 2 came out at 1.19 of pass 1 for `cig run`, 1.20 for stage `b` and 1.22 for stage `c`. The history grew as it did then, from 126 runs to 5796, and pass 2 against pass 1 is now 1.00, 0.98 and 0.98 ([#252](https://github.com/otmof-ops/cig-script/issues/252)). Stage `c` against stage `b` is 0.76 in pass 1 and 0.76 in pass 2.

The times above pool both passes. A ratio within a few hundredths of 1 is level. The pairs whose medians in the two passes differ by more than 10%:

| program | side | pass 1, ms | pass 2, ms |
|---|---|---:|---:|
| `fib` | stage c | 3.2 | 2.8 |
| `json-deep` | stage c | 3.1 | 3.5 |
| `json-deep` | `cig run --dry-run` | 3.3 | 3.9 |
| `nbody` | Rust | 2.6 | 2.2 |
| `richards` | LuaJIT `-joff` | 2.9 | 2.3 |
| `strbuild` | stage a | 3.5 | 3.8 |
| `strbuild` | stage a, `--dry-run` | 3.5 | 3.2 |
| `strbuild` | stage b, `--dry-run` | 3.6 | 3.1 |
| `hello` | stage b | 2.4 | 2.2 |
| `hello` | stage c, `CIG_NATIVE=0` | 2.6 | 2.2 |
| `hello` | `cig run --engine vm` | 2.9 | 2.5 |
| `startup-0` | stage a | 2.6 | 2.1 |
| `startup-0` | stage c | 2.3 | 2.1 |
| `startup-0` | LuaJIT | 1.8 | 1.6 |
| `startup-1` | `cig run` | 2.6 | 2.9 |
| `startup-1` | stage b | 2.4 | 2.1 |
| `startup-1` | stage c | 2.7 | 2.3 |
| `startup-1` | stage b, `--dry-run` | 2.1 | 1.9 |
| `startup-1` | stage c, `--dry-run` | 2.0 | 2.2 |
| `startup-1` | `cig run --engine tree` | 2.9 | 2.6 |
| `startup-1` | Lua | 1.8 | 1.6 |
| `startup-1` | LuaJIT `-joff` | 2.0 | 1.6 |
| `startup-10` | stage b | 2.4 | 2.1 |
| `startup-10` | stage c, `--dry-run` | 2.5 | 2.1 |

## How it was measured

- **A whole process.** Each time is the wall time of one process, from just before it is started until it has exited and been reaped: the time a person waits. An interpreter's start-up is part of every number; the four start-up programs show it alone.
- **Every run checked.** A run counts only if it exits 0, prints exactly the program's `.expect` file, byte for byte, and writes nothing to stderr but cig's own summary of a run.
- **Rounds in a random order.** A pass is 21 rounds. In each round each of the 424 program-and-side pairs runs once, in an order drawn from a seeded generator, so the clock and the temperature, as they drift, fall on every program and every side alike.
- **Two passes**, 8904 samples in the first and 8904 in the second. A time above is the median of a pair's samples over both.
- **Pinned to one core.** The processor has cores of three kinds (six performance cores, eight efficiency cores, two low-power cores), and the same program takes a different time on each. Every measured process was pinned to one performance core (`--cpus 2`), set in the child before it starts; the builds were not.
- **A folder per program and side**, at the same path for every run. Before each run the files the last run left there are removed, and when that changes the disk, the disk is synced before the clock starts.
- **The run record is in the time.** `cig run` and a compiled program write a record of every run: that is part of what running a script with cig costs, so it is in the headline. The same programs as dry runs are in [cig's own sides](#cigs-own-sides).
- **cig from source, built as a release is.** cig at commit `ed7ad3a` (1.25.0 and what has merged since), built with `cargo build --release --target x86_64-unknown-linux-musl`: a static binary with its own allocator, as the released Linux assets are. cigcompile at commit `b0a287b`, and the programs it builds from the same scripts on that cig, at `--stage a`, `b` and `c`. Every cig side runs with `CIG_SEAL_KEY=file`, `CIG_MAX_STEPS=0`, `CIG_REPORTS=never`, `CIG_DOCTOR=0` and `NO_COLOR=1`.
- **The others as this machine has them.** Lua 5.4.9 built from its source with the release makefile's flags (`-O2`) and LuaJIT 2.1 with its own makefile's defaults; CPython 3.12.3 and Node 18.19.1 as the machine's distribution ships them; Rust 1.99.1 with `-C opt-level=3`. No runtime was tuned or given flags beyond those.

## Conditions

|  | pass 1 | pass 2 |
|---|---|---|
| started (UTC) | 2026-11-25T05:13:38Z | 2026-11-25T05:18:43Z |
| processor | Intel(R) Core(TM) Ultra 7 255H | Intel(R) Core(TM) Ultra 7 255H |
| logical cores | 16 | 16 |
| highest clock, MHz | 5100 | 5100 |
| kernel | 6.17.0-1033-oem | 6.17.0-1033-oem |
| frequency driver | intel_pstate | intel_pstate |
| CPU governor | powersave | powersave |
| energy preference | performance | performance |
| turbo | on | on |
| power profile | performance | performance |
| transparent huge pages | always [madvise] never | always [madvise] never |
| on mains power | yes | yes |
| load average at the start | 1.00 1.12 1.25 | 1.25 1.24 1.26 |
| package °C at the start | 68 | 72 |
| cig's run records at the start | 0 | 3150 |
| harness | Python 3.12.3 | Python 3.12.3 |
| rounds | 21 | 21 |
| seed of the rounds' order | 1 | 2 |
| pinned to CPUs | 2 | 2 |
| `true`, median ms | 1.036 | 1.071 |
| cig's run records at the last round | 2646 | 5796 |
| mean clock at a round's start, lowest, MHz | 931 | 1021 |
| mean clock at a round's start, highest, MHz | 3281 | 2529 |
| package °C at a round's start, highest | 98 | 100 |
| cig | cig 1.25.0 | cig 1.25.0 |
| cigcompile | cigcompile 0.5.1 (makes programs for cig 1.25.0) | cigcompile 0.5.1 (makes programs for cig 1.25.0) |
| Lua | Lua 5.4.9  Copyright (C) 1994-2026 Lua.org, PUC-Rio | Lua 5.4.9  Copyright (C) 1994-2026 Lua.org, PUC-Rio |
| LuaJIT | LuaJIT 2.1.1794386141 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/ | LuaJIT 2.1.1794386141 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/ |
| CPython | Python 3.12.3 | Python 3.12.3 |
| Node | v18.19.1 | v18.19.1 |
| rustc | rustc 1.99.1 (58cf8f9c3 2026-10-13) | rustc 1.99.1 (58cf8f9c3 2026-10-13) |
| sha256 of `cig` | `7f2f620e1ac217edb5c0627a324064dee3172c19ba4c0942f1bfed6d167d6649` | `7f2f620e1ac217edb5c0627a324064dee3172c19ba4c0942f1bfed6d167d6649` |
| sha256 of `cigcompile` | `e34b73b0d363abc85b71328ce62b06e11d3bc7270036102d49031236f60f11f7` | `e34b73b0d363abc85b71328ce62b06e11d3bc7270036102d49031236f60f11f7` |
| sha256 of `lua` | `e00e7038402bdb1adaac94c6b88d4f3f201accc6f84d28e3f918dbc6d428e56e` | `e00e7038402bdb1adaac94c6b88d4f3f201accc6f84d28e3f918dbc6d428e56e` |
| sha256 of `luajit` | `e505ab009c3220cd4d7fd9936c0926d4a01ba885001e0ac4e0ec768473d87872` | `e505ab009c3220cd4d7fd9936c0926d4a01ba885001e0ac4e0ec768473d87872` |
| sha256 of `python3` | `e50d468e8b0adfb05733f5b87b3cff34829c4a8c1aea50c865aa8bdfe4bb150f` | `e50d468e8b0adfb05733f5b87b3cff34829c4a8c1aea50c865aa8bdfe4bb150f` |
| sha256 of `node` | `f3f93db342d5ac5bb61656d0599a603a73779e98befd9342171e550002725f4d` | `f3f93db342d5ac5bb61656d0599a603a73779e98befd9342171e550002725f4d` |
| sha256 of `rustc` | `b53b780f77f952062f8ca8a3b70aef2390b1e45ad91939be94afdc9543026853` | `b53b780f77f952062f8ca8a3b70aef2390b1e45ad91939be94afdc9543026853` |

## Run it again

```sh
# one pass (every side whose tool is named; a missing one is said and left out)
python3 bench/compare/compare.py --out pass1 --seed 1 --rounds 21 \
  --cig ./cig --cigcompile ./cigcompile --runtime ./cig \
  --lua ./lua --luajit ./luajit --python python3 --node node --rustc ./rustc --cpus 2
# this file's numbers, made and checked
python3 bench/compare/report.py --pass bench/results/2026-11-25/pass1 --pass bench/results/2026-11-25/pass2 \
  --numbers bench/results/2026-11-25/numbers.json \
  --earlier 2026-11-24 bench/results/2026-11-24/numbers.json \
  --earlier alone bench/results/2026-11-25/alone/numbers.json \
  --render bench/results/2026-11-25/paper.md.in docs/BENCHMARKS-2026-11-25.md
python3 bench/compare/check.py --pass bench/results/2026-11-25/pass1 --pass bench/results/2026-11-25/pass2 \
  --numbers bench/results/2026-11-25/numbers.json \
  --earlier 2026-11-24 bench/results/2026-11-24/numbers.json \
  --earlier alone bench/results/2026-11-25/alone/numbers.json \
  --render bench/results/2026-11-25/paper.md.in docs/BENCHMARKS-2026-11-25.md
```

## What this does not show

- **A release.** cig and cigcompile are built from `main`. The released cig 1.25.0 has none of this; the next release that carries it is measured again, as released.
- **One machine.** A 2025 laptop processor, on mains power, every run on one performance core: the machine of the run of 2026-11-24, and not the one of 2026-11-15, which was a 2018 laptop on battery, unpinned, with a newer CPython and Node. Neither that run's times nor its ratios against other languages carry over.
- **Two runs set side by side are two runs.** The table in [Since the last paper](#since-the-last-paper) divides a median of this run by a median of another. The other languages' columns are there to show what that is worth: where they are not 1.00, nothing changed but the day.
- **Small programs, timed whole.** Most of these programs run for a few milliseconds to a few tens, so start-up is a large part of most of these times. A program whose loop takes a tenth of its stage `b` time is not ten times as fast as a process: `mandel`'s whole process takes 0.24 of its stage `b` time with its loops native.
- **Native code where it does not pay yet.** 14 of the 21 compute programs have native code and 9 are faster for it. The rest wait for what comes next: a call from native code that does not go through the VM, types for what a field and an element hold, strings.
- **Not every program in every language.** Rust has eight of the 25 programs; Lua has no `json`, `json-deep`, `csv` or `startup-10`; Node has no `csv`. The three counts beside each geometric mean add up to the programs it covers.
- **Memory is the peak resident set**, read when the process exits.
