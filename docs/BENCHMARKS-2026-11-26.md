# Benchmarks, 2026-11-26: `cig run` on the VM, and a VM that got faster under the native code

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The 25 programs of [`bench/compute`](../bench/compute/), each written in CigScript and again in Python (all 25), JavaScript (24: all but `csv`), Lua (21) and Rust (8), timed side by side on one machine, fifteen hours after [the run of 2026-11-25](BENCHMARKS-2026-11-25.md): this one began at 20:02 UTC on the 25th, which was the 26th where it was made, and is named for that. Four things changed between the two runs, and this run measures them:

- **`cig run` runs a script on the VM.** The default engine is `auto` ([#259](https://github.com/otmof-ops/cig-script/issues/259), cig-script#285 and #287): the tree-walker runs a script's first two runs, the second leaves the lowering to a process of its own, and from the third the VM runs the script from the module it kept. `cig run` in this paper is that third run and every one after it: each program was run twice before the rounds. In the run of 2026-11-25 `cig run` was the tree-walker, which is `cig run --engine tree` here.
- **The VM's fast loop counts a loop turn's steps in one instruction**, where a turn had two, and counting is a subtraction and a branch (cig-script#280 and #286). Steps are counted whether or not a run has a step budget; these runs have none.
- **The VM's fast loop runs `for x in xs`** (cig-script#286). A turn of a loop over a list took the VM's general path; it stays in the fast loop now.
- **Stage `c` frees a loop's scratch registers once**, where the loop is entered, and writes them with a plain store from then on, where every write tested the register first ([cig-compile#115](https://github.com/otmof-ops/cig-compile/pull/115)).

**This is a run of cig's source, not of a release.** None of it is in a release yet, so cig is built here from commit `ca5236b` and cigcompile from commit `6882f02`, each as its release workflow builds it for Linux (`cargo build --release --target x86_64-unknown-linux-musl`). cigcompile at that commit lowers a script with cig-script at `da5cd6f`; between that commit and `ca5236b` the lowering, the bytecode and the VM differ by nothing that runs (names for the value tags and a set-of-tags type were added, which nothing reads yet). It is the machine of the run of 2026-11-25, on the same core, with the same programs and the same builds of Lua, LuaJIT, CPython, Node and Rust, so [one section](#since-the-last-paper) sets the two runs side by side. Within this one, every side ran in the same rounds.

Every number of the comparison is made from the samples in [`bench/results/2026-11-26/`](../bench/results/2026-11-26/) by [`bench/compare/report.py`](../bench/compare/report.py), and made again by [`bench/compare/check.py`](../bench/compare/check.py), a second program written from the [specification](../bench/compare/SPEC.md) alone; the two agree on every one.

## In short

- **`cig run` takes 1.36 times Lua 5.4's time** at the geometric mean of the 18 compute programs both have, where the tree-walker took 3.15 in the run before, and 0.62 of CPython 3.12's, where it took 1.32: faster than CPython on 19 of 21 programs, where it was faster on 7. It runs the bytecode a stage `b` program runs, on the same VM: a stage `b` program takes 0.96 of its time.
- **A stage `c` program takes 0.94 times Lua 5.4's time**: faster on 8 of 18, slower on 10. In the run before it took 0.98 times. Against CPython 3.12 it takes 0.45 of the time (it took 0.48), 1.09 times LuaJIT's interpreter's (1.14), 1.79 times LuaJIT's with its compiler on (1.84), and 3.12 times Rust's over the 6 programs Rust has (3.26). Node 18 takes 48.8 ms to start on this machine, which decides its row (0.16).
- **The bytecode alone got faster again.** A stage `b` program takes 1.31 times Lua 5.4's time, where it took 1.36, and 0.60 of CPython's, where it took 0.63. `sieve`, which ends by walking a list of 300,001 elements, takes 20.8 ms as a stage `b` program, where it took 26.6.
- **Over all 21 compute programs a stage `c` program takes 0.76 of stage `b`'s time**, as it did in the run before (0.76): faster on 9, level (within 3%) on 9, slower on 3. `mandel` takes 0.18 of its stage `b` time, where it took 0.24: chiefly, its loops no longer test a register before every write.
- **Three programs come out slower with their native code than as bytecode:** `maps` (1.04), `strings` (1.03) and `words` (1.05), where in the run before they were level (0.99, 0.97, 0.99). Timed alone, `maps` and `words` are 1.05 and 1.05, and `strings` is at the 3% line. Their loops are strings and dictionary lookups, which the runtime does for the native code one call at a time ([below](#stage-c-against-stage-b)).
- **Start-up:** `hello` takes 2.1 ms as a stage `b` program, 2.0 as a stage `c` one and 2.4 under `cig run`. CPython takes 7.7 ms, Lua 1.6 and Node 48.8.

## Since the last paper

Each side's median time in this run divided by its median in [the run of 2026-11-25](BENCHMARKS-2026-11-25.md). Under 1, the side took less time than it did in that run.

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 0.66 | 0.97 | 0.98 | 0.99 | 0.96 | 0.95 | 0.99 | 0.99 | – |
| `csv` | 0.87 | 0.95 | 0.97 | – | – | – | 0.99 | – | – |
| `deltablue` | 0.60 | 0.94 | 0.84 | 0.94 | 0.98 | 0.91 | 0.98 | 0.98 | – |
| `dispatch` | 0.43 | 0.97 | 0.96 | 0.95 | 0.95 | 0.96 | 1.00 | 0.98 | – |
| `fannkuch` | 0.18 | 0.94 | 0.95 | 0.99 | 0.99 | 0.98 | 0.99 | 0.98 | – |
| `fib` | 0.33 | 0.95 | 0.94 | 0.98 | 0.94 | 0.96 | 0.99 | 0.99 | 0.89 |
| `json` | 0.92 | 0.99 | 0.99 | – | – | – | 0.99 | 0.99 | – |
| `json-deep` | 0.85 | 0.93 | 0.87 | – | – | – | 0.97 | 0.98 | – |
| `lists` | 0.96 | 0.98 | 0.99 | 0.99 | 0.99 | 1.00 | 1.00 | 0.98 | – |
| `loop` | 0.19 | 0.86 | 0.85 | 0.96 | 0.97 | 0.98 | 0.98 | 0.99 | 0.96 |
| `mandel` | 0.14 | 0.90 | 0.68 | 0.98 | 0.99 | 1.00 | 1.00 | 0.99 | 0.96 |
| `maps` | 0.77 | 0.97 | 1.01 | 0.99 | 0.97 | 0.99 | 0.99 | 0.99 | – |
| `nbody` | 0.24 | 0.93 | 0.97 | 0.98 | 0.98 | 0.93 | 0.99 | 0.99 | 0.86 |
| `regex-ish` | 0.72 | 0.97 | 0.98 | 1.00 | 0.99 | 0.99 | 1.00 | 0.99 | – |
| `richards` | 0.53 | 0.94 | 0.92 | 0.88 | 0.96 | 0.90 | 1.00 | 0.97 | – |
| `sieve` | 0.24 | 0.78 | 1.01 | 1.00 | 0.97 | 0.98 | 0.99 | 0.99 | 0.97 |
| `spectral` | 0.24 | 0.96 | 0.94 | 0.99 | 0.98 | 0.91 | 0.99 | 0.98 | – |
| `strbuild` | 0.92 | 0.97 | 0.94 | 0.94 | 0.96 | 0.96 | 0.97 | 0.98 | 0.94 |
| `strings` | 0.70 | 0.97 | 1.03 | 0.99 | 0.98 | 0.98 | 0.99 | 0.98 | – |
| `trees` | 0.41 | 0.98 | 0.97 | 0.98 | 0.99 | 0.99 | 0.99 | 0.98 | – |
| `words` | 0.64 | 0.90 | 0.95 | 0.99 | 1.00 | 0.96 | 0.98 | 0.99 | – |
| `hello` | 0.92 | 0.93 | 0.90 | 0.94 | 0.90 | 0.93 | 0.94 | 0.98 | 0.95 |
| `startup-0` | 0.94 | 0.87 | 0.97 | 0.85 | 0.89 | 0.98 | 0.99 | 0.98 | 0.89 |
| `startup-1` | 0.93 | 0.94 | 0.82 | 0.93 | 0.94 | 0.95 | 0.98 | 0.98 | – |
| `startup-10` | 0.98 | 0.91 | 0.96 | – | – | – | 0.98 | 0.99 | – |
| **compute programs, geometric mean** | 0.47 | 0.94 | 0.94 | 0.97 | 0.98 | 0.96 | 0.99 | 0.98 | 0.93 |

- **The other languages are the yardstick.** Lua, LuaJIT, CPython, Node and Rust ran from the same binaries on the same core in both runs, and nothing about them changed: their columns say how far two runs of one thing differ on this machine, and their geometric means are in the last row. What cig's columns show beyond that is cig's.
- **`cig run`** changed engines: its column is the VM in this run over the tree-walker in that one. `trees` takes 74.4 ms, where it took 181.0; `mandel` 24.5, where it took 180.4. A stage `a` program, which still runs its script on the tree-walker, takes 3.11 times Lua's time, where it took 3.07: nothing changed for it.
- **Stage `b`** is the VM's own gains, beyond what the yardstick moved. The two changes to its fast loop landed together, and this run cannot give each its share. By what the programs do: `loop` and `mandel` are loops of arithmetic and little else, where a turn's steps are a good part of the turn, and `sieve` and `words` have the loops over a list.
- **Stage `c`** has the VM's gains where its code is bytecode, and its own on `mandel`, whose loops write their scratch registers with a plain store (cig-compile#115 measured that alone, on one runtime, at 0.66 of `mandel`'s time). `sieve`'s native code walked its list already, so its stage `c` time did not move while its stage `b` time did.

## Stage c against stage b

Each program's median time as a stage `b` program and as a stage `c` program, the second divided by the first, and what of the program cigcompile gave native code: its top level, so many of its functions, or nothing. The last two rows are the geometric mean of the ratios over every compute program and over the ones with native code, with how many were faster, level (within 3%) and slower.

| program | native | stage b, ms | stage c, ms | stage c ÷ stage b |
|---|---|---:|---:|---:|
| `closures` | none | 15.0 | 15.1 | 1.00 |
| `csv` | none | 19.9 | 20.3 | 1.02 |
| `deltablue` | the top level | 2.4 | 2.2 | 0.90 |
| `dispatch` | none | 13.4 | 13.2 | 0.99 |
| `fannkuch` | the top level | 28.7 | 9.3 | 0.32 |
| `fib` | 1 of 1 functions | 9.3 | 2.7 | 0.29 |
| `json` | none | 30.8 | 30.6 | 0.99 |
| `json-deep` | none | 2.9 | 3.0 | 1.02 |
| `lists` | the top level, 0 of 2 functions | 14.3 | 14.2 | 0.99 |
| `loop` | the top level | 4.9 | 2.6 | 0.52 |
| `mandel` | the top level | 23.9 | 4.3 | 0.18 |
| `maps` | the top level | 12.2 | 12.7 | 1.04 |
| `nbody` | the top level, 2 of 2 functions | 56.5 | 32.8 | 0.58 |
| `regex-ish` | the top level | 11.3 | 10.7 | 0.95 |
| `richards` | the top level | 5.2 | 4.4 | 0.86 |
| `sieve` | the top level | 20.8 | 14.4 | 0.69 |
| `spectral` | the top level, 0 of 3 functions | 23.4 | 23.5 | 1.00 |
| `strbuild` | none | 3.3 | 3.2 | 0.98 |
| `strings` | the top level | 6.8 | 7.0 | 1.03 |
| `trees` | none | 79.2 | 79.0 | 1.00 |
| `words` | the top level | 9.8 | 10.3 | 1.05 |
| **all compute programs** | – | 21 | – | 0.76 (9/9/3) |
| **programs with native code** | – | 14 | – | 0.66 (9/2/3) |

**What gets native code.** `cigcompile build --stage c` compiles a script's bytecode loop by loop to machine code that runs on the same registers cig's VM uses, so native code can sit beside anything: what it has no machine code for, the runtime runs.

- **A loop of arithmetic** on ints, floats and bools, with a list's elements read and written by an int: `mandel`, `loop`, `fannkuch`, `sieve`.
- **A loop over a list** (`for x in xs`), and `push`: the list's elements are read where they lie, and an element that owns memory (a string, a list, a record) is put in its register by the runtime.
- **A record's fields.** A map a script writes as a record has a shape, the runtime keeps for each field instruction the shape it last saw and the field's place, and machine code compares one number and reads or writes the value there: `nbody` 0.58, `richards` 0.86.
- **A function on numbers alone**, compiled whole for the argument types it is called with: `fib`.
- **One instruction the runtime runs for it**: a string built or compared, a builtin called, a dictionary read by a computed key. Machine code calls the runtime for that one instruction and goes on. Such a call costs about what the interpreter takes for the instruction, so a loop made of them gains little by being compiled, and since the VM's own loop got faster it can lose: `regex-ish` 0.95, `lists` 0.99, `strings` 1.03, `maps` 1.04, `words` 1.05.
- **Not yet:** a loop whose work is calls (`closures`, `dispatch`, `trees`, and the inner loops of `spectral`, whose three functions stay bytecode under a native top level: 1.00). A call from native code goes through the VM, which costs more than the interpreter making the call itself, so the compiler leaves such a loop to the bytecode. Nor a type for what a field or an element holds: every `+` on a value read from one still tests its operands.

**Two are slower, and a third is at the line.** `maps`, `strings` and `words` come out 1.04, 1.03 and 1.05 of their stage `b` time, past the 3% line, `strings` by a hair. In the run of 2026-11-25 they were 0.99, 0.97 and 0.99. Timed alone for 201 rounds under each of two other seeds ([`alone/`](../bench/results/2026-11-26/alone/)), they come out 1.05, 1.03 and 1.05, and against the same stage `c` program run without its native code (`CIG_NATIVE=0`) 1.05, 1.07 and 1.07.

- **`maps` and `words`** are behind their bytecode by 5% or more on both comparisons: that is their native code, not the run.
- **`strings`** is at the line against its stage `b` program and 7% behind itself without its native code. Those two runs of its bytecode differ between themselves (6.4 ms as a stage `b` program, 6.2 as the stage `c` program without its native code), so all this run says of `strings` is that its native code does not help it.
- **What moved between the two runs.** As stage `b` programs the three take 12.2, 6.8 and 9.8 ms, where they took 12.7, 7.0 and 10.9; as stage `c` programs 12.7, 7.0 and 10.3, where they took 12.6, 6.8 and 10.8. `words` got faster as bytecode by a tenth, with its two loops over a list, and by half that with its native code. `maps` and `strings` moved as bytecode by what the other languages moved between the runs, and did not get faster with their native code.
- **Why it can happen.** The compiler plans each loop from figures of what an instruction costs as machine code, as a call of the runtime and in the interpreter, and by those figures a plan is never worse than the bytecode. The figures were measured against the VM as it was before its fast loop changed. What moved these programs is not separated here; it is [cig-compile#117](https://github.com/otmof-ops/cig-compile/issues/117), with measuring the figures again.
- **The short ones.** `json-deep` has no native code: its stage `b` and stage `c` programs are one file, and it comes out at 1.02 here and 1.01 alone, which is what two copies of one program of three milliseconds differ by. `deltablue`, whose top level is native, comes out at 0.90 here and 0.91 alone.

**What the native code itself adds:** the same stage `c` programs run with `CIG_NATIVE=0`, which makes them run their bytecode and never load their native code, take 1.31 times as long at the geometric mean (stage `c` ÷ the same program without its native code: 0.76).

## Against each language

A cell is cig's time divided by the other language's: the geometric mean, over the programs both have, of the ratio of their median times. Under 1, cig took less time. In brackets: how many of those programs cig was faster on, level with (within 3%) and slower on.

| time ÷ theirs | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 1.36 (4/0/14) | 1.57 (4/1/13) | 2.59 (3/0/15) | 0.62 (19/1/1) | 0.22 (18/1/1) | 6.92 (0/0/6) |
| stage a | 3.11 (4/0/14) | 3.59 (4/0/14) | 5.91 (2/0/16) | 1.28 (8/2/11) | 0.46 (14/0/6) | 24.0 (0/0/6) |
| stage b | 1.31 (4/0/14) | 1.51 (5/0/13) | 2.48 (4/0/14) | 0.60 (20/0/1) | 0.21 (18/1/1) | 6.64 (0/0/6) |
| stage c | 0.94 (8/0/10) | 1.09 (9/0/9) | 1.79 (3/2/13) | 0.45 (20/0/1) | 0.16 (19/0/1) | 3.12 (0/0/6) |

- **`cig run`** is `cig run script.cig` as it runs a script from its third run on: on cig's VM, from the module it kept. The tree-walker, which runs a script's first two runs, is `cig run --engine tree` in [cig's own sides](#cigs-own-sides).
- **A stage `a` program** is what `cigcompile build` makes by default: cig itself with the script inside it, run by the tree-walker.
- **A stage `b` program** (`cigcompile build --stage b`) runs the script's bytecode on cig's VM.
- **A stage `c` program** (`cigcompile build --stage c`) is a stage `b` program with native code beside its bytecode.

These are the 21 programs that are not start-up tests; start-up has [its own section](#start-up).

## Every program

The median time of each program on each side, in milliseconds, over 21 rounds in each of 2 passes:

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 15.1 | 15.0 | 15.1 | 7.0 | 6.0 | 5.9 | 16.9 | 54.2 | – |
| `csv` | 20.7 | 19.9 | 20.3 | – | – | – | 37.9 | – | – |
| `deltablue` | 2.8 | 2.4 | 2.2 | 1.8 | 1.9 | 1.7 | 8.6 | 49.0 | – |
| `dispatch` | 13.2 | 13.4 | 13.2 | 5.5 | 5.5 | 3.8 | 19.7 | 57.0 | – |
| `fannkuch` | 29.0 | 28.7 | 9.3 | 22.2 | 17.4 | 9.2 | 32.8 | 54.8 | – |
| `fib` | 9.4 | 9.3 | 2.7 | 4.2 | 3.8 | 2.0 | 12.4 | 50.2 | 1.6 |
| `json` | 30.6 | 30.8 | 30.6 | – | – | – | 32.5 | 73.8 | – |
| `json-deep` | 3.3 | 2.9 | 3.0 | – | – | – | 12.8 | 49.8 | – |
| `lists` | 14.9 | 14.3 | 14.2 | 15.5 | 16.4 | 13.7 | 14.9 | 60.9 | – |
| `loop` | 5.4 | 4.9 | 2.6 | 4.1 | 3.1 | 2.1 | 21.2 | 53.9 | 1.7 |
| `mandel` | 24.5 | 23.9 | 4.3 | 15.6 | 11.7 | 2.9 | 52.3 | 52.9 | 2.6 |
| `maps` | 13.3 | 12.2 | 12.7 | 20.9 | 13.5 | 12.8 | 18.9 | 66.2 | – |
| `nbody` | 55.8 | 56.5 | 32.8 | 28.2 | 18.5 | 3.4 | 82.3 | 55.4 | 2.1 |
| `regex-ish` | 11.4 | 11.3 | 10.7 | 31.4 | 28.6 | 24.8 | 18.7 | 103.2 | – |
| `richards` | 5.8 | 5.2 | 4.4 | 2.4 | 2.4 | 1.7 | 10.6 | 51.2 | – |
| `sieve` | 21.5 | 20.8 | 14.4 | 11.2 | 9.0 | 4.5 | 24.8 | 67.2 | 2.3 |
| `spectral` | 23.2 | 23.4 | 23.5 | 11.5 | 8.8 | 2.1 | 35.0 | 56.3 | – |
| `strbuild` | 3.5 | 3.3 | 3.2 | 2.6 | 6.0 | 5.8 | 7.8 | 49.6 | 1.4 |
| `strings` | 7.3 | 6.8 | 7.0 | 13.6 | 10.5 | 8.7 | 12.6 | 58.6 | – |
| `trees` | 74.4 | 79.2 | 79.0 | 54.2 | 28.4 | 19.6 | 54.7 | 61.3 | – |
| `words` | 10.6 | 9.8 | 10.3 | 8.4 | 7.4 | 5.7 | 14.5 | 62.3 | – |
| `hello` | 2.4 | 2.1 | 2.0 | 1.6 | 1.6 | 1.6 | 7.7 | 48.8 | 1.6 |
| `startup-0` | 2.4 | 2.0 | 2.1 | 1.5 | 1.6 | 1.7 | 7.7 | 48.5 | 1.5 |
| `startup-1` | 2.6 | 2.1 | 2.0 | 1.5 | 1.6 | 1.6 | 7.7 | 48.6 | – |
| `startup-10` | 2.5 | 2.0 | 2.2 | – | – | – | 13.5 | 53.9 | – |

From the 10th to the 90th percentile of the same samples:

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 14.4–15.9 | 14.5–17.0 | 14.6–20.5 | 6.6–7.6 | 5.7–8.0 | 5.6–6.8 | 16.6–17.8 | 53.6–56.6 | – |
| `csv` | 19.6–25.2 | 19.1–22.5 | 19.4–24.2 | – | – | – | 37.1–38.8 | – | – |
| `deltablue` | 2.4–4.0 | 2.1–2.9 | 1.9–3.2 | 1.5–2.4 | 1.5–2.5 | 1.4–2.3 | 8.1–9.6 | 48.4–51.6 | – |
| `dispatch` | 12.6–14.5 | 13.1–15.2 | 12.9–13.9 | 5.2–6.6 | 5.2–6.2 | 3.5–5.3 | 19.2–21.9 | 56.4–58.6 | – |
| `fannkuch` | 28.4–35.4 | 28.2–31.5 | 9.1–10.3 | 21.9–23.5 | 16.4–24.1 | 8.9–13.8 | 32.4–35.4 | 54.1–61.1 | – |
| `fib` | 8.8–10.8 | 8.9–9.9 | 2.4–3.5 | 3.8–4.8 | 3.6–4.6 | 1.8–2.6 | 12.0–17.9 | 49.2–55.1 | 1.4–2.1 |
| `json` | 30.2–32.3 | 30.3–31.8 | 30.1–31.9 | – | – | – | 31.8–33.5 | 72.7–80.4 | – |
| `json-deep` | 2.9–4.3 | 2.6–3.7 | 2.6–3.8 | – | – | – | 12.4–14.0 | 49.4–50.8 | – |
| `lists` | 14.6–15.6 | 13.9–15.3 | 13.9–15.2 | 15.2–17.0 | 16.0–18.9 | 13.2–21.0 | 14.3–17.9 | 59.8–67.1 | – |
| `loop` | 4.9–6.5 | 4.6–5.5 | 2.4–3.2 | 3.9–5.2 | 2.9–3.8 | 1.7–2.7 | 20.8–22.0 | 53.2–60.7 | 1.5–2.5 |
| `mandel` | 24.0–25.6 | 23.5–24.9 | 4.0–7.6 | 15.2–16.8 | 11.4–18.6 | 2.5–3.9 | 51.6–54.2 | 51.7–58.9 | 2.2–4.1 |
| `maps` | 12.6–14.2 | 11.8–12.9 | 12.2–13.4 | 20.2–21.9 | 13.2–14.2 | 12.3–13.5 | 18.2–19.7 | 65.2–69.1 | – |
| `nbody` | 55.0–59.0 | 55.7–60.7 | 32.0–35.7 | 27.2–30.2 | 18.2–21.3 | 3.1–4.0 | 80.4–86.3 | 53.7–60.0 | 1.9–2.6 |
| `regex-ish` | 10.7–12.8 | 10.8–13.0 | 10.4–11.3 | 30.9–32.1 | 28.1–35.5 | 24.2–26.8 | 18.1–26.3 | 101.9–109.6 | – |
| `richards` | 5.2–7.6 | 4.9–6.8 | 4.1–5.3 | 2.2–3.5 | 2.1–3.4 | 1.5–2.3 | 10.0–16.2 | 50.3–57.8 | – |
| `sieve` | 21.0–22.5 | 20.4–21.8 | 13.6–19.9 | 10.6–16.8 | 8.6–10.6 | 4.1–6.5 | 24.5–26.0 | 66.2–71.7 | 2.0–3.2 |
| `spectral` | 22.5–24.3 | 22.9–24.3 | 23.0–28.8 | 10.9–16.9 | 8.4–9.6 | 1.9–3.0 | 34.4–36.0 | 55.7–58.4 | – |
| `strbuild` | 3.3–4.5 | 2.9–5.0 | 2.9–4.5 | 2.3–3.2 | 5.8–6.6 | 5.6–6.4 | 7.5–9.0 | 49.0–50.9 | 1.2–2.0 |
| `strings` | 6.9–10.7 | 6.4–8.1 | 6.6–8.3 | 13.1–14.5 | 10.1–11.6 | 8.2–9.9 | 12.1–16.9 | 57.9–64.3 | – |
| `trees` | 73.5–77.5 | 78.1–81.5 | 78.4–80.2 | 53.3–57.7 | 28.0–29.7 | 19.2–20.8 | 54.1–56.3 | 60.6–66.8 | – |
| `words` | 10.1–14.1 | 9.6–14.3 | 10.0–11.2 | 8.1–9.4 | 7.1–10.7 | 5.5–7.1 | 13.7–15.8 | 61.4–65.0 | – |
| `hello` | 2.2–3.7 | 1.8–2.7 | 1.8–2.6 | 1.3–2.3 | 1.3–2.3 | 1.3–2.2 | 7.4–13.0 | 47.5–52.0 | 1.2–2.1 |
| `startup-0` | 2.0–3.3 | 1.8–2.9 | 1.8–2.7 | 1.2–2.1 | 1.3–2.2 | 1.3–2.2 | 7.3–8.3 | 47.5–53.8 | 1.3–1.9 |
| `startup-1` | 2.1–3.3 | 1.7–2.7 | 1.8–2.6 | 1.3–2.1 | 1.3–2.2 | 1.3–2.0 | 7.3–8.5 | 47.9–51.8 | – |
| `startup-10` | 2.1–3.3 | 1.7–2.6 | 1.8–2.8 | – | – | – | 13.0–14.4 | 52.9–56.7 | – |

## Start-up

| time ÷ theirs | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 1.61 (0/0/3) | 1.56 (0/0/3) | 1.51 (0/0/3) | 0.28 (4/0/0) | 0.05 (4/0/0) | 1.59 (0/0/2) |
| stage a | 1.29 (0/0/3) | 1.25 (0/0/3) | 1.21 (0/0/3) | 0.23 (4/0/0) | 0.04 (4/0/0) | 1.30 (0/0/2) |
| stage b | 1.33 (0/0/3) | 1.29 (0/0/3) | 1.25 (0/0/3) | 0.23 (4/0/0) | 0.04 (4/0/0) | 1.34 (0/0/2) |
| stage c | 1.32 (0/0/3) | 1.28 (0/0/3) | 1.24 (0/0/3) | 0.23 (4/0/0) | 0.04 (4/0/0) | 1.33 (0/0/2) |

The four start-up programs, each side's time divided by the other's as above. Their median times are in [Every program](#every-program). `cig run` of `hello` in a new folder each time takes 2.5 ms.

## cig's own sides

Every program on each of cig's sides, a run and a dry run (`--dry-run` writes no run record):

| program | `cig run` | `cig run --dry-run` | stage a | stage a, `--dry-run` | stage b | stage b, `--dry-run` | stage c | stage c, `--dry-run` |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 15.1 | 15.0 | 22.0 | 21.9 | 15.0 | 14.7 | 15.1 | 14.6 |
| `csv` | 20.7 | – | 23.3 | – | 19.9 | – | 20.3 | – |
| `deltablue` | 2.8 | 2.5 | 4.2 | 3.8 | 2.4 | 2.1 | 2.2 | 2.0 |
| `dispatch` | 13.2 | 12.9 | 30.4 | 30.4 | 13.4 | 13.4 | 13.2 | 13.6 |
| `fannkuch` | 29.0 | 28.5 | 155.3 | 155.3 | 28.7 | 28.3 | 9.3 | 9.2 |
| `fib` | 9.4 | 9.2 | 27.9 | 27.6 | 9.3 | 9.1 | 2.7 | 2.5 |
| `json` | 30.6 | 30.2 | 33.0 | 32.2 | 30.8 | 30.2 | 30.6 | 30.0 |
| `json-deep` | 3.3 | 3.1 | 3.2 | 3.1 | 2.9 | 2.8 | 3.0 | 2.6 |
| `lists` | 14.9 | 14.6 | 14.3 | 13.9 | 14.3 | 14.1 | 14.2 | 14.2 |
| `loop` | 5.4 | 4.9 | 27.5 | 27.3 | 4.9 | 4.8 | 2.6 | 2.4 |
| `mandel` | 24.5 | 24.2 | 179.9 | 179.5 | 23.9 | 23.7 | 4.3 | 4.1 |
| `maps` | 13.3 | 13.1 | 16.9 | 16.7 | 12.2 | 11.9 | 12.7 | 12.6 |
| `nbody` | 55.8 | 55.6 | 231.1 | 230.8 | 56.5 | 56.1 | 32.8 | 32.3 |
| `regex-ish` | 11.4 | 10.6 | 15.0 | 14.9 | 11.3 | 10.9 | 10.7 | 10.6 |
| `richards` | 5.8 | 5.3 | 10.4 | 10.1 | 5.2 | 5.1 | 4.4 | 4.2 |
| `sieve` | 21.5 | 20.9 | 89.1 | 88.9 | 20.8 | 20.2 | 14.4 | 14.0 |
| `spectral` | 23.2 | 22.8 | 96.7 | 96.3 | 23.4 | 23.5 | 23.5 | 23.3 |
| `strbuild` | 3.5 | 3.4 | 3.2 | 3.3 | 3.3 | 3.0 | 3.2 | 2.9 |
| `strings` | 7.3 | 6.8 | 10.1 | 9.9 | 6.8 | 6.6 | 7.0 | 7.0 |
| `trees` | 74.4 | 74.3 | 181.3 | 180.9 | 79.2 | 78.8 | 79.0 | 78.8 |
| `words` | 10.6 | 10.2 | 16.0 | 15.8 | 9.8 | 9.8 | 10.3 | 10.3 |
| `hello` | 2.4 | 2.2 | 2.0 | 1.8 | 2.1 | 1.9 | 2.0 | 1.8 |
| `startup-0` | 2.4 | 2.2 | 2.0 | 1.9 | 2.0 | 1.8 | 2.1 | 1.9 |
| `startup-1` | 2.6 | 2.1 | 2.0 | 1.8 | 2.1 | 1.8 | 2.0 | 1.9 |
| `startup-10` | 2.5 | 2.3 | 2.1 | 1.8 | 2.0 | 1.9 | 2.2 | 2.0 |

`cig run` on each engine by name: the tree-walker, the VM with the script's module already in the tin, and the VM lowering the script first. `cig run` with no engine named, in the tables above, is the second of these reached by default:

| program | `cig run --engine tree` | `cig run --engine vm` | `cig run --engine vm`, empty tin | vm ÷ tree | vm, empty tin ÷ tree |
|---|---:|---:|---:|---:|---:|
| `closures` | 22.6 | 15.0 | 15.4 | 0.66 | 0.68 |
| `csv` | 23.8 | 20.6 | 20.7 | 0.87 | 0.87 |
| `deltablue` | 4.7 | 2.8 | 3.2 | 0.59 | 0.68 |
| `dispatch` | 30.5 | 13.5 | 13.8 | 0.44 | 0.45 |
| `fannkuch` | 155.6 | 29.0 | 30.3 | 0.19 | 0.19 |
| `fib` | 28.5 | 10.0 | 9.6 | 0.35 | 0.34 |
| `json` | 32.8 | 31.3 | 31.2 | 0.95 | 0.95 |
| `json-deep` | 3.5 | 3.3 | 3.8 | 0.94 | 1.07 |
| `lists` | 15.3 | 14.9 | 15.6 | 0.97 | 1.02 |
| `loop` | 27.6 | 5.3 | 5.5 | 0.19 | 0.20 |
| `mandel` | 180.2 | 24.2 | 25.1 | 0.13 | 0.14 |
| `maps` | 17.2 | 13.1 | 13.5 | 0.76 | 0.79 |
| `nbody` | 232.9 | 55.8 | 56.7 | 0.24 | 0.24 |
| `regex-ish` | 15.4 | 11.3 | 11.5 | 0.73 | 0.75 |
| `richards` | 10.8 | 5.7 | 6.4 | 0.53 | 0.59 |
| `sieve` | 90.4 | 21.7 | 22.0 | 0.24 | 0.24 |
| `spectral` | 96.5 | 23.6 | 23.9 | 0.24 | 0.25 |
| `strbuild` | 3.7 | 3.8 | 3.9 | 1.01 | 1.06 |
| `strings` | 10.4 | 7.3 | 7.5 | 0.70 | 0.72 |
| `trees` | 180.4 | 76.0 | 74.7 | 0.42 | 0.41 |
| `words` | 16.4 | 10.7 | 11.0 | 0.66 | 0.67 |
| `hello` | 2.4 | 2.6 | 2.6 | 1.06 | 1.09 |
| `startup-0` | 2.4 | 2.4 | 2.7 | 1.02 | 1.12 |
| `startup-1` | 2.5 | 2.5 | 2.8 | 0.98 | 1.12 |
| `startup-10` | 2.6 | 2.6 | 2.8 | 0.98 | 1.05 |

## The programs in the other languages

Each program in [`bench/ref`](../bench/ref/) is held to one rule: the same program as its CigScript twin, statement for statement. The rule, how the set was checked against it, and what is still not the same are in the [paper of 2026-11-15](BENCHMARKS-2026-11-15.md#the-programs-in-the-other-languages); the references have not changed since. Where the usual way to write a step is native code, the idiomatic version is measured beside the translation:

| program | language | translation, ms | idiomatic, ms | idiomatic ÷ translation | stage b ÷ idiomatic |
|---|---|---:|---:|---:|---:|
| `fannkuch` | CPython | 32.8 | 32.6 | 0.99 | 0.88 |
| `sieve` | CPython | 24.8 | 20.7 | 0.84 | 1.00 |
| `words` | CPython | 14.5 | 14.7 | 1.01 | 0.67 |
| `lists` | Node | 60.9 | 58.2 | 0.95 | 0.25 |
| `maps` | Node | 66.2 | 78.7 | 1.19 | 0.16 |
| `sieve` | Node | 67.2 | 59.3 | 0.88 | 0.35 |
| `sieve` | Rust | 2.3 | 1.8 | 0.77 | 11.7 |

## Memory

The peak resident memory of each program, in MB, read as it exits (the median of every reading, three in each pass). It counts the pages of the program's own files with its allocations. A stage `c` program maps its native code from its own file, read and execute; none of its memory is ever both writable and executable.

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 8.6 | 13.7 | 13.7 | 2.4 | 2.5 | 2.6 | 9.9 | 55.8 | – |
| `csv` | 30.5 | 34.4 | 34.4 | – | – | – | 22.5 | – | – |
| `deltablue` | 8.4 | 13.7 | 13.8 | 2.4 | 2.5 | 2.6 | 9.9 | 49.7 | – |
| `dispatch` | 8.5 | 13.7 | 13.7 | 2.4 | 2.4 | 2.6 | 9.9 | 54.0 | – |
| `fannkuch` | 8.4 | 13.7 | 13.8 | 2.4 | 2.5 | 2.6 | 10.0 | 56.1 | – |
| `fib` | 8.5 | 13.7 | 13.7 | 2.4 | 2.5 | 2.6 | 9.8 | 53.2 | 2.2 |
| `json` | 66.2 | 72.4 | 72.4 | – | – | – | 27.1 | 66.9 | – |
| `json-deep` | 9.7 | 15.0 | 15.0 | – | – | – | 11.7 | 50.1 | – |
| `lists` | 21.9 | 24.4 | 26.6 | 4.2 | 3.4 | 3.6 | 13.8 | 56.0 | – |
| `loop` | 8.4 | 13.7 | 13.8 | 2.4 | 2.5 | 2.6 | 9.9 | 53.8 | 2.2 |
| `mandel` | 8.4 | 13.7 | 13.8 | 2.5 | 2.5 | 2.7 | 9.9 | 53.6 | 2.2 |
| `maps` | 26.7 | 28.5 | 29.5 | 10.4 | 7.9 | 8.1 | 16.1 | 63.5 | – |
| `nbody` | 8.5 | 13.8 | 16.4 | 2.8 | 2.7 | 2.8 | 10.5 | 54.4 | 2.2 |
| `regex-ish` | 29.1 | 36.3 | 35.0 | 15.5 | 15.8 | 16.0 | 23.6 | 87.9 | – |
| `richards` | 8.4 | 13.7 | 16.1 | 2.4 | 2.5 | 2.6 | 9.9 | 53.3 | – |
| `sieve` | 44.6 | 47.6 | 49.7 | 10.6 | 6.4 | 6.6 | 12.2 | 64.8 | 2.4 |
| `spectral` | 8.5 | 13.7 | 13.8 | 2.8 | 2.6 | 2.8 | 10.2 | 54.6 | – |
| `strbuild` | 8.4 | 13.8 | 13.8 | 2.8 | 2.6 | 2.7 | 9.9 | 50.1 | 2.2 |
| `strings` | 25.4 | 27.6 | 32.8 | 9.7 | 7.6 | 7.8 | 14.7 | 62.0 | – |
| `trees` | 10.5 | 15.8 | 15.8 | 6.1 | 7.2 | 7.8 | 11.3 | 61.7 | – |
| `words` | 26.1 | 27.6 | 28.5 | 4.8 | 3.8 | 4.0 | 14.4 | 60.5 | – |
| `hello` | 8.4 | 13.7 | 13.7 | 2.2 | 2.5 | 2.4 | 9.8 | 49.7 | 2.2 |
| `startup-0` | 8.4 | 13.7 | 13.7 | 2.2 | 2.5 | 2.5 | 9.8 | 49.7 | 2.2 |
| `startup-1` | 8.4 | 13.7 | 13.7 | 2.2 | 2.5 | 2.5 | 10.1 | 49.7 | – |
| `startup-10` | 8.5 | 13.7 | 13.7 | – | – | – | 15.3 | 51.2 | – |

## How steady the numbers are

The geometric means of the compute set, pass by pass:

| compute set | `cig run` ÷, pass 1 | `cig run` ÷, pass 2 | stage b ÷, pass 1 | stage b ÷, pass 2 | stage c ÷, pass 1 | stage c ÷, pass 2 |
|---|---:|---:|---:|---:|---:|---:|
| Lua | 1.35 | 1.37 | 1.30 | 1.31 | 0.94 | 0.95 |
| LuaJIT `-joff` | 1.57 | 1.57 | 1.51 | 1.51 | 1.09 | 1.09 |
| LuaJIT | 2.58 | 2.59 | 2.49 | 2.48 | 1.80 | 1.79 |
| CPython | 0.63 | 0.62 | 0.60 | 0.60 | 0.46 | 0.45 |
| Node | 0.22 | 0.22 | 0.21 | 0.21 | 0.16 | 0.16 |
| Rust | 6.91 | 6.95 | 6.68 | 6.66 | 3.15 | 3.13 |

Each side, pass against pass and within each pass ("last third ÷ first third" compares the median of a pair's last third of rounds with its first), and how many programs had a coefficient of variation over 5%:

| side | programs | pass 2 ÷ pass 1 | last third ÷ first third, pass 1 | last third ÷ first third, pass 2 | CV over 5%, pass 1 | CV over 5%, pass 2 |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 25 | 1.00 | 1.00 | 0.99 | 18 | 20 |
| stage a | 25 | 0.99 | 1.01 | 0.99 | 15 | 17 |
| stage b | 25 | 0.99 | 1.01 | 1.00 | 19 | 21 |
| stage c | 25 | 1.00 | 1.00 | 1.01 | 22 | 21 |
| `cig run --dry-run` | 24 | 1.00 | 1.02 | 1.02 | 19 | 20 |
| stage a, `--dry-run` | 24 | 1.02 | 0.99 | 1.03 | 14 | 16 |
| stage b, `--dry-run` | 24 | 1.00 | 0.99 | 1.01 | 18 | 21 |
| stage c, `--dry-run` | 24 | 1.01 | 0.99 | 0.99 | 19 | 23 |
| stage c, `CIG_NATIVE=0` | 25 | 1.01 | 1.01 | 0.98 | 19 | 21 |
| `cig run`, new folder | 1 | 0.97 | 0.89 | 1.20 | 1 | 1 |
| `cig run --engine tree` | 25 | 1.01 | 1.00 | 0.98 | 16 | 15 |
| `cig run --engine vm` | 25 | 1.00 | 1.01 | 1.02 | 21 | 20 |
| `cig run --engine vm`, empty tin | 25 | 0.99 | 1.02 | 1.02 | 22 | 23 |
| Lua | 21 | 0.97 | 0.99 | 1.00 | 18 | 20 |
| LuaJIT `-joff` | 21 | 0.99 | 1.06 | 0.97 | 20 | 19 |
| LuaJIT | 21 | 1.00 | 1.02 | 0.99 | 20 | 19 |
| CPython | 25 | 1.00 | 1.01 | 1.00 | 17 | 18 |
| Node | 24 | 1.00 | 1.00 | 1.01 | 8 | 6 |
| Rust | 8 | 0.96 | 1.04 | 0.98 | 8 | 8 |
| CPython, idiomatic | 3 | 1.02 | 1.01 | 1.00 | 3 | 3 |
| Node, idiomatic | 3 | 1.02 | 1.01 | 0.97 | 0 | 0 |
| Rust, idiomatic | 1 | 0.97 | 1.08 | 1.16 | 1 | 1 |

What moved during the run:

- **The machine is a laptop**, and its package was at up to 101 °C at a round's start in pass 1 and 100 °C in pass 2: nothing waits for it to cool, as in the runs before. Pass 2 against pass 1, the other languages came out at 0.97 (Lua) and 1.00 (CPython); Rust's eight programs take about two milliseconds each, and came out at 0.96.
- **cig's sides do not slow as its run history grows.** One cig home serves both passes, so the history grew from 152 runs to 5848, and pass 2 against pass 1 is 1.00 for `cig run`, 0.99 for stage `b` and 1.00 for stage `c` ([#252](https://github.com/otmof-ops/cig-script/issues/252)). Stage `c` against stage `b` is 0.76 in pass 1 and 0.76 in pass 2.

The times above pool both passes. A ratio within a few hundredths of 1 is level. The pairs whose medians in the two passes differ by more than 10%:

| program | side | pass 1, ms | pass 2, ms |
|---|---|---:|---:|
| `deltablue` | stage b, `--dry-run` | 2.0 | 2.3 |
| `deltablue` | Lua | 2.1 | 1.8 |
| `deltablue` | LuaJIT | 1.6 | 1.8 |
| `json-deep` | stage c | 2.8 | 3.2 |
| `json-deep` | stage b, `--dry-run` | 3.0 | 2.6 |
| `mandel` | LuaJIT | 3.2 | 2.8 |
| `richards` | LuaJIT `-joff` | 2.6 | 2.3 |
| `strbuild` | stage c | 3.5 | 3.1 |
| `hello` | `cig run --engine vm` | 2.7 | 2.4 |
| `hello` | LuaJIT | 1.5 | 1.9 |
| `startup-0` | stage b | 2.1 | 1.9 |
| `startup-0` | stage a, `--dry-run` | 1.7 | 2.0 |
| `startup-0` | `cig run --engine vm`, empty tin | 3.0 | 2.5 |
| `startup-0` | Lua | 1.6 | 1.4 |
| `startup-0` | LuaJIT | 1.8 | 1.6 |
| `startup-0` | Rust | 1.6 | 1.3 |
| `startup-1` | stage b | 2.3 | 1.9 |
| `startup-1` | LuaJIT `-joff` | 1.9 | 1.6 |
| `startup-10` | stage a | 2.4 | 2.1 |
| `startup-10` | stage b | 1.9 | 2.1 |
| `startup-10` | stage a, `--dry-run` | 1.7 | 1.9 |
| `startup-10` | `cig run --engine tree` | 2.5 | 3.0 |

## How it was measured

- **A whole process.** Each time is the wall time of one process, from just before it is started until it has exited and been reaped: the time a person waits. An interpreter's start-up is part of every number; the four start-up programs show it alone.
- **Every run checked.** A run counts only if it exits 0, prints exactly the program's `.expect` file, byte for byte, and writes nothing to stderr but cig's own summary of a run.
- **Rounds in a random order.** A pass is 21 rounds. In each round each of the 424 program-and-side pairs runs once, in an order drawn from a seeded generator, so the clock and the temperature, as they drift, fall on every program and every side alike.
- **Two passes**, 8904 samples in the first and 8904 in the second. A time above is the median of a pair's samples over both.
- **Pinned to one core.** The processor has cores of three kinds (six performance cores, eight efficiency cores, two low-power cores), and the same program takes a different time on each. Every measured process was pinned to one performance core (`--cpus 2`), set in the child before it starts; the builds were not.
- **A folder per program and side**, at the same path for every run. Before each run the files the last run left there are removed, and when that changes the disk, the disk is synced before the clock starts.
- **The run record is in the time.** `cig run` and a compiled program write a record of every run: that is part of what running a script with cig costs, so it is in the headline. The same programs as dry runs are in [cig's own sides](#cigs-own-sides).
- **`cig run` with its module kept.** Under the default engine a script's first two runs are the tree-walker's. Each program is run twice under `cig run` before the rounds, the lowering the second run leaves behind is waited for, and every timed run finds the module. After each `cig run` the harness waits until nothing of cig is left running; no wait is part of any time.
- **cig from source, built as a release is.** cig at commit `ca5236b` (1.25.0 and what has merged since; it still reports itself as `cig 1.25.0`), built with `cargo build --release --target x86_64-unknown-linux-musl`: a static binary with its own allocator, as the released Linux assets are. cigcompile at commit `6882f02`, built the same way, and the programs it builds from the same scripts on that cig, at `--stage a`, `b` and `c`. Every cig side runs with `CIG_SEAL_KEY=file`, `CIG_MAX_STEPS=0`, `CIG_REPORTS=never`, `CIG_DOCTOR=0` and `NO_COLOR=1`.
- **The others as this machine has them.** Lua 5.4.9 built from its source with the release makefile's flags (`-O2`) and LuaJIT 2.1 with its own makefile's defaults; CPython 3.12.3 and Node 18.19.1 as the machine's distribution ships them; Rust 1.99.1 with `-C opt-level=3`. No runtime was tuned or given flags beyond those.

## Conditions

|  | pass 1 | pass 2 |
|---|---|---|
| started (UTC) | 2026-11-25T20:02:28Z | 2026-11-25T20:06:51Z |
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
| load average at the start | 1.73 1.69 0.97 | 3.75 2.19 1.31 |
| package °C at the start | 63 | 73 |
| cig's run records at the start | 0 | 3176 |
| harness | Python 3.12.3 | Python 3.12.3 |
| rounds | 21 | 21 |
| seed of the rounds' order | 1 | 2 |
| pinned to CPUs | 2 | 2 |
| `true`, median ms | 1.258 | 0.936 |
| waits for a left-behind lowering that gave up | 0 | 0 |
| cig's run records at the last round | 2672 | 5848 |
| mean clock at a round's start, lowest, MHz | 1248 | 1000 |
| mean clock at a round's start, highest, MHz | 3061 | 3509 |
| package °C at a round's start, highest | 101 | 100 |
| cig | cig 1.25.0 | cig 1.25.0 |
| cigcompile | cigcompile 0.5.1 (makes programs for cig 1.25.0) | cigcompile 0.5.1 (makes programs for cig 1.25.0) |
| Lua | Lua 5.4.9  Copyright (C) 1994-2026 Lua.org, PUC-Rio | Lua 5.4.9  Copyright (C) 1994-2026 Lua.org, PUC-Rio |
| LuaJIT | LuaJIT 2.1.1794386141 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/ | LuaJIT 2.1.1794386141 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/ |
| CPython | Python 3.12.3 | Python 3.12.3 |
| Node | v18.19.1 | v18.19.1 |
| rustc | rustc 1.99.1 (58cf8f9c3 2026-10-13) | rustc 1.99.1 (58cf8f9c3 2026-10-13) |
| sha256 of `cig` | `76274240e5fe5c1b878a2e1df15285c9d3f414a570cc333b43757001c879a211` | `76274240e5fe5c1b878a2e1df15285c9d3f414a570cc333b43757001c879a211` |
| sha256 of `cigcompile` | `2eb5eca47c85ecd39717ad7eaa66a6a397c9604d27bba7994df872308b0a3115` | `2eb5eca47c85ecd39717ad7eaa66a6a397c9604d27bba7994df872308b0a3115` |
| sha256 of `lua` | `e00e7038402bdb1adaac94c6b88d4f3f201accc6f84d28e3f918dbc6d428e56e` | `e00e7038402bdb1adaac94c6b88d4f3f201accc6f84d28e3f918dbc6d428e56e` |
| sha256 of `luajit` | `e505ab009c3220cd4d7fd9936c0926d4a01ba885001e0ac4e0ec768473d87872` | `e505ab009c3220cd4d7fd9936c0926d4a01ba885001e0ac4e0ec768473d87872` |
| sha256 of `python3` | `e50d468e8b0adfb05733f5b87b3cff34829c4a8c1aea50c865aa8bdfe4bb150f` | `e50d468e8b0adfb05733f5b87b3cff34829c4a8c1aea50c865aa8bdfe4bb150f` |
| sha256 of `node` | `f3f93db342d5ac5bb61656d0599a603a73779e98befd9342171e550002725f4d` | `f3f93db342d5ac5bb61656d0599a603a73779e98befd9342171e550002725f4d` |
| sha256 of `rustc` | `b53b780f77f952062f8ca8a3b70aef2390b1e45ad91939be94afdc9543026853` | `b53b780f77f952062f8ca8a3b70aef2390b1e45ad91939be94afdc9543026853` |

## Run it again

```sh
# one pass of the sides this paper has (a side whose tool is not named is said and left out)
python3 bench/compare/compare.py --out pass1 --seed 1 --rounds 21 \
  --cig ./cig --cigcompile ./cigcompile --runtime ./cig \
  --lua ./lua --luajit ./luajit --python python3 --node node --rustc ./rustc --cpus 2 \
  --sides cig,cig-dry,cig-first,cig-tree,cig-vm,cig-vm-cold,stage-a,stage-a-dry,stage-b,stage-b-dry,stage-c,stage-c-dry,stage-c-off,lua,luajit-off,luajit,python,python-idiom,node,node-idiom,rust,rust-idiom
# this file's numbers, made and checked
python3 bench/compare/report.py --pass bench/results/2026-11-26/pass1 --pass bench/results/2026-11-26/pass2 \
  --numbers bench/results/2026-11-26/numbers.json \
  --earlier 2026-11-25 bench/results/2026-11-25/numbers.json \
  --earlier alone bench/results/2026-11-26/alone/numbers.json \
  --render bench/results/2026-11-26/paper.md.in docs/BENCHMARKS-2026-11-26.md
python3 bench/compare/check.py --pass bench/results/2026-11-26/pass1 --pass bench/results/2026-11-26/pass2 \
  --numbers bench/results/2026-11-26/numbers.json \
  --earlier 2026-11-25 bench/results/2026-11-25/numbers.json \
  --earlier alone bench/results/2026-11-26/alone/numbers.json \
  --render bench/results/2026-11-26/paper.md.in docs/BENCHMARKS-2026-11-26.md
```

## What this does not show

- **A release.** cig and cigcompile are built from `main`. The released cig 1.25.0 runs a script on the tree-walker unless it is asked for the VM, and has none of the rest; the next release that carries it is measured again, as released.
- **A script's first two runs.** `cig run` here is a script's third run and later. What its first run and its second cost under the default is [measured on its own](ENGINES-FIRST-RUNS-2026-11-25.md).
- **One machine.** A 2025 laptop processor, on mains power, every run on one performance core: the machine of the run of 2026-11-25, and not the one of 2026-11-15, which was a 2018 laptop on battery, unpinned, with a newer CPython and Node. Neither that run's times nor its ratios against other languages carry over.
- **Two runs set side by side are two runs.** The table in [Since the last paper](#since-the-last-paper) divides a median of this run by a median of another. The other languages' columns are there to show what that is worth: where they are not 1.00, nothing changed but the day.
- **Small programs, timed whole.** Most of these programs run for a few milliseconds to a few tens, so start-up is a large part of most of these times. A program whose loop takes a tenth of its stage `b` time is not ten times as fast as a process: `mandel`'s whole process takes 0.18 of its stage `b` time with its loops native.
- **Native code where it does not pay yet.** 14 of the 21 compute programs have native code: 9 are faster for it, 2 level and 3 slower ([above](#stage-c-against-stage-b)). The ones it does not speed up wait for what comes next: a call from native code that does not go through the VM, types for what a field and an element hold, strings.
- **Not every program in every language.** Rust has eight of the 25 programs; Lua has no `json`, `json-deep`, `csv` or `startup-10`; Node has no `csv`. The three counts beside each geometric mean add up to the programs it covers.
- **Memory is the peak resident set**, read when the process exits.
