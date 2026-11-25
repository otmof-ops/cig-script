# Benchmarks, 2026-11-24: stage c, native code for loops

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The 25 programs of [`bench/compute`](../bench/compute/), each written in CigScript and again in Python (all 25), JavaScript (24: all but `csv`), Lua (21) and Rust (8), timed side by side on one machine, with one side that is new: **a stage `c` program**, which [cigcompile](https://github.com/otmof-ops/cig-compile) builds with native code for the loops and functions it finds faster that way.

**This is a run of cig's source, not of a release.** Stage `c`'s native code for loops is on `main` and in no release yet, so cig is built here from commit `277e0e2` and cigcompile from commit `be2a18e`, cig as its release workflow builds it for Linux (`cargo build --release --target x86_64-unknown-linux-musl`). It is also another machine than the run of [2026-11-15](BENCHMARKS-2026-11-15.md), which measured cig 1.24.0 as released: a time in one paper is not to be set beside a time in the other. Within this one, every side ran in the same rounds.

> **A day later** the same machine ran the set again, with the VM's calls and start-up reworked and stage `c` reading lists and records: [the paper of 2026-11-25](BENCHMARKS-2026-11-25.md), which sets the two runs side by side.

Every number of the comparison is made from the samples in [`bench/results/2026-11-24/`](../bench/results/2026-11-24/) by [`bench/compare/report.py`](../bench/compare/report.py), and made again by [`bench/compare/check.py`](../bench/compare/check.py), a second program written from the [specification](../bench/compare/SPEC.md) alone; the two agree on every one.

## In short

- **Where a program has native code, it takes 0.52 of its stage `b` time** (1.92 times as fast), at the geometric mean of the 8 compute programs cigcompile gave any. The loops of arithmetic gain most: `mandel` takes 0.18 of its stage `b` time, `fib` 0.26, `fannkuch` 0.30, `loop` 0.50, `sieve` 0.78.
- **Over all 21 compute programs a stage `c` program takes 0.78 of stage `b`'s time**: faster on 7, level (within 3%) on 14, slower on 0. Most programs get no native code: their loops read a map, build a string or call a function every turn, and the compiler leaves a loop to the bytecode when native code would have to hand every other instruction back. Such a program is its stage `b` program.
- **Against the other languages, built at stage `c`:** 0.66 of CPython 3.12's time (faster on 15 of 21 programs; stage `b` takes 0.84), 1.37 times Lua 5.4's time (stage `b`: 1.83), 1.60 times LuaJIT's interpreter's, 2.54 times LuaJIT's with its compiler on, and 4.99 times Rust's over the 6 programs Rust has (stage `b`: 9.73). On the loops it compiles it is with Lua 5.4 or ahead of it: `mandel` takes 7.4 ms against Lua's 16.2, `fannkuch` 12.3 against 22.4, `fib` 4.4 against 4.4 and `loop` 4.6 against 4.3. Node 18 takes 49.0 ms to start on this machine, which decides its row (0.23).
- **A stage `b` program takes 0.55 of `cig run`'s time**, and a stage `c` program 0.43.
- **Start-up is unchanged:** `hello` takes 4.0 ms as a stage `c` program and 4.1 ms as a stage `b` one, where CPython takes 8.2 ms and Node 49.0 ms.

## Stage c against stage b

Each program's median time as a stage `b` program and as a stage `c` program, the second divided by the first, and what of the program cigcompile gave native code: its top level, so many of its functions, or nothing. The last two rows are the geometric mean of the ratios over every compute program and over the ones with native code, with how many were faster, level (within 3%) and slower.

| program | native | stage b, ms | stage c, ms | stage c ÷ stage b |
|---|---|---:|---:|---:|
| `closures` | none | 20.8 | 20.9 | 1.01 |
| `csv` | none | 22.4 | 22.2 | 0.99 |
| `deltablue` | the top level | 4.3 | 3.9 | 0.92 |
| `dispatch` | none | 18.9 | 18.9 | 1.00 |
| `fannkuch` | the top level | 40.7 | 12.3 | 0.30 |
| `fib` | 1 of 1 functions | 16.8 | 4.4 | 0.26 |
| `json` | none | 33.1 | 33.5 | 1.01 |
| `json-deep` | none | 4.9 | 5.0 | 1.03 |
| `lists` | none | 17.0 | 17.2 | 1.02 |
| `loop` | the top level | 9.3 | 4.6 | 0.50 |
| `mandel` | the top level | 40.2 | 7.4 | 0.18 |
| `maps` | none | 13.9 | 13.9 | 1.00 |
| `nbody` | none | 80.1 | 80.3 | 1.00 |
| `regex-ish` | none | 13.2 | 12.8 | 0.97 |
| `richards` | the top level | 7.8 | 8.1 | 1.03 |
| `sieve` | the top level | 32.8 | 25.5 | 0.78 |
| `spectral` | the top level, 0 of 3 functions | 39.3 | 40.2 | 1.02 |
| `strbuild` | none | 5.3 | 5.1 | 0.97 |
| `strings` | none | 8.5 | 8.4 | 0.99 |
| `trees` | none | 110.8 | 110.9 | 1.00 |
| `words` | none | 12.4 | 12.2 | 0.99 |
| **all compute programs** | – | 21 | – | 0.78 (7/14/0) |
| **programs with native code** | – | 8 | – | 0.52 (6/2/0) |

**What gets native code.** `cigcompile build --stage c` compiles a script's bytecode loop by loop to machine code that runs on the same registers cig's VM uses, so native code can sit beside anything: what it has no machine code for, the interpreter runs.

- **A loop of arithmetic** on ints, floats and bools, with a list's elements read and written by an int: `mandel`, `loop`, `fannkuch`, `sieve` and `deltablue`. `richards` and `spectral` have a loop compiled too and come out level: the loop is a small part of what they do.
- **A function on numbers alone**, compiled whole for the argument types it is called with: `fib`.
- **Not yet:** a loop that calls a function each turn (`closures`, `dispatch`, `spectral`, `trees`), reads a map's field (`nbody`, `maps`, `json`), or builds a string (`strings`, `strbuild`, `words`). Native code gains about two nanoseconds an instruction over cig's VM, and going between the two costs several times that, so the compiler weighs each loop and leaves it to the bytecode when it would not come out ahead. By its own figures a stage `c` program is never slower than its stage `b` program; here none was by more than the run's noise: the largest ratios are `json-deep`'s 1.03 and `richards`'s 1.03, and `json-deep` has no native code at all, so its two sides are one program's bytes run in the same rounds.

**What the native code itself adds:** the same stage `c` programs run with `CIG_NATIVE=0`, which makes them run their bytecode and never load their native code, take 1.28 times as long at the geometric mean (stage `c` ÷ the same program without its native code: 0.78).

## Against each language

A cell is cig's time divided by the other language's: the geometric mean, over the programs both have, of the ratio of their median times. Under 1, cig took less time. In brackets: how many of those programs cig was faster on, level with (within 3%) and slower on.

| time ÷ theirs | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 3.56 (3/0/15) | 4.17 (1/0/17) | 6.63 (1/0/17) | 1.53 (6/1/14) | 0.56 (14/0/6) | 25.4 (0/0/6) |
| stage a | 3.31 (3/1/14) | 3.87 (3/0/15) | 6.15 (2/0/16) | 1.41 (7/1/13) | 0.52 (14/0/6) | 23.6 (0/0/6) |
| stage b | 1.83 (3/0/15) | 2.14 (3/2/13) | 3.40 (3/0/15) | 0.84 (13/1/7) | 0.30 (18/0/2) | 9.73 (0/0/6) |
| stage c | 1.37 (5/1/12) | 1.60 (5/1/12) | 2.54 (3/0/15) | 0.66 (15/2/4) | 0.23 (18/0/2) | 4.99 (0/0/6) |

- **`cig run`** is cig's tree-walking interpreter, as `cig run script.cig` runs a script.
- **A stage `a` program** is what `cigcompile build` makes by default: cig itself with the script inside it, run by the same interpreter.
- **A stage `b` program** (`cigcompile build --stage b`) runs the script's bytecode on cig's VM.
- **A stage `c` program** (`cigcompile build --stage c`) is a stage `b` program with native code beside its bytecode.

These are the 21 programs that are not start-up tests; start-up has [its own section](#start-up).

## Every program

The median time of each program on each side, in milliseconds, over 21 rounds in each of 2 passes:

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 25.2 | 20.8 | 20.9 | 7.4 | 6.1 | 6.2 | 17.3 | 54.4 | – |
| `csv` | 26.3 | 22.4 | 22.2 | – | – | – | 38.2 | – | – |
| `deltablue` | 7.8 | 4.3 | 3.9 | 2.2 | 1.9 | 1.9 | 8.9 | 49.2 | – |
| `dispatch` | 33.2 | 18.9 | 18.9 | 5.8 | 5.8 | 3.9 | 19.9 | 57.5 | – |
| `fannkuch` | 159.4 | 40.7 | 12.3 | 22.4 | 17.9 | 9.5 | 33.2 | 55.5 | – |
| `fib` | 32.1 | 16.8 | 4.4 | 4.4 | 4.2 | 2.3 | 12.6 | 50.4 | 2.0 |
| `json` | 35.0 | 33.1 | 33.5 | – | – | – | 33.1 | 73.6 | – |
| `json-deep` | 6.8 | 4.9 | 5.0 | – | – | – | 13.4 | 50.0 | – |
| `lists` | 18.1 | 17.0 | 17.2 | 15.8 | 16.6 | 14.0 | 15.0 | 60.9 | – |
| `loop` | 30.9 | 9.3 | 4.6 | 4.3 | 3.3 | 2.4 | 21.7 | 53.9 | 1.9 |
| `mandel` | 183.6 | 40.2 | 7.4 | 16.2 | 11.9 | 3.1 | 53.0 | 52.8 | 2.8 |
| `maps` | 19.5 | 13.9 | 13.9 | 21.1 | 13.7 | 12.8 | 19.0 | 66.3 | – |
| `nbody` | 236.4 | 80.1 | 80.3 | 28.9 | 18.8 | 3.7 | 83.1 | 55.7 | 2.4 |
| `regex-ish` | 17.6 | 13.2 | 12.8 | 31.7 | 29.0 | 25.3 | 18.7 | 103.5 | – |
| `richards` | 14.1 | 7.8 | 8.1 | 2.7 | 2.4 | 2.0 | 10.5 | 51.5 | – |
| `sieve` | 93.3 | 32.8 | 25.5 | 11.1 | 9.2 | 4.7 | 25.2 | 66.9 | 2.5 |
| `spectral` | 100.4 | 39.3 | 40.2 | 11.6 | 8.8 | 2.4 | 35.4 | 56.8 | – |
| `strbuild` | 7.0 | 5.3 | 5.1 | 2.8 | 6.5 | 6.1 | 8.2 | 49.9 | 1.6 |
| `strings` | 12.1 | 8.5 | 8.4 | 13.9 | 10.9 | 8.8 | 12.7 | 59.6 | – |
| `trees` | 183.3 | 110.8 | 110.9 | 55.0 | 28.9 | 20.0 | 55.2 | 61.6 | – |
| `words` | 18.8 | 12.4 | 12.2 | 8.7 | 7.6 | 6.0 | 14.6 | 62.2 | – |
| `hello` | 5.5 | 4.1 | 4.0 | 1.6 | 1.8 | 1.8 | 8.2 | 49.0 | 1.9 |
| `startup-0` | 5.6 | 4.1 | 3.9 | 1.7 | 1.7 | 1.9 | 7.9 | 48.6 | 1.6 |
| `startup-1` | 5.7 | 3.9 | 4.0 | 1.7 | 2.0 | 1.8 | 7.8 | 49.1 | – |
| `startup-10` | 5.6 | 4.1 | 4.1 | – | – | – | 13.7 | 54.0 | – |

From the 10th to the 90th percentile of the same samples:

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 23.1–27.1 | 19.1–22.9 | 19.2–23.5 | 6.8–8.1 | 5.8–7.0 | 5.7–7.1 | 16.7–19.0 | 53.5–58.9 | – |
| `csv` | 23.4–30.6 | 20.8–28.4 | 21.2–25.2 | – | – | – | 37.6–40.1 | – | – |
| `deltablue` | 5.3–10.3 | 2.9–6.3 | 2.8–5.5 | 1.6–3.2 | 1.7–2.7 | 1.6–2.6 | 8.2–9.9 | 48.2–50.4 | – |
| `dispatch` | 30.9–35.8 | 17.6–21.7 | 17.0–21.1 | 5.5–6.9 | 5.3–7.0 | 3.7–5.4 | 19.5–27.0 | 56.9–58.6 | – |
| `fannkuch` | 157.6–164.9 | 39.3–48.3 | 11.0–14.2 | 22.1–25.4 | 16.5–25.3 | 9.1–11.2 | 32.6–40.4 | 54.5–61.0 | – |
| `fib` | 29.6–34.8 | 15.2–19.0 | 3.5–6.1 | 4.0–5.3 | 3.8–4.9 | 1.9–3.1 | 12.1–13.5 | 49.3–55.8 | 1.5–2.6 |
| `json` | 33.4–38.7 | 32.2–34.9 | 32.3–36.1 | – | – | – | 32.3–40.0 | 72.7–76.6 | – |
| `json-deep` | 4.2–9.0 | 3.3–7.2 | 3.2–6.9 | – | – | – | 12.6–18.3 | 49.2–53.1 | – |
| `lists` | 15.8–23.3 | 15.7–19.4 | 15.5–19.7 | 15.4–18.4 | 16.2–18.4 | 13.5–21.1 | 14.3–18.1 | 59.7–62.5 | – |
| `loop` | 29.1–34.1 | 8.0–12.7 | 3.2–6.8 | 4.1–5.1 | 3.0–4.8 | 2.0–3.3 | 21.0–25.2 | 53.2–60.2 | 1.6–2.8 |
| `mandel` | 181.0–186.7 | 38.3–43.4 | 6.0–9.8 | 15.5–18.2 | 11.7–12.7 | 2.6–4.1 | 52.2–57.2 | 51.8–59.4 | 2.5–3.9 |
| `maps` | 17.8–21.5 | 13.0–16.1 | 12.8–15.6 | 20.4–22.2 | 13.3–14.6 | 12.4–13.6 | 18.5–20.3 | 65.4–72.0 | – |
| `nbody` | 233.6–244.8 | 78.5–82.7 | 78.3–81.8 | 28.0–30.7 | 18.4–20.3 | 3.2–4.4 | 81.5–87.5 | 53.6–61.3 | 1.9–3.0 |
| `regex-ish` | 15.5–20.0 | 12.1–16.6 | 12.0–13.9 | 31.1–33.6 | 28.3–33.0 | 24.5–28.7 | 18.3–24.6 | 101.9–109.6 | – |
| `richards` | 11.4–16.9 | 6.0–10.8 | 6.3–9.7 | 2.3–3.9 | 2.1–3.2 | 1.6–2.6 | 10.1–11.4 | 50.4–56.9 | – |
| `sieve` | 89.7–98.0 | 31.6–35.2 | 24.1–29.4 | 10.7–13.2 | 8.9–10.1 | 4.2–7.9 | 24.7–31.1 | 66.0–70.6 | 2.2–3.5 |
| `spectral` | 97.7–105.2 | 37.9–41.6 | 39.1–41.5 | 11.0–14.0 | 8.6–10.3 | 2.1–3.3 | 34.9–36.7 | 55.8–60.9 | – |
| `strbuild` | 4.3–8.7 | 3.6–6.7 | 3.4–6.5 | 2.5–3.8 | 5.9–7.2 | 5.7–6.9 | 7.5–10.9 | 49.1–52.0 | 1.3–2.4 |
| `strings` | 10.2–14.8 | 7.7–10.8 | 7.5–11.1 | 13.2–14.6 | 10.3–17.4 | 8.4–9.8 | 12.2–14.0 | 58.1–65.4 | – |
| `trees` | 180.7–189.7 | 109.2–114.9 | 109.0–117.4 | 54.0–56.3 | 28.4–36.2 | 19.5–21.1 | 54.6–56.4 | 60.7–66.0 | – |
| `words` | 16.5–21.0 | 11.5–14.5 | 11.6–14.1 | 8.2–9.6 | 7.2–8.7 | 5.7–6.9 | 14.0–15.9 | 61.4–68.5 | – |
| `hello` | 3.1–7.3 | 2.3–5.0 | 2.5–5.5 | 1.4–2.3 | 1.4–2.5 | 1.5–2.5 | 7.5–12.7 | 47.9–54.8 | 1.4–2.4 |
| `startup-0` | 3.0–8.6 | 2.4–5.4 | 2.4–5.2 | 1.4–2.4 | 1.4–2.6 | 1.5–2.5 | 7.5–12.9 | 47.9–54.3 | 1.4–2.2 |
| `startup-1` | 3.5–7.8 | 2.7–5.4 | 2.4–5.6 | 1.4–2.4 | 1.4–2.7 | 1.5–2.3 | 7.5–9.1 | 48.2–56.0 | – |
| `startup-10` | 3.3–7.3 | 2.4–5.7 | 2.5–5.3 | – | – | – | 13.1–15.5 | 53.0–57.0 | – |

## Start-up

| time ÷ theirs | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 3.32 (0/0/3) | 3.07 (0/0/3) | 3.05 (0/0/3) | 0.61 (4/0/0) | 0.11 (4/0/0) | 3.17 (0/0/2) |
| stage a | 2.26 (0/0/3) | 2.08 (0/0/3) | 2.07 (0/0/3) | 0.42 (4/0/0) | 0.08 (4/0/0) | 2.14 (0/0/2) |
| stage b | 2.41 (0/0/3) | 2.22 (0/0/3) | 2.21 (0/0/3) | 0.44 (4/0/0) | 0.08 (4/0/0) | 2.36 (0/0/2) |
| stage c | 2.37 (0/0/3) | 2.18 (0/0/3) | 2.17 (0/0/3) | 0.44 (4/0/0) | 0.08 (4/0/0) | 2.27 (0/0/2) |

The four start-up programs, each side's time divided by the other's as above. Their median times are in [Every program](#every-program). `cig run` of `hello` in a new folder each time takes 5.2 ms.

## cig's own sides

Every program on each of cig's sides, a run and a dry run (`--dry-run` writes no run record):

| program | `cig run` | `cig run --dry-run` | stage a | stage a, `--dry-run` | stage b | stage b, `--dry-run` | stage c | stage c, `--dry-run` |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 25.2 | 23.9 | 24.3 | 24.0 | 20.8 | 20.5 | 20.9 | 20.6 |
| `csv` | 26.3 | – | 24.0 | – | 22.4 | – | 22.2 | – |
| `deltablue` | 7.8 | 6.0 | 6.0 | 5.4 | 4.3 | 4.0 | 3.9 | 3.9 |
| `dispatch` | 33.2 | 31.7 | 33.2 | 32.9 | 18.9 | 19.0 | 18.9 | 19.3 |
| `fannkuch` | 159.4 | 158.4 | 158.0 | 158.0 | 40.7 | 40.0 | 12.3 | 11.8 |
| `fib` | 32.1 | 30.6 | 30.3 | 29.9 | 16.8 | 15.9 | 4.4 | 4.6 |
| `json` | 35.0 | 33.5 | 34.0 | 33.2 | 33.1 | 32.7 | 33.5 | 32.5 |
| `json-deep` | 6.8 | 5.1 | 4.9 | 4.9 | 4.9 | 4.5 | 5.0 | 4.5 |
| `lists` | 18.1 | 16.2 | 16.0 | 15.6 | 17.0 | 17.0 | 17.2 | 17.0 |
| `loop` | 30.9 | 29.0 | 29.7 | 29.6 | 9.3 | 9.4 | 4.6 | 4.4 |
| `mandel` | 183.6 | 182.1 | 182.4 | 181.4 | 40.2 | 40.3 | 7.4 | 7.3 |
| `maps` | 19.5 | 18.0 | 18.1 | 18.1 | 13.9 | 13.7 | 13.9 | 13.6 |
| `nbody` | 236.4 | 234.5 | 236.4 | 237.9 | 80.1 | 80.2 | 80.3 | 81.0 |
| `regex-ish` | 17.6 | 15.8 | 16.3 | 15.7 | 13.2 | 12.5 | 12.8 | 12.5 |
| `richards` | 14.1 | 12.4 | 12.8 | 12.5 | 7.8 | 7.7 | 8.1 | 7.4 |
| `sieve` | 93.3 | 91.1 | 91.8 | 91.2 | 32.8 | 32.5 | 25.5 | 25.4 |
| `spectral` | 100.4 | 98.4 | 98.9 | 98.3 | 39.3 | 39.4 | 40.2 | 40.7 |
| `strbuild` | 7.0 | 5.2 | 4.9 | 4.9 | 5.3 | 4.7 | 5.1 | 4.8 |
| `strings` | 12.1 | 10.8 | 11.2 | 10.9 | 8.5 | 8.1 | 8.4 | 8.4 |
| `trees` | 183.3 | 182.1 | 183.2 | 183.4 | 110.8 | 111.8 | 110.9 | 110.7 |
| `words` | 18.8 | 17.4 | 17.1 | 16.9 | 12.4 | 12.1 | 12.2 | 12.3 |
| `hello` | 5.5 | 4.1 | 3.6 | 3.5 | 4.1 | 3.8 | 4.0 | 3.5 |
| `startup-0` | 5.6 | 3.9 | 3.9 | 3.3 | 4.1 | 3.5 | 3.9 | 3.8 |
| `startup-1` | 5.7 | 3.9 | 3.9 | 3.5 | 3.9 | 3.7 | 4.0 | 3.8 |
| `startup-10` | 5.6 | 4.0 | 3.9 | 3.5 | 4.1 | 3.5 | 4.1 | 4.0 |

`cig run` on each engine: the tree-walker, the VM with the script's module already in the tin, and the VM lowering the script first:

| program | `cig run --engine tree` | `cig run --engine vm` | `cig run --engine vm`, empty tin | vm ÷ tree | vm, empty tin ÷ tree |
|---|---:|---:|---:|---:|---:|
| `closures` | 23.3 | 19.3 | 18.8 | 0.83 | 0.81 |
| `csv` | 23.8 | 22.2 | 22.4 | 0.93 | 0.94 |
| `deltablue` | 5.3 | 3.5 | 3.8 | 0.66 | 0.71 |
| `dispatch` | 30.9 | 17.4 | 17.9 | 0.56 | 0.58 |
| `fannkuch` | 157.1 | 39.7 | 40.5 | 0.25 | 0.26 |
| `fib` | 29.9 | 15.7 | 15.9 | 0.52 | 0.53 |
| `json` | 33.1 | 32.8 | 33.2 | 0.99 | 1.00 |
| `json-deep` | 4.1 | 4.1 | 4.4 | 0.99 | 1.05 |
| `lists` | 15.9 | 16.5 | 16.7 | 1.04 | 1.05 |
| `loop` | 28.6 | 8.4 | 8.7 | 0.29 | 0.30 |
| `mandel` | 181.1 | 39.1 | 39.6 | 0.22 | 0.22 |
| `maps` | 17.7 | 13.9 | 14.6 | 0.79 | 0.82 |
| `nbody` | 234.5 | 75.9 | 76.2 | 0.32 | 0.32 |
| `regex-ish` | 15.7 | 11.8 | 12.4 | 0.75 | 0.79 |
| `richards` | 11.4 | 6.8 | 7.2 | 0.59 | 0.63 |
| `sieve` | 90.3 | 32.3 | 32.9 | 0.36 | 0.36 |
| `spectral` | 98.0 | 36.9 | 36.5 | 0.38 | 0.37 |
| `strbuild` | 4.3 | 4.3 | 4.6 | 1.01 | 1.07 |
| `strings` | 10.3 | 7.5 | 8.4 | 0.73 | 0.81 |
| `trees` | 180.3 | 109.3 | 110.4 | 0.61 | 0.61 |
| `words` | 16.7 | 12.1 | 12.6 | 0.72 | 0.76 |
| `hello` | 3.1 | 3.1 | 3.4 | 1.02 | 1.10 |
| `startup-0` | 3.2 | 3.0 | 3.0 | 0.95 | 0.95 |
| `startup-1` | 3.1 | 3.2 | 3.4 | 1.04 | 1.10 |
| `startup-10` | 3.1 | 3.1 | 3.5 | 1.01 | 1.11 |

## The programs in the other languages

Each program in [`bench/ref`](../bench/ref/) is held to one rule: the same program as its CigScript twin, statement for statement. The rule, how the set was checked against it, and what is still not the same are in the [paper of 2026-11-15](BENCHMARKS-2026-11-15.md#the-programs-in-the-other-languages); the references have not changed since. Where the usual way to write a step is native code, the idiomatic version is measured beside the translation:

| program | language | translation, ms | idiomatic, ms | idiomatic ÷ translation | stage b ÷ idiomatic |
|---|---|---:|---:|---:|---:|
| `fannkuch` | CPython | 33.2 | 33.1 | 1.00 | 1.23 |
| `sieve` | CPython | 25.2 | 20.9 | 0.83 | 1.56 |
| `words` | CPython | 14.6 | 14.6 | 1.00 | 0.85 |
| `lists` | Node | 60.9 | 58.0 | 0.95 | 0.29 |
| `maps` | Node | 66.3 | 78.8 | 1.19 | 0.18 |
| `sieve` | Node | 66.9 | 59.2 | 0.89 | 0.55 |
| `sieve` | Rust | 2.5 | 2.1 | 0.84 | 15.8 |

## Memory

The peak resident memory of each program, in MB, read as it exits (the median of every reading, three in each pass). It counts the pages of the program's own files with its allocations. A stage `c` program maps its native code from its own file, read and execute; none of its memory is ever both writable and executable.

| program | `cig run` | stage b | stage c | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 13.4 | 19.4 | 19.3 | 2.4 | 2.5 | 2.7 | 10.1 | 56.0 | – |
| `csv` | 28.6 | 35.0 | 35.0 | – | – | – | 22.6 | – | – |
| `deltablue` | 13.4 | 19.2 | 18.7 | 2.4 | 2.5 | 2.7 | 10.1 | 49.8 | – |
| `dispatch` | 13.4 | 19.2 | 19.2 | 2.4 | 2.5 | 2.6 | 10.1 | 54.1 | – |
| `fannkuch` | 13.4 | 19.2 | 19.8 | 2.4 | 2.4 | 2.7 | 10.2 | 56.2 | – |
| `fib` | 13.4 | 19.4 | 20.0 | 2.4 | 2.5 | 2.6 | 10.0 | 53.3 | 2.2 |
| `json` | 66.1 | 72.9 | 72.9 | – | – | – | 27.3 | 66.9 | – |
| `json-deep` | 14.3 | 19.6 | 19.5 | – | – | – | 11.9 | 50.2 | – |
| `lists` | 25.3 | 30.3 | 28.7 | 4.2 | 3.4 | 3.5 | 14.1 | 56.2 | – |
| `loop` | 13.4 | 19.4 | 20.0 | 2.4 | 2.5 | 2.6 | 10.0 | 54.0 | 2.2 |
| `mandel` | 13.4 | 19.3 | 20.0 | 2.5 | 2.5 | 2.7 | 10.1 | 53.7 | 2.2 |
| `maps` | 24.7 | 30.1 | 31.1 | 10.4 | 7.9 | 8.1 | 16.4 | 63.6 | – |
| `nbody` | 14.6 | 19.2 | 19.3 | 2.8 | 2.7 | 2.9 | 10.6 | 54.6 | 2.2 |
| `regex-ish` | 30.1 | 36.7 | 36.7 | 15.5 | 15.8 | 16.0 | 23.9 | 87.9 | – |
| `richards` | 13.4 | 19.3 | 19.9 | 2.4 | 2.5 | 2.6 | 10.1 | 53.5 | – |
| `sieve` | 47.3 | 53.0 | 54.4 | 10.6 | 6.4 | 6.7 | 12.3 | 65.0 | 2.4 |
| `spectral` | 13.5 | 19.3 | 19.9 | 2.8 | 2.7 | 2.8 | 10.4 | 54.8 | – |
| `strbuild` | 14.4 | 19.3 | 19.2 | 2.8 | 2.6 | 2.8 | 10.0 | 50.2 | 2.2 |
| `strings` | 23.4 | 28.3 | 28.3 | 9.7 | 7.6 | 7.8 | 14.8 | 62.1 | – |
| `trees` | 16.3 | 19.4 | 19.3 | 6.1 | 7.2 | 7.9 | 11.5 | 61.8 | – |
| `words` | 22.7 | 27.9 | 27.9 | 4.8 | 3.8 | 3.9 | 14.6 | 60.6 | – |
| `hello` | 13.4 | 19.3 | 19.3 | 2.2 | 2.5 | 2.5 | 10.0 | 49.8 | 2.2 |
| `startup-0` | 14.4 | 19.3 | 19.3 | 2.2 | 2.5 | 2.4 | 10.0 | 49.8 | 2.2 |
| `startup-1` | 13.5 | 19.4 | 19.2 | 2.2 | 2.5 | 2.5 | 10.3 | 49.8 | – |
| `startup-10` | 13.6 | 19.3 | 19.3 | – | – | – | 15.5 | 51.4 | – |

## How steady the numbers are

The geometric means of the compute set, pass by pass:

| compute set | `cig run` ÷, pass 1 | `cig run` ÷, pass 2 | stage b ÷, pass 1 | stage b ÷, pass 2 | stage c ÷, pass 1 | stage c ÷, pass 2 |
|---|---:|---:|---:|---:|---:|---:|
| Lua | 3.33 | 3.72 | 1.70 | 1.93 | 1.25 | 1.46 |
| LuaJIT `-joff` | 3.88 | 4.36 | 1.98 | 2.26 | 1.46 | 1.71 |
| LuaJIT | 6.27 | 6.82 | 3.20 | 3.53 | 2.35 | 2.67 |
| CPython | 1.43 | 1.60 | 0.78 | 0.89 | 0.60 | 0.70 |
| Node | 0.52 | 0.59 | 0.28 | 0.32 | 0.21 | 0.25 |
| Rust | 23.4 | 25.5 | 8.87 | 9.75 | 4.30 | 5.23 |

Each side, pass against pass and within each pass ("last third ÷ first third" compares the median of a pair's last third of rounds with its first), and how many programs had a coefficient of variation over 5%:

| side | programs | pass 2 ÷ pass 1 | last third ÷ first third, pass 1 | last third ÷ first third, pass 2 | CV over 5%, pass 1 | CV over 5%, pass 2 |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 25 | 1.19 | 1.14 | 1.10 | 16 | 18 |
| stage a | 25 | 1.15 | 1.09 | 1.06 | 18 | 19 |
| stage b | 25 | 1.20 | 1.13 | 1.08 | 20 | 21 |
| stage c | 25 | 1.22 | 1.15 | 1.11 | 23 | 22 |
| `cig run --dry-run` | 24 | 1.14 | 1.10 | 1.07 | 17 | 15 |
| stage a, `--dry-run` | 24 | 1.18 | 1.08 | 1.06 | 15 | 18 |
| stage b, `--dry-run` | 24 | 1.22 | 1.11 | 1.08 | 18 | 19 |
| stage c, `--dry-run` | 24 | 1.25 | 1.13 | 1.12 | 21 | 19 |
| stage c, `CIG_NATIVE=0` | 25 | 1.20 | 1.14 | 1.08 | 18 | 20 |
| `cig run`, new folder | 1 | 1.74 | 1.63 | 1.33 | 1 | 1 |
| `cig run --engine tree` | 25 | 1.06 | 1.02 | 1.04 | 16 | 19 |
| `cig run --engine vm` | 25 | 1.06 | 1.04 | 1.06 | 16 | 19 |
| `cig run --engine vm`, empty tin | 25 | 1.06 | 1.06 | 1.06 | 19 | 19 |
| Lua | 21 | 1.00 | 1.01 | 1.04 | 16 | 18 |
| LuaJIT `-joff` | 21 | 0.99 | 1.06 | 1.00 | 21 | 21 |
| LuaJIT | 21 | 1.03 | 1.01 | 1.03 | 20 | 21 |
| CPython | 25 | 1.00 | 0.99 | 1.02 | 19 | 22 |
| Node | 24 | 1.00 | 1.00 | 1.00 | 7 | 6 |
| Rust | 8 | 1.02 | 1.03 | 1.05 | 8 | 8 |
| CPython, idiomatic | 3 | 1.02 | 1.01 | 1.01 | 2 | 2 |
| Node, idiomatic | 3 | 1.01 | 1.00 | 1.00 | 0 | 0 |
| Rust, idiomatic | 1 | 1.05 | 1.06 | 1.11 | 1 | 1 |

Two things moved during the run:

- **The machine is a laptop**, and its package reached 97 °C in pass 1 and 100 °C in pass 2. The other languages still repeated: pass 2 against pass 1, Lua 1.00, CPython 1.00, Rust 1.02.
- **cig's sides slowed as its run history grew** ([#252](https://github.com/otmof-ops/cig-script/issues/252)): every cig run reads the whole run index first, and the history went from 126 runs to 5796 by the end. Pass 2 against pass 1: `cig run` 1.19, stage `b` 1.20, stage `c` 1.22. It falls on every cig side alike, so stage `c` against stage `b` is the same in both passes (0.77 and 0.79); cig against the other languages is the worse for it, by what a history of thousands of runs costs.

The times above pool both passes. A ratio within a few hundredths of 1 is level. The pairs whose medians in the two passes differ by more than 10%:

| program | side | pass 1, ms | pass 2, ms |
|---|---|---:|---:|
| `closures` | `cig run` | 23.7 | 26.4 |
| `closures` | stage b | 19.8 | 21.8 |
| `closures` | stage c | 19.9 | 22.4 |
| `closures` | stage c, `--dry-run` | 19.5 | 21.5 |
| `closures` | stage c, `CIG_NATIVE=0` | 20.0 | 22.4 |
| `deltablue` | `cig run` | 6.2 | 8.3 |
| `deltablue` | stage a | 5.2 | 6.7 |
| `deltablue` | stage b | 3.2 | 5.3 |
| `deltablue` | stage c | 3.2 | 4.7 |
| `deltablue` | `cig run --dry-run` | 5.3 | 6.6 |
| `deltablue` | stage a, `--dry-run` | 4.8 | 6.9 |
| `deltablue` | stage b, `--dry-run` | 3.2 | 5.2 |
| `deltablue` | stage c, `--dry-run` | 3.3 | 4.6 |
| `deltablue` | stage c, `CIG_NATIVE=0` | 3.4 | 5.0 |
| `deltablue` | `cig run --engine tree` | 4.8 | 5.6 |
| `deltablue` | `cig run --engine vm` | 3.3 | 3.7 |
| `deltablue` | `cig run --engine vm`, empty tin | 3.6 | 4.0 |
| `dispatch` | stage c | 17.8 | 19.8 |
| `dispatch` | stage b, `--dry-run` | 18.1 | 20.0 |
| `dispatch` | stage c, `--dry-run` | 18.2 | 20.2 |
| `dispatch` | stage c, `CIG_NATIVE=0` | 18.0 | 20.2 |
| `fannkuch` | stage c | 11.4 | 12.7 |
| `fannkuch` | stage c, `--dry-run` | 11.0 | 12.3 |
| `fib` | `cig run` | 30.5 | 33.8 |
| `fib` | stage b | 15.7 | 17.4 |
| `fib` | stage c | 3.8 | 5.2 |
| `fib` | stage b, `--dry-run` | 15.2 | 16.8 |
| `fib` | stage c, `--dry-run` | 3.5 | 4.9 |
| `fib` | stage c, `CIG_NATIVE=0` | 15.6 | 17.5 |
| `fib` | LuaJIT `-joff` | 4.5 | 4.0 |
| `fib` | Rust | 1.9 | 2.3 |
| `json-deep` | `cig run` | 5.3 | 7.5 |
| `json-deep` | stage a | 3.9 | 5.8 |
| `json-deep` | stage b | 3.6 | 5.6 |
| `json-deep` | stage c | 3.8 | 5.8 |
| `json-deep` | `cig run --dry-run` | 4.3 | 6.1 |
| `json-deep` | stage a, `--dry-run` | 3.9 | 6.2 |
| `json-deep` | stage b, `--dry-run` | 3.7 | 5.6 |
| `json-deep` | stage c, `--dry-run` | 3.5 | 5.5 |
| `json-deep` | stage c, `CIG_NATIVE=0` | 4.0 | 5.4 |
| `json-deep` | `cig run --engine tree` | 3.9 | 4.4 |
| `json-deep` | `cig run --engine vm` | 3.6 | 4.3 |
| `json-deep` | `cig run --engine vm`, empty tin | 4.1 | 4.8 |
| `lists` | `cig run` | 17.1 | 19.3 |
| `lists` | stage b | 16.2 | 17.9 |
| `lists` | stage c | 16.2 | 18.1 |
| `lists` | stage a, `--dry-run` | 14.7 | 16.6 |
| `lists` | stage b, `--dry-run` | 15.9 | 17.8 |
| `lists` | stage c, `--dry-run` | 15.9 | 17.9 |
| `lists` | stage c, `CIG_NATIVE=0` | 16.3 | 18.1 |
| `loop` | stage b | 8.6 | 10.1 |
| `loop` | stage c | 3.5 | 5.5 |
| `loop` | stage b, `--dry-run` | 8.3 | 10.1 |
| `loop` | stage c, `--dry-run` | 3.3 | 4.9 |
| `loop` | stage c, `CIG_NATIVE=0` | 8.5 | 10.0 |
| `loop` | LuaJIT | 2.2 | 2.5 |
| `loop` | Rust | 1.9 | 2.2 |
| `mandel` | stage c | 6.7 | 7.8 |
| `mandel` | stage c, `--dry-run` | 6.2 | 7.7 |
| `mandel` | LuaJIT | 3.0 | 3.4 |
| `maps` | `cig run` | 18.2 | 20.8 |
| `maps` | stage a | 17.4 | 19.5 |
| `maps` | stage b, `--dry-run` | 13.2 | 14.6 |
| `nbody` | Rust | 2.7 | 2.3 |
| `regex-ish` | `cig run` | 16.4 | 18.7 |
| `regex-ish` | stage c, `--dry-run` | 11.9 | 13.1 |
| `regex-ish` | stage c, `CIG_NATIVE=0` | 12.2 | 13.5 |
| `richards` | `cig run` | 12.7 | 15.3 |
| `richards` | stage a | 11.6 | 13.2 |
| `richards` | stage b | 6.4 | 9.0 |
| `richards` | stage c | 6.7 | 8.9 |
| `richards` | stage a, `--dry-run` | 11.4 | 13.5 |
| `richards` | stage b, `--dry-run` | 6.9 | 8.7 |
| `richards` | stage c, `--dry-run` | 6.5 | 8.1 |
| `richards` | stage c, `CIG_NATIVE=0` | 6.9 | 8.5 |
| `richards` | LuaJIT | 1.8 | 2.2 |
| `strbuild` | `cig run` | 5.4 | 7.7 |
| `strbuild` | stage a | 4.2 | 5.6 |
| `strbuild` | stage b | 4.2 | 5.6 |
| `strbuild` | stage c | 4.1 | 5.6 |
| `strbuild` | `cig run --dry-run` | 4.2 | 6.0 |
| `strbuild` | stage a, `--dry-run` | 4.0 | 6.0 |
| `strbuild` | stage b, `--dry-run` | 3.8 | 5.6 |
| `strbuild` | stage c, `--dry-run` | 4.0 | 5.4 |
| `strbuild` | stage c, `CIG_NATIVE=0` | 3.9 | 5.5 |
| `strbuild` | `cig run --engine vm` | 4.0 | 4.5 |
| `strings` | `cig run` | 11.0 | 13.6 |
| `strings` | stage b | 7.8 | 9.0 |
| `strings` | stage c | 7.9 | 9.1 |
| `strings` | `cig run --dry-run` | 10.1 | 11.2 |
| `strings` | stage b, `--dry-run` | 7.6 | 9.1 |
| `strings` | stage c, `--dry-run` | 7.6 | 9.2 |
| `strings` | stage c, `CIG_NATIVE=0` | 7.9 | 8.9 |
| `words` | `cig run` | 17.6 | 19.6 |
| `words` | stage b | 11.9 | 13.4 |
| `words` | `cig run --dry-run` | 16.5 | 18.1 |
| `words` | stage b, `--dry-run` | 11.7 | 13.0 |
| `words` | stage c, `--dry-run` | 11.7 | 12.9 |
| `hello` | `cig run` | 3.9 | 6.6 |
| `hello` | stage a | 3.0 | 4.7 |
| `hello` | stage b | 3.1 | 4.6 |
| `hello` | stage c | 3.2 | 4.8 |
| `hello` | `cig run --dry-run` | 3.1 | 4.8 |
| `hello` | stage a, `--dry-run` | 2.7 | 4.2 |
| `hello` | stage b, `--dry-run` | 2.5 | 4.7 |
| `hello` | stage c, `--dry-run` | 2.6 | 4.5 |
| `hello` | stage c, `CIG_NATIVE=0` | 3.0 | 4.8 |
| `hello` | `cig run`, new folder | 3.7 | 6.4 |
| `hello` | `cig run --engine tree` | 2.7 | 3.2 |
| `hello` | `cig run --engine vm`, empty tin | 3.0 | 3.7 |
| `hello` | LuaJIT | 1.8 | 2.1 |
| `startup-0` | `cig run` | 3.8 | 6.3 |
| `startup-0` | stage a | 3.0 | 4.5 |
| `startup-0` | stage b | 2.8 | 4.6 |
| `startup-0` | stage c | 2.8 | 4.5 |
| `startup-0` | `cig run --dry-run` | 3.2 | 4.2 |
| `startup-0` | stage a, `--dry-run` | 2.8 | 4.4 |
| `startup-0` | stage b, `--dry-run` | 2.7 | 4.5 |
| `startup-0` | stage c, `--dry-run` | 2.8 | 4.4 |
| `startup-0` | stage c, `CIG_NATIVE=0` | 2.9 | 4.7 |
| `startup-0` | `cig run --engine tree` | 2.8 | 3.4 |
| `startup-0` | `cig run --engine vm` | 2.8 | 3.1 |
| `startup-1` | `cig run` | 4.3 | 6.6 |
| `startup-1` | stage a | 3.0 | 4.7 |
| `startup-1` | stage b | 3.1 | 4.8 |
| `startup-1` | stage c | 2.9 | 4.6 |
| `startup-1` | `cig run --dry-run` | 3.1 | 4.6 |
| `startup-1` | stage a, `--dry-run` | 2.8 | 4.4 |
| `startup-1` | stage b, `--dry-run` | 2.8 | 4.4 |
| `startup-1` | stage c, `--dry-run` | 2.8 | 4.8 |
| `startup-1` | stage c, `CIG_NATIVE=0` | 2.8 | 4.8 |
| `startup-1` | `cig run --engine tree` | 2.8 | 3.3 |
| `startup-1` | `cig run --engine vm` | 2.7 | 3.2 |
| `startup-1` | `cig run --engine vm`, empty tin | 2.9 | 3.9 |
| `startup-1` | LuaJIT `-joff` | 2.1 | 1.8 |
| `startup-10` | `cig run` | 4.1 | 6.4 |
| `startup-10` | stage a | 3.2 | 4.4 |
| `startup-10` | stage b | 3.3 | 4.8 |
| `startup-10` | stage c | 3.2 | 4.9 |
| `startup-10` | `cig run --dry-run` | 3.2 | 4.7 |
| `startup-10` | stage a, `--dry-run` | 2.8 | 4.9 |
| `startup-10` | stage b, `--dry-run` | 2.9 | 4.3 |
| `startup-10` | stage c, `--dry-run` | 3.0 | 4.8 |
| `startup-10` | stage c, `CIG_NATIVE=0` | 2.8 | 4.5 |
| `startup-10` | `cig run --engine tree` | 2.6 | 3.5 |
| `startup-10` | `cig run --engine vm` | 2.9 | 3.4 |
| `startup-10` | `cig run --engine vm`, empty tin | 3.2 | 3.7 |

## How it was measured

- **A whole process.** Each time is the wall time of one process, from just before it is started until it has exited and been reaped: the time a person waits. An interpreter's start-up is part of every number; the four start-up programs show it alone.
- **Every run checked.** A run counts only if it exits 0, prints exactly the program's `.expect` file, byte for byte, and writes nothing to stderr but cig's own summary of a run.
- **Rounds in a random order.** A pass is 21 rounds. In each round each of the 424 program-and-side pairs runs once, in an order drawn from a seeded generator, so the clock and the temperature, as they drift, fall on every program and every side alike.
- **Two passes**, 8904 samples in the first and 8904 in the second. A time above is the median of a pair's samples over both.
- **Pinned to one core.** The processor has cores of three kinds (six performance cores, eight efficiency cores, two low-power cores), and the same program takes a different time on each. Every measured process was pinned to one performance core (`--cpus 2`), set in the child before it starts; the builds were not.
- **A folder per program and side**, at the same path for every run. Before each run the files the last run left there are removed, and when that changes the disk, the disk is synced before the clock starts.
- **The run record is in the time.** `cig run` and a compiled program write a record of every run: that is part of what running a script with cig costs, so it is in the headline. The same programs as dry runs are in [cig's own sides](#cigs-own-sides).
- **cig from source, built as a release is.** cig at commit `277e0e2` (1.25.0 and what has merged since, the VM's side of stage `c` among it), built with `cargo build --release --target x86_64-unknown-linux-musl`: a static binary with its own allocator, as the released Linux assets are. cigcompile at commit `be2a18e`, and the programs it builds from the same scripts on that cig, at `--stage a`, `b` and `c`. Every cig side runs with `CIG_SEAL_KEY=file`, `CIG_MAX_STEPS=0`, `CIG_REPORTS=never`, `CIG_DOCTOR=0` and `NO_COLOR=1`.
- **The others as this machine has them.** Lua 5.4.9 built from its source with the release makefile's flags (`-O2`) and LuaJIT 2.1 with its own makefile's defaults; CPython 3.12.3 and Node 18.19.1 as the machine's distribution ships them; Rust 1.99.1 with `-C opt-level=3`. No runtime was tuned or given flags beyond those.

## Conditions

|  | pass 1 | pass 2 |
|---|---|---|
| started (UTC) | 2026-11-24T13:30:10Z | 2026-11-24T13:35:44Z |
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
| load average at the start | 1.16 1.66 1.91 | 1.76 1.95 1.97 |
| package °C at the start | 72 | 72 |
| cig's run records at the start | 0 | 3150 |
| harness | Python 3.12.3 | Python 3.12.3 |
| rounds | 21 | 21 |
| seed of the rounds' order | 1 | 2 |
| pinned to CPUs | 2 | 2 |
| `true`, median ms | 1.008 | 1.014 |
| cig's run records at the last round | 2646 | 5796 |
| mean clock at a round's start, lowest, MHz | 1081 | 892 |
| mean clock at a round's start, highest, MHz | 2634 | 2895 |
| package °C at a round's start, highest | 97 | 100 |
| cig | cig 1.25.0 | cig 1.25.0 |
| cigcompile | cigcompile 0.5.1 (makes programs for cig 1.25.0) | cigcompile 0.5.1 (makes programs for cig 1.25.0) |
| Lua | Lua 5.4.9  Copyright (C) 1994-2026 Lua.org, PUC-Rio | Lua 5.4.9  Copyright (C) 1994-2026 Lua.org, PUC-Rio |
| LuaJIT | LuaJIT 2.1.1794386141 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/ | LuaJIT 2.1.1794386141 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/ |
| CPython | Python 3.12.3 | Python 3.12.3 |
| Node | v18.19.1 | v18.19.1 |
| rustc | rustc 1.99.1 (58cf8f9c3 2026-10-13) | rustc 1.99.1 (58cf8f9c3 2026-10-13) |
| sha256 of `cig` | `1ef685ba42a285dab15059eb0223ea5004917671e5b62a784deb49f8a9440f8d` | `1ef685ba42a285dab15059eb0223ea5004917671e5b62a784deb49f8a9440f8d` |
| sha256 of `cigcompile` | `e527431233da36cc9aad6a0e0bbd7bcb1b5f55708819808cd3952b332fe6aff5` | `e527431233da36cc9aad6a0e0bbd7bcb1b5f55708819808cd3952b332fe6aff5` |
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
python3 bench/compare/report.py --pass bench/results/2026-11-24/pass1 --pass bench/results/2026-11-24/pass2 \
  --numbers bench/results/2026-11-24/numbers.json \
  --render bench/results/2026-11-24/paper.md.in docs/BENCHMARKS-2026-11-24.md
python3 bench/compare/check.py --pass bench/results/2026-11-24/pass1 --pass bench/results/2026-11-24/pass2 \
  --numbers bench/results/2026-11-24/numbers.json \
  --render bench/results/2026-11-24/paper.md.in docs/BENCHMARKS-2026-11-24.md
```

## What this does not show

- **A release.** cig and cigcompile are built from `main`. The released cig 1.25.0 has no native code for loops; the next release that carries it is measured again, as released.
- **One machine, and not the last paper's.** A 2025 laptop processor, on mains power, every run on one performance core. The run of 2026-11-15 was on a 2018 laptop on battery, unpinned, with a newer CPython and Node: neither its times nor its ratios against other languages carry over.
- **Small programs, timed whole.** Most of these programs run for a few milliseconds to a few tens, so start-up is a large part of most of these times. A program whose loop takes a tenth of its stage `b` time is not ten times as fast as a process: `mandel`'s whole process takes 0.18 of its stage `b` time with its loops native.
- **Most programs are unchanged by stage `c` today.** 8 of the 21 compute programs have native code. The rest wait for what comes next: calls from native code to native code, a map's fields, strings.
- **Not every program in every language.** Rust has eight of the 25 programs; Lua has no `json`, `json-deep`, `csv` or `startup-10`; Node has no `csv`. The three counts beside each geometric mean add up to the programs it covers.
- **Memory is the peak resident set**, read when the process exits.
