# Benchmarks, 2026-11-15: cig against Lua, LuaJIT, CPython, Node and Rust

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The 25 programs of [`bench/compute`](../bench/compute/), each written in CigScript and again in Python (all 25), JavaScript (24: all but `csv`), Lua (21) and Rust (8), timed side by side on one machine. cig is measured as it is released: cig 1.24.0, and the programs cigcompile 0.5.0 builds from the same scripts. The others: Lua 5.4.9, LuaJIT 2.1, CPython 3.13.5, Node 22.23.5 and Rust 1.100.0.

Every number of the comparison is made from the samples in [`bench/results/2026-11-15/`](../bench/results/2026-11-15/) by [`bench/compare/report.py`](../bench/compare/report.py), and made again by [`bench/compare/check.py`](../bench/compare/check.py), a second program written apart from the first to [the same definitions](../bench/compare/SPEC.md); the two agree on every number and every character of this file, and none was typed by hand. The one other table, cig-script's own suite, is `cig bench`'s own output, which `check.py` confirms this file quotes exactly.

## In short

- **Built at stage `b`, cig runs the 21 compute programs in 0.82 of CPython 3.13's time**, 1.22 times as fast: a program `cigcompile build --stage b` makes runs its script's bytecode on cig's VM, and was faster than CPython on 12 of the 21 programs, level on 2 and slower on 7. Against Node 22 it takes 0.84 of the time; Node takes 27.2 ms just to start, and most of these programs finish soon after.
- **A stage `b` program takes 0.52 of `cig run`'s time** (1.91 times as fast), faster on all 21. A program `cigcompile build` makes by default, stage `a`, runs on the interpreter: it takes 1.44 times CPython's time. `cig run`, the tree-walking interpreter a script runs on today, takes 1.56 times CPython's time and 1.65 times Node's.
- **Lua is faster.** A stage `b` program takes 1.97 times Lua 5.4's time, 2.77 times LuaJIT's interpreter's and 5.20 times LuaJIT's with its compiler on; and 26.2 times Rust's, on the 6 compute programs Rust has. The VM work to close the gap to Lua is [#243](https://github.com/otmof-ops/cig-script/issues/243).
- **cig starts faster than CPython and Node**: `hello` takes 5.8 ms with `cig run` and 3.7 ms as a compiled program, where CPython takes 13.5 ms and Node 27.2 ms. Lua, LuaJIT and Rust start in about a millisecond.
- **This run found that cig slows down as its run history grows** ([#252](https://github.com/otmof-ops/cig-script/issues/252)): every run reads the whole run index first. `hello` with `cig run` took 4.3 ms over the first pass, while cig's history grew from 76 to 2356 runs, and 7.8 ms over the second (2736 to 5016). Every cig time here comes from that growing history, so cig's times, its start-up most of all, are slower than a new install's would be.

## Against each language

A cell is cig's time divided by the other language's: the geometric mean, over the programs both have, of the ratio of their median times. Under 1, cig took less time. In brackets: how many of those programs cig was faster on, level with (within 3%) and slower on; the three add up to the programs the cell covers.

| time ÷ theirs | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 4.03 (1/2/15) | 5.66 (1/0/17) | 10.6 (1/0/17) | 1.56 (4/0/17) | 1.65 (9/0/11) | 67.5 (0/0/6) |
| stage a | 3.77 (3/1/14) | 5.30 (2/1/15) | 9.95 (2/0/16) | 1.44 (4/1/16) | 1.53 (9/0/11) | 63.4 (0/0/6) |
| stage b | 1.97 (4/0/14) | 2.77 (3/0/15) | 5.20 (2/0/16) | 0.82 (12/2/7) | 0.84 (10/1/9) | 26.2 (0/0/6) |

- **`cig run`** is cig's tree-walking interpreter, as `cig run script.cig` runs a script.
- **A stage `a` program** is what `cigcompile build` makes by default: cig itself with the script inside it, run by the same interpreter.
- **A stage `b` program** (`cigcompile build --stage b`) runs the script's bytecode on cig's VM.

These are the 21 programs that are not start-up tests; start-up has [its own section](#start-up).

## Every program

The median time of each program on each side, in milliseconds, over 31 rounds in each of 2 passes:

| program | `cig run` | stage b | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 53.1 | 40.4 | 12.8 | 10.0 | 10.4 | 32.3 | 32.8 | – |
| `csv` | 63.3 | 55.6 | – | – | – | 83.2 | – | – |
| `deltablue` | 11.3 | 4.6 | 1.3 | 1.3 | 1.2 | 14.8 | 28.8 | – |
| `dispatch` | 78.4 | 35.7 | 12.9 | 8.2 | 4.5 | 41.9 | 35.9 | – |
| `fannkuch` | 327.8 | 76.3 | 46.1 | 30.9 | 17.8 | 70.0 | 35.7 | – |
| `fib` | 62.6 | 34.4 | 6.2 | 4.5 | 1.9 | 24.6 | 29.7 | 1.1 |
| `json` | 74.0 | 66.6 | – | – | – | 64.1 | 55.4 | – |
| `json-deep` | 8.4 | 5.4 | – | – | – | 21.3 | 28.1 | – |
| `lists` | 32.9 | 27.8 | 29.7 | 30.2 | 24.4 | 27.7 | 51.0 | – |
| `loop` | 64.8 | 17.6 | 9.4 | 4.5 | 1.5 | 47.6 | 32.7 | 1.3 |
| `mandel` | 374.5 | 71.8 | 33.2 | 17.8 | 3.5 | 93.3 | 33.4 | 3.1 |
| `maps` | 41.8 | 29.4 | 43.0 | 21.5 | 19.6 | 35.3 | 50.3 | – |
| `nbody` | 474.2 | 202.3 | 77.2 | 34.7 | 5.2 | 172.6 | 37.5 | 1.8 |
| `regex-ish` | 42.4 | 32.6 | 66.8 | 54.3 | 48.7 | 33.7 | 77.8 | – |
| `richards` | 25.2 | 12.1 | 3.2 | 2.4 | 1.4 | 19.0 | 31.4 | – |
| `sieve` | 164.6 | 69.7 | 19.1 | 12.2 | 4.8 | 55.8 | 46.8 | 1.9 |
| `spectral` | 216.6 | 76.6 | 21.7 | 14.2 | 2.1 | 75.9 | 34.4 | – |
| `strbuild` | 10.0 | 6.6 | 3.3 | 9.3 | 8.8 | 13.9 | 28.5 | 0.9 |
| `strings` | 25.2 | 17.4 | 25.1 | 16.9 | 12.3 | 22.9 | 42.0 | – |
| `trees` | 397.5 | 260.0 | 131.8 | 57.7 | 42.1 | 115.2 | 46.5 | – |
| `words` | 39.2 | 25.4 | 15.4 | 12.1 | 9.7 | 27.3 | 47.9 | – |
| `hello` | 5.8 | 3.7 | 0.9 | 1.0 | 1.0 | 13.5 | 27.2 | 0.9 |
| `startup-0` | 6.0 | 3.7 | 0.9 | 1.0 | 1.0 | 13.1 | 27.1 | 0.9 |
| `startup-1` | 6.1 | 3.6 | 0.9 | 1.0 | 0.9 | 13.1 | 27.0 | – |
| `startup-10` | 6.1 | 3.7 | – | – | – | 26.2 | 29.6 | – |

From the 10th to the 90th percentile of the same samples:

| program | `cig run` | stage b | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 48.2–59.0 | 37.8–44.4 | 11.8–13.4 | 9.7–11.0 | 9.7–11.1 | 31.5–35.6 | 30.2–38.0 | – |
| `csv` | 56.8–90.7 | 51.2–81.6 | – | – | – | 75.6–105.7 | – | – |
| `deltablue` | 8.5–14.4 | 3.2–6.2 | 1.2–1.4 | 1.1–1.3 | 1.0–1.2 | 14.0–15.9 | 25.4–33.5 | – |
| `dispatch` | 72.8–90.7 | 32.4–39.5 | 12.0–14.3 | 7.8–8.8 | 4.4–4.9 | 39.8–45.4 | 32.5–39.0 | – |
| `fannkuch` | 300.1–344.7 | 71.2–82.2 | 42.1–48.7 | 28.4–33.4 | 16.5–19.2 | 64.8–74.5 | 33.2–42.4 | – |
| `fib` | 56.7–67.8 | 31.7–38.4 | 5.9–6.7 | 4.2–5.1 | 1.7–2.0 | 23.0–26.0 | 26.8–35.1 | 1.0–1.2 |
| `json` | 68.9–80.7 | 64.1–73.0 | – | – | – | 62.9–70.7 | 51.2–63.9 | – |
| `json-deep` | 5.5–11.1 | 4.2–7.2 | – | – | – | 20.9–23.7 | 26.2–33.4 | – |
| `lists` | 28.2–36.6 | 25.4–30.6 | 27.6–31.3 | 28.4–33.0 | 22.9–26.1 | 27.1–30.4 | 46.1–56.0 | – |
| `loop` | 59.4–72.2 | 15.3–19.8 | 9.1–10.3 | 4.3–4.8 | 1.4–1.7 | 44.3–50.2 | 30.2–37.4 | 1.2–1.3 |
| `mandel` | 343.9–396.5 | 66.1–77.3 | 32.6–37.0 | 16.4–20.9 | 3.3–3.8 | 89.4–102.1 | 30.2–38.6 | 3.0–3.4 |
| `maps` | 37.4–46.5 | 27.4–32.8 | 40.4–45.2 | 20.5–24.5 | 18.1–21.2 | 32.7–37.5 | 47.7–54.6 | – |
| `nbody` | 449.5–536.3 | 182.0–209.4 | 71.8–83.2 | 31.9–38.7 | 5.0–5.6 | 158.2–186.1 | 34.4–42.6 | 1.7–2.0 |
| `regex-ish` | 38.3–47.3 | 30.8–35.8 | 62.5–70.1 | 52.6–59.3 | 46.5–52.8 | 32.2–36.2 | 72.7–84.9 | – |
| `richards` | 21.2–28.7 | 9.9–14.1 | 3.0–3.5 | 2.2–2.5 | 1.2–1.5 | 18.2–21.2 | 28.4–35.6 | – |
| `sieve` | 159.2–182.9 | 62.9–74.7 | 17.6–20.0 | 11.7–13.4 | 4.5–5.2 | 50.1–62.0 | 43.7–50.8 | 1.7–2.0 |
| `spectral` | 200.2–230.4 | 73.3–84.9 | 21.2–23.9 | 13.0–18.9 | 2.0–2.3 | 71.4–81.3 | 30.9–39.8 | – |
| `strbuild` | 6.9–13.0 | 5.0–8.3 | 3.2–3.6 | 8.9–10.2 | 8.4–9.5 | 13.2–15.1 | 25.0–32.2 | 0.7–0.9 |
| `strings` | 21.8–28.6 | 15.9–19.2 | 24.2–27.5 | 16.3–18.6 | 11.9–13.4 | 22.0–25.0 | 39.5–48.7 | – |
| `trees` | 359.6–416.5 | 235.7–271.8 | 117.5–135.2 | 54.9–62.6 | 39.4–46.0 | 106.9–122.7 | 43.7–50.4 | – |
| `words` | 35.0–43.6 | 23.5–27.9 | 14.6–16.6 | 11.9–13.5 | 9.0–10.4 | 26.0–29.3 | 45.2–54.0 | – |
| `hello` | 3.4–9.2 | 2.4–5.3 | 0.7–0.9 | 0.8–1.0 | 0.8–1.0 | 12.5–14.2 | 24.7–30.4 | 0.7–0.9 |
| `startup-0` | 3.3–8.9 | 2.3–5.5 | 0.7–1.0 | 0.8–1.0 | 0.8–1.0 | 12.5–14.1 | 25.1–31.5 | 0.8–0.9 |
| `startup-1` | 3.2–8.6 | 2.3–5.3 | 0.8–1.0 | 0.9–1.0 | 0.8–1.0 | 12.5–14.2 | 24.5–30.7 | – |
| `startup-10` | 3.7–8.8 | 2.3–5.6 | – | – | – | 25.2–28.2 | 26.4–35.9 | – |

## Start-up

| time ÷ theirs | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 6.67 (0/0/3) | 6.17 (0/0/3) | 6.27 (0/0/3) | 0.38 (4/0/0) | 0.22 (4/0/0) | 6.84 (0/0/2) |
| stage a | 4.18 (0/0/3) | 3.87 (0/0/3) | 3.93 (0/0/3) | 0.24 (4/0/0) | 0.14 (4/0/0) | 4.29 (0/0/2) |
| stage b | 4.08 (0/0/3) | 3.78 (0/0/3) | 3.84 (0/0/3) | 0.23 (4/0/0) | 0.13 (4/0/0) | 4.26 (0/0/2) |

The four start-up programs, each side's time divided by the other's as above. Their median times are in [Every program](#every-program): `hello` takes 5.8 ms with `cig run`, 3.7 ms as a compiled program, 13.5 ms with CPython, 27.2 ms with Node, 0.9 ms with Lua, 1.0 ms with LuaJIT and 0.9 ms as a Rust program. The harness itself, starting and reaping `true`, takes 0.63 ms in pass 1 and 0.81 in pass 2.

`cig run` of `hello` in a new folder each time takes 7.4 ms.

## The run record

`cig run` writes a record of every run, what `cig runs` lists; a run that burns also keeps the journal `cig unburn` reads, and makes both durable. Since cig 1.24.0 a run that burns nothing, as 24 of these 25 programs do, writes its record without syncing it. A dry run (`--dry-run`) runs the same program and keeps no record, and `cig run` (not a dry run, not a compiled program) also reads the whole run index a second time before it starts, for the script's last good version, which grows with the history ([#252](https://github.com/otmof-ops/cig-script/issues/252)). What a run costs over its dry run, the record and that read together, over the programs that have both:

|  | programs | run ÷ dry run | run − dry run, median ms |
|---|---:|---:|---:|
| `cig run` | 24 | 1.17 | 2.8 |
| stage a | 24 | 1.05 | 0.5 |
| stage b | 24 | 1.04 | 0.4 |

Every program, cig's sides only:

| program | `cig run` | `cig run --dry-run` | stage a | stage a, `--dry-run` | stage b | stage b, `--dry-run` |
|---|---:|---:|---:|---:|---:|---:|
| `closures` | 53.1 | 49.6 | 52.2 | 50.0 | 40.4 | 40.2 |
| `csv` | 63.3 | – | 61.4 | – | 55.6 | – |
| `deltablue` | 11.3 | 9.0 | 9.2 | 8.6 | 4.6 | 4.3 |
| `dispatch` | 78.4 | 75.6 | 76.2 | 76.8 | 35.7 | 35.9 |
| `fannkuch` | 327.8 | 331.3 | 322.0 | 316.9 | 76.3 | 75.3 |
| `fib` | 62.6 | 58.2 | 59.9 | 56.4 | 34.4 | 32.8 |
| `json` | 74.0 | 69.9 | 69.8 | 69.6 | 66.6 | 65.8 |
| `json-deep` | 8.4 | 6.1 | 6.1 | 5.9 | 5.4 | 5.1 |
| `lists` | 32.9 | 29.9 | 29.6 | 29.4 | 27.8 | 26.7 |
| `loop` | 64.8 | 61.6 | 62.0 | 60.8 | 17.6 | 17.4 |
| `mandel` | 374.5 | 354.9 | 357.3 | 362.2 | 71.8 | 70.7 |
| `maps` | 41.8 | 38.0 | 38.3 | 37.9 | 29.4 | 29.4 |
| `nbody` | 474.2 | 458.9 | 474.9 | 491.3 | 202.3 | 193.2 |
| `regex-ish` | 42.4 | 41.4 | 39.1 | 38.5 | 32.6 | 31.8 |
| `richards` | 25.2 | 22.4 | 23.2 | 22.7 | 12.1 | 11.5 |
| `sieve` | 164.6 | 171.8 | 163.9 | 161.1 | 69.7 | 67.1 |
| `spectral` | 216.6 | 210.3 | 212.8 | 200.2 | 76.6 | 78.3 |
| `strbuild` | 10.0 | 7.7 | 7.8 | 7.3 | 6.6 | 6.1 |
| `strings` | 25.2 | 22.4 | 22.9 | 22.5 | 17.4 | 16.9 |
| `trees` | 397.5 | 382.2 | 376.9 | 369.4 | 260.0 | 244.9 |
| `words` | 39.2 | 36.0 | 38.2 | 35.4 | 25.4 | 25.1 |
| `hello` | 5.8 | 3.5 | 3.7 | 3.3 | 3.7 | 3.2 |
| `startup-0` | 6.0 | 3.6 | 3.7 | 3.2 | 3.7 | 3.3 |
| `startup-1` | 6.1 | 3.6 | 3.8 | 3.3 | 3.6 | 3.3 |
| `startup-10` | 6.1 | 3.8 | 3.8 | 3.2 | 3.7 | 3.3 |

And against the other languages with cig's sides as dry runs, the computation without the record:

| time ÷ theirs | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|
| `cig run --dry-run` | 3.74 (3/1/14) | 5.25 (2/1/15) | 9.86 (2/0/16) | 1.48 (3/1/16) | 1.52 (9/0/11) | 62.8 (0/0/6) |
| stage a, `--dry-run` | 3.67 (3/1/14) | 5.16 (2/1/15) | 9.69 (2/0/16) | 1.46 (3/1/16) | 1.49 (9/0/11) | 62.1 (0/0/6) |
| stage b, `--dry-run` | 1.92 (4/0/14) | 2.69 (3/1/14) | 5.05 (2/0/16) | 0.80 (12/1/7) | 0.82 (10/1/9) | 25.2 (0/0/6) |

## The programs in the other languages

A comparison is only as fair as its programs. Each program in [`bench/ref`](../bench/ref/) is held to one rule: **the same program as its CigScript twin, statement for statement.**

- The same algorithm, sizes, iteration counts and output.
- Where the CigScript program does a step in a loop of its own, the reference loops too; where it calls a builtin (`join`, `split`, `replace`, `find`, `sort`, `slice`, `json.parse`, `csv.read`), the reference calls its language's standard library for that step.
- The same data structures, as the language spells them: a list is a list (an array, a `Vec`); a map with named fields is a dict in Python, an object in JavaScript, a table in Lua and a struct in Rust; a map with computed keys is a dict, a `Map` or a table.
- Written as that language's users write it: Python's programs keep their code in a `main()` (CPython runs a module's top level more slowly; the four start-up programs, a line or a few each, excepted), Lua counts from 1, and Rust's fixed sizes go through `black_box` so the compiler cannot fold the work away.

**How the set was checked.** Three reviewers first read the references then in the set against their CigScript programs, each told to report what favors the other language as carefully as what favors cig. Of the 72 they read, 52 were the same work; 11 favored cig (among them Lua doing in script what cig does in a builtin, chiefly `json`, `json-deep` and `csv`, and Python and Node importing modules `startup-10` never used) and 9 favored the other language (among them `fannkuch` and `sieve` in Python doing their loops as slice and list operations in C, and `nbody` in Node and Rust keeping each body in an array of numbers where cig has a map with named fields). The one left off their lists, `nbody` in Python, kept each body in a list the same way. Each was rewritten to the rule above, kept where the difference is the language's own (Python's `json` and `json-deep` import `json`), or dropped where the language has no counterpart (Lua has no JSON, CSV or hashing), as stated below. The rewritten Python, Rust and JavaScript set, nine new references among them, was read by two more reviewers: 61 of 61 held to the rule. The rewritten Lua set, three idiomatic versions added afterwards and four files changed after that reading were read by a third: 26 of 28 the same work, and of the two that favored cig, slightly, `lists` in Lua now sorts its list where it is, as a Lua programmer would. Every reference prints its program's `.expect` before it gets a time, as every cig run does.

**Where the usual way to write a step is native code**, the idiomatic version is measured beside the translation, from [`bench/ref/idiom`](../bench/ref/idiom/): `fannkuch`, `sieve` and `words` in Python (slice assignment; a list repeat and `sum`; `collections.Counter`), `sieve`, `lists` and `maps` in JavaScript (`Array.fill`; an `Int32Array` sort; a plain object for the map) and `sieve` in Rust (`vec![true; n]` and an iterator count).

| program | language | translation, ms | idiomatic, ms | idiomatic ÷ translation | stage b ÷ idiomatic |
|---|---|---:|---:|---:|---:|
| `fannkuch` | CPython | 70.0 | 61.8 | 0.88 | 1.23 |
| `sieve` | CPython | 55.8 | 42.9 | 0.77 | 1.62 |
| `words` | CPython | 27.3 | 28.1 | 1.03 | 0.91 |
| `lists` | Node | 51.0 | 39.4 | 0.77 | 0.71 |
| `maps` | Node | 50.3 | 72.8 | 1.45 | 0.40 |
| `sieve` | Node | 46.8 | 41.2 | 0.88 | 1.69 |
| `sieve` | Rust | 1.9 | 1.4 | 0.74 | 51.1 |

**What is still not the same, and stays so:**

- **`strbuild`**: `s += "x"` (in Lua, `s = s .. "x"`) is the same statement everywhere, and each language's own speed at it: CPython appends in place when `s` is a local, V8 builds a rope, Rust's `String` grows in place, Lua and cig copy.
- **`csv`**: cig writes the file inside a `burn`, which journals it so the run can be undone; Python writes it plainly (the same bytes). Node and Lua have no CSV in their standard libraries and are not timed on it; Lua's has no JSON or hashing either, so `json`, `json-deep` and `startup-10` have no Lua version.
- **`json` and `json-deep` in Python** import the `json` module, which cig does not need: part of what Python costs to do this.
- **`lists` in Lua**: Lua has no `filter` or `map`, so the reference loops where cig calls builtins.
- **`fannkuch` in Lua** copies its 8-item list with a loop where cig calls `slice`: the native alternative, `{unpack(t)}`, has a different name on Lua 5.4 and on LuaJIT, and the copy is a small part of the program next to its flips.
- **Rust** has eight of the 25 programs, the eight cig-compile's benchmark set carries; the rest are not written yet. Its column is a measure of how far the others are from native code, over those eight.
- **Rounding**: JavaScript's `Math.round` takes no number of places, so `nbody` and `spectral` round with `Math.round(x * 1e9) / 1e9`, and Rust prints with `{:.9}`; every output is still byte for byte the `.expect` file.

## Memory

The peak resident memory of each program, in MB: the most the process held in memory at once, read as it exits (the median of every reading, three in each pass). It counts the pages of the program's own files with its allocations, so a big executable weighs in: cig's, which a compiled program carries too, and Node's. cig's peak also grew between the passes, as its run history did: it reads the whole run index into memory at start-up ([#252](https://github.com/otmof-ops/cig-script/issues/252)). The machine runs transparent huge pages `always`, which can round a heap up in 2 MiB steps.

| program | `cig run` | stage b | Lua | LuaJIT `-joff` | LuaJIT | CPython | Node | Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `closures` | 15.9 | 14.2 | 2.3 | 2.4 | 2.6 | 9.9 | 70.1 | – |
| `csv` | 35.9 | 31.1 | – | – | – | 23.7 | – | – |
| `deltablue` | 15.9 | 14.2 | 2.3 | 2.5 | 2.6 | 9.9 | 61.5 | – |
| `dispatch` | 15.9 | 14.2 | 2.4 | 2.5 | 2.6 | 9.9 | 69.4 | – |
| `fannkuch` | 16.9 | 14.2 | 2.4 | 2.5 | 2.6 | 10.0 | 70.5 | – |
| `fib` | 15.9 | 14.2 | 2.3 | 2.4 | 2.6 | 9.9 | 68.0 | 2.1 |
| `json` | 69.6 | 69.2 | – | – | – | 27.3 | 78.5 | – |
| `json-deep` | 17.0 | 14.3 | – | – | – | 12.0 | 61.7 | – |
| `lists` | 26.1 | 24.7 | 4.2 | 3.4 | 3.5 | 14.2 | 72.3 | – |
| `loop` | 15.9 | 14.3 | 2.3 | 2.4 | 2.6 | 9.8 | 69.2 | 2.1 |
| `mandel` | 15.9 | 14.2 | 2.5 | 2.5 | 2.7 | 9.9 | 70.0 | 2.1 |
| `maps` | 28.3 | 26.1 | 10.4 | 7.9 | 8.1 | 16.3 | 76.8 | – |
| `nbody` | 16.0 | 14.1 | 2.7 | 2.7 | 2.8 | 10.6 | 70.8 | 2.1 |
| `regex-ish` | 33.6 | 33.0 | 15.5 | 15.7 | 16.0 | 23.7 | 103.5 | – |
| `richards` | 15.9 | 14.1 | 2.3 | 2.4 | 2.6 | 10.0 | 68.3 | – |
| `sieve` | 51.3 | 47.6 | 10.6 | 6.4 | 6.8 | 12.1 | 86.0 | 2.3 |
| `spectral` | 15.9 | 14.2 | 2.7 | 2.6 | 2.8 | 10.2 | 70.6 | – |
| `strbuild` | 15.9 | 14.2 | 2.7 | 2.7 | 2.7 | 9.9 | 61.8 | 2.1 |
| `strings` | 24.9 | 26.7 | 9.7 | 7.6 | 7.9 | 14.7 | 77.1 | – |
| `trees` | 17.6 | 14.5 | 6.0 | 7.9 | 7.7 | 11.0 | 73.3 | – |
| `words` | 25.2 | 25.5 | 4.8 | 3.9 | 3.9 | 14.7 | 76.0 | – |
| `hello` | 15.9 | 14.2 | 2.2 | 2.4 | 2.5 | 9.9 | 61.1 | 2.1 |
| `startup-0` | 15.9 | 14.2 | 2.2 | 2.4 | 2.5 | 9.8 | 61.2 | 2.1 |
| `startup-1` | 15.9 | 14.2 | 2.2 | 2.4 | 2.5 | 9.9 | 61.1 | – |
| `startup-10` | 15.9 | 14.5 | – | – | – | 17.0 | 64.7 | – |

## How steady the numbers are

The geometric means of the compute set, pass by pass:

| compute set | `cig run` ÷, pass 1 | `cig run` ÷, pass 2 | stage b ÷, pass 1 | stage b ÷, pass 2 |
|---|---:|---:|---:|---:|
| Lua | 3.83 | 4.19 | 1.87 | 2.05 |
| LuaJIT `-joff` | 5.39 | 5.83 | 2.64 | 2.85 |
| LuaJIT | 10.0 | 11.1 | 4.91 | 5.41 |
| CPython | 1.49 | 1.60 | 0.78 | 0.84 |
| Node | 1.54 | 1.74 | 0.79 | 0.89 |
| Rust | 63.5 | 71.6 | 24.2 | 27.2 |

Each side, pass against pass and within each pass ("last third ÷ first third" compares the median of a pair's last third of rounds with its first; over 1, the side slowed down as the pass went on), and how many programs had a coefficient of variation over 5%:

| side | programs | pass 2 ÷ pass 1 | last third ÷ first third, pass 1 | last third ÷ first third, pass 2 | CV over 5%, pass 1 | CV over 5%, pass 2 |
|---|---:|---:|---:|---:|---:|---:|
| `cig run` | 25 | 1.22 | 1.18 | 1.11 | 25 | 25 |
| stage a | 25 | 1.20 | 1.12 | 1.07 | 25 | 25 |
| stage b | 25 | 1.22 | 1.13 | 1.09 | 25 | 25 |
| `cig run --dry-run` | 24 | 1.16 | 1.12 | 1.05 | 24 | 23 |
| stage a, `--dry-run` | 24 | 1.22 | 1.15 | 1.09 | 23 | 22 |
| stage b, `--dry-run` | 24 | 1.25 | 1.17 | 1.11 | 24 | 23 |
| `cig run`, new folder | 1 | 2.18 | 2.04 | 1.57 | 1 | 1 |
| Lua | 21 | 1.04 | 1.03 | 0.99 | 20 | 18 |
| LuaJIT `-joff` | 21 | 1.05 | 1.02 | 1.00 | 18 | 18 |
| LuaJIT | 21 | 1.03 | 1.02 | 1.00 | 20 | 19 |
| CPython | 25 | 1.06 | 1.03 | 1.00 | 25 | 22 |
| Node | 24 | 1.02 | 1.02 | 1.01 | 23 | 23 |
| Rust | 8 | 1.01 | 0.98 | 0.99 | 8 | 8 |
| CPython, idiomatic | 3 | 1.05 | 1.05 | 0.99 | 3 | 3 |
| Node, idiomatic | 3 | 0.98 | 1.02 | 0.99 | 3 | 2 |
| Rust, idiomatic | 1 | 1.00 | 0.97 | 0.99 | 1 | 0 |

Two things moved during the run, and the table shows both:

- **The other languages moved a little.** Pass 2 against pass 1, at the geometric mean of each side's programs: Lua 1.04, LuaJIT 1.03, CPython 1.06, Node 1.02, Rust 1.01; CPython's short programs as much as its long ones (`hello` 12.8 and 13.7 ms, `nbody` 164.2 and 177.0). The run does not show why: the machine was on battery throughout, and pass 2 was not the hotter one (the hottest round start was 92 °C in pass 1 and 87 in pass 2).
- **cig moved more**, its shortest programs most, from its run history ([#252](https://github.com/otmof-ops/cig-script/issues/252)). Every cig run reads the whole run index before it starts, and a real `cig run` reads it twice; the index gains a line or two with every run, and the history reached 5016 runs by the last round. So cig's start-up grew through the run: `hello` took 4.3 ms with `cig run` over pass 1 and 7.8 ms over pass 2, 2.8 and 4.8 ms compiled. The other languages keep no history and moved little.

The times above pool both passes, so they include both. The pairs whose medians in the two passes differ by more than 10%:

| program | side | pass 1, ms | pass 2, ms |
|---|---|---:|---:|
| `closures` | `cig run` | 49.8 | 56.2 |
| `closures` | stage a | 48.5 | 54.8 |
| `closures` | stage b, `--dry-run` | 38.1 | 42.4 |
| `deltablue` | `cig run` | 9.6 | 13.2 |
| `deltablue` | stage a | 8.0 | 10.1 |
| `deltablue` | stage b | 3.8 | 5.7 |
| `deltablue` | `cig run --dry-run` | 8.1 | 9.7 |
| `deltablue` | stage a, `--dry-run` | 7.7 | 9.6 |
| `deltablue` | stage b, `--dry-run` | 3.4 | 5.2 |
| `dispatch` | `cig run` | 75.2 | 82.8 |
| `dispatch` | stage a | 73.9 | 82.3 |
| `dispatch` | stage b | 33.1 | 37.0 |
| `dispatch` | stage b, `--dry-run` | 33.3 | 37.0 |
| `fannkuch` | stage b | 72.1 | 79.8 |
| `fannkuch` | stage a, `--dry-run` | 308.3 | 341.5 |
| `fib` | `cig run` | 58.3 | 64.8 |
| `fib` | stage a | 54.7 | 60.5 |
| `fib` | stage b | 32.4 | 36.3 |
| `fib` | `cig run --dry-run` | 56.4 | 62.5 |
| `fib` | stage a, `--dry-run` | 54.1 | 60.1 |
| `fib` | stage b, `--dry-run` | 31.1 | 34.7 |
| `json-deep` | `cig run` | 6.9 | 10.0 |
| `json-deep` | stage a | 5.1 | 7.2 |
| `json-deep` | stage b | 4.6 | 6.6 |
| `json-deep` | `cig run --dry-run` | 5.4 | 6.7 |
| `json-deep` | stage a, `--dry-run` | 4.7 | 6.9 |
| `json-deep` | stage b, `--dry-run` | 4.3 | 6.0 |
| `lists` | `cig run` | 30.2 | 34.1 |
| `lists` | stage a | 28.3 | 31.5 |
| `lists` | stage b | 25.8 | 28.9 |
| `lists` | `cig run --dry-run` | 28.3 | 31.3 |
| `lists` | stage a, `--dry-run` | 27.7 | 31.0 |
| `loop` | `cig run` | 62.0 | 68.4 |
| `loop` | stage b | 16.2 | 18.4 |
| `loop` | stage b, `--dry-run` | 15.7 | 18.0 |
| `mandel` | stage b | 67.1 | 74.0 |
| `mandel` | stage a, `--dry-run` | 346.7 | 385.6 |
| `mandel` | stage b, `--dry-run` | 66.7 | 74.0 |
| `maps` | `cig run` | 38.8 | 43.8 |
| `maps` | stage b | 28.0 | 31.1 |
| `nbody` | `cig run` | 454.9 | 502.6 |
| `nbody` | stage b | 184.9 | 204.2 |
| `nbody` | `cig run --dry-run` | 447.7 | 495.8 |
| `nbody` | stage b, `--dry-run` | 183.5 | 203.7 |
| `regex-ish` | `cig run` | 39.5 | 43.8 |
| `regex-ish` | stage b | 31.4 | 34.6 |
| `regex-ish` | stage a, `--dry-run` | 37.5 | 41.3 |
| `regex-ish` | stage b, `--dry-run` | 30.6 | 34.2 |
| `richards` | `cig run` | 22.6 | 26.8 |
| `richards` | stage a | 21.3 | 24.3 |
| `richards` | stage b | 10.8 | 13.0 |
| `richards` | `cig run --dry-run` | 21.2 | 23.7 |
| `richards` | stage a, `--dry-run` | 21.1 | 23.3 |
| `richards` | stage b, `--dry-run` | 10.6 | 12.6 |
| `sieve` | `cig run` | 161.4 | 177.8 |
| `sieve` | stage a, `--dry-run` | 158.2 | 176.9 |
| `spectral` | `cig run` | 204.3 | 225.2 |
| `spectral` | stage b, `--dry-run` | 76.5 | 84.2 |
| `strbuild` | `cig run` | 8.4 | 11.8 |
| `strbuild` | stage a | 6.8 | 8.7 |
| `strbuild` | stage b | 5.6 | 7.6 |
| `strbuild` | `cig run --dry-run` | 7.0 | 8.5 |
| `strbuild` | stage a, `--dry-run` | 6.4 | 8.4 |
| `strbuild` | stage b, `--dry-run` | 5.0 | 7.3 |
| `strings` | `cig run` | 22.8 | 26.8 |
| `strings` | stage a | 21.8 | 24.1 |
| `strings` | `cig run --dry-run` | 21.2 | 23.4 |
| `strings` | stage a, `--dry-run` | 21.1 | 23.6 |
| `strings` | stage b, `--dry-run` | 16.1 | 18.0 |
| `trees` | stage a | 361.5 | 398.7 |
| `trees` | stage b, `--dry-run` | 239.5 | 265.9 |
| `words` | `cig run` | 36.3 | 41.1 |
| `words` | stage b | 24.1 | 26.8 |
| `hello` | `cig run` | 4.3 | 7.8 |
| `hello` | stage a | 2.6 | 4.7 |
| `hello` | stage b | 2.8 | 4.8 |
| `hello` | `cig run --dry-run` | 3.0 | 4.3 |
| `hello` | stage a, `--dry-run` | 2.3 | 4.4 |
| `hello` | stage b, `--dry-run` | 2.3 | 4.3 |
| `hello` | `cig run`, new folder | 5.2 | 11.2 |
| `startup-0` | `cig run` | 4.4 | 7.9 |
| `startup-0` | stage a | 2.7 | 4.7 |
| `startup-0` | stage b | 2.8 | 4.7 |
| `startup-0` | `cig run --dry-run` | 3.0 | 4.2 |
| `startup-0` | stage a, `--dry-run` | 2.4 | 4.4 |
| `startup-0` | stage b, `--dry-run` | 2.3 | 4.3 |
| `startup-1` | `cig run` | 4.5 | 7.6 |
| `startup-1` | stage a | 2.8 | 4.6 |
| `startup-1` | stage b | 2.7 | 4.7 |
| `startup-1` | `cig run --dry-run` | 3.1 | 4.3 |
| `startup-1` | stage a, `--dry-run` | 2.3 | 4.4 |
| `startup-1` | stage b, `--dry-run` | 2.4 | 4.4 |
| `startup-10` | `cig run` | 4.7 | 7.8 |
| `startup-10` | stage a | 2.8 | 4.9 |
| `startup-10` | stage b | 2.8 | 4.8 |
| `startup-10` | `cig run --dry-run` | 3.0 | 4.4 |
| `startup-10` | stage a, `--dry-run` | 2.5 | 4.3 |
| `startup-10` | stage b, `--dry-run` | 2.3 | 4.5 |

## cig-script's own suite

`cig bench bench/bench.toml` on cig 1.24.0 as released, between the two passes, as `cig bench` prints it (its JSON, with every sample, is in the results folder). Its compute rows are the same programs, timed by `cig bench`'s own harness, in a quiet stretch with no other language interleaved; its start-up rows run each program 20 times in a row. Each sample runs in a fresh scratch home, with no run history but its own runs, which is why its `hello` and `startup-0` are faster than this paper's ([#252](https://github.com/otmof-ops/cig-script/issues/252)); its `startup-1` and `startup-10` are slower than this run's first rounds, which this paper does not explain.

**cig bench**: cig, cig 1.24.0 — Intel(R) Core(TM) i5-8265U CPU @ 1.60GHz, 8 cores, linux 7.2.11+parrot7.1-amd64, governor powersave

| case | group | median ms | p10 to p90 ms | CV | RSS MB |
|---|---|---:|---|---:|---:|
| `hello` | start-up | 2.3 | 2.3 to 2.4 | 0.5% | 11.1 |
| `startup-0` | start-up | 2.8~ | 2.3 to 5.6 | 35.5% | 11.1 |
| `startup-1` | start-up | 5.6 | 5.6 to 5.6 | 0.2% | 11.1 |
| `startup-10` | start-up | 5.8 | 5.8 to 5.8 | 0.3% | 11.1 |
| `burn one write` | start-up | 6.3 | 6.2 to 6.4 | 1.0% | 11.1 |
| `closures` | compute | 46.6 | 46.5 to 47.0 | 0.4% | 11.1 |
| `csv` | compute | 45.5 | 45.2 to 45.9 | 0.5% | 33.5 |
| `deltablue` | compute | 7.1 | 7.0 to 7.3 | 1.3% | 11.1 |
| `dispatch` | compute | 71.8 | 70.3 to 72.0 | 0.9% | 11.1 |
| `fannkuch` | compute | 298.6 | 296.6 to 299.9 | 0.4% | 11.1 |
| `fib` | compute | 56.3 | 55.7 to 57.4 | 1.2% | 11.1 |
| `json-deep` | compute | 4.6 | 4.6 to 4.7 | 0.9% | 11.1 |
| `json` | compute | 73.5 | 71.6 to 73.8 | 1.3% | 65.9 |
| `lists` | compute | 29.4 | 29.3 to 29.5 | 0.2% | 18.1 |
| `loop` | compute | 63.6 | 63.5 to 63.6 | 0.0% | 11.1 |
| `mandel` | compute | 404.1 | 392.3 to 404.2 | 1.1% | 11.1 |
| `maps` | compute | 40.7 | 40.4 to 41.7 | 1.3% | 21.4 |
| `nbody` | compute | 513.7 | 508.4 to 560.1 | 3.7% | 11.1 |
| `regex-ish` | compute | 40.3 | 40.2 to 40.5 | 0.2% | 29.7 |
| `richards` | compute | 21.9 | 21.7 to 22.0 | 0.4% | 11.1 |
| `sieve` | compute | 173.2 | 169.9 to 174.1 | 1.1% | 43.3 |
| `spectral` | compute | 224.7 | 220.1 to 229.1 | 1.4% | 11.1 |
| `strbuild` | compute | 6.6 | 6.6 to 6.7 | 0.5% | 11.1 |
| `strings` | compute | 23.1 | 23.0 to 23.2 | 0.3% | 22.9 |
| `trees` | compute | 414.8 | 412.3 to 419.5 | 0.6% | 11.1 |
| `words` | compute | 39.2 | 38.5 to 39.5 | 0.9% | 23.1 |
| `burn small tree` | kernel | 7.6 | 7.6 to 7.8 | 1.1% | 11.1 |
| `burn big tree` | kernel | 168.3 | 166.7 to 173.8 | 1.6% | 23.0 |
| `unburn small tree` | kernel | 10.5 | 10.4 to 10.7 | 0.8% | 13.3 |
| `unburn big tree` | kernel | 260.7~ | 253.5 to 294.8 | 5.7% | 22.1 |
| `overwrite big file` | kernel | 580.7 | 578.3 to 581.3 | 0.2% | 125.6 |
| `restore big file` | kernel | 697.1 | 693.9 to 735.5 | 2.3% | 53.8 |
| `hop, 2,000 files unchanged` | kernel | 97.3 | 91.8 to 100.0 | 2.9% | 47.0 |

| case | measure | value |
|---|---|---:|
| `burn small tree` | `tin-growth` | 0 B |
| `burn small tree` | `disk-per-byte` | 4.80x |
| `burn small tree` | `fsyncs-per-op` | 1.10 |
| `burn big tree` | `tin-growth` | 0 B |
| `burn big tree` | `disk-per-byte` | 1.04x |
| `burn big tree` | `fsyncs-per-op` | 1.01 |
| `unburn small tree` | `tin-growth` | 0 B |
| `unburn small tree` | `fsyncs-per-op` | 0.09 |
| `unburn big tree` | `tin-growth` | 0 B |
| `unburn big tree` | `fsyncs-per-op` | 0.01 |
| `overwrite big file` | `tin-growth` | 16.0 KiB |
| `overwrite big file` | `disk-per-byte` | 0.00x |
| `overwrite big file` | `fsyncs-per-op` | 15.00 |
| `overwrite big file` | `file:$CIGSCRIPT_HOME/work/big.bin` | 32.0 MiB |
| `restore big file` | `tin-growth` | 0 B |
| `restore big file` | `fsyncs-per-op` | 138.00 |
| `restore big file` | `file:$CIGSCRIPT_HOME/work/big.bin` | 32.0 MiB |
| `hop, 2,000 files unchanged` | `tin-growth` | 0 B |
| `hop, 2,000 files unchanged` | `fsyncs-per-op` | 12.00 |

## How it was measured

- **A whole process.** Each time is the wall time of one process, from just before it is started until it has exited and been reaped: the time a person waits. An interpreter's start-up is part of every number, as it is when you run a script; the four start-up programs show it alone. Starting and reaping a process costs the harness 0.63 ms on this machine (`true`, the median of 50, in pass 1; 0.81 in pass 2), and nothing is subtracted from any side. The harness waits on the process's pidfd, which wakes the moment it exits.
- **Every run checked.** A run counts only if it exits 0, prints exactly the program's `.expect` file, byte for byte, and writes nothing to stderr but cig's own summary of a run (`burned 1 op (0 irreversible), run …` after `csv`'s burn, `dry-run plan: 0 ops` and `nothing was changed` after a dry run). A program and side that fails this once, in any round, gets no time at all. Failed pairs: 0 in pass 1, 0 in pass 2.
- **Rounds in a random order.** A pass is 31 rounds. In each round each of the 275 program-and-side pairs runs once, in an order drawn from a seeded generator (the seeds are in the conditions below). So the clock and the temperature, as they drift, fall on every program and every side alike, and cig's run history, which slows only cig's sides, falls on each of their programs alike. Each pair has one checked run before the rounds, not timed.
- **Two passes.** The whole run was made twice, with cig-script's and cig-compile's own suites between the two. A time below is the median of a pair's samples over both passes (8525 samples in pass 1, 8525 in pass 2); how far the passes agree is under [How steady the numbers are](#how-steady-the-numbers-are).
- **A folder per program and side**, at the same path for every run, as a project is. The script is copied in once. Before each run the files the last run left there are removed (`csv` writes one), and when that changes the disk, the disk is synced before the clock starts, so no run pays for the harness's own writes.
- **The run record is in the time.** `cig run` writes a record of every run (and, for a run that burns, the journal `cig unburn` reads): that is part of what running a script with cig costs, so it is in the headline. The same programs as dry runs (`--dry-run`, no record) are measured too; see [The run record](#the-run-record).
- **cig as released**: the Linux assets of cig 1.24.0 and cigcompile 0.5.0, each held to its release's sha256 file before the run, and the programs cigcompile 0.5.0 builds from the same scripts on that cig. Every cig side runs with `CIG_SEAL_KEY=file`, `CIG_MAX_STEPS=0`, `CIG_REPORTS=never`, `CIG_DOCTOR=0` and `NO_COLOR=1`, the settings `cigcompile bench` uses; none makes a run that succeeds do less (the key for sealed secrets is kept in a file, as cig's own tests keep it; the step budget is off because some programs run past its ten million steps; the other two speak only when a run fails).
- **The others as their users get them.** Lua 5.4.9 built from its source at the `v5.4.9` tag with the flags of the release makefile's `make linux` (`-O2`), and LuaJIT 2.1 (commit `63c8f1a`) with its own makefile's defaults, both by gcc 14.2; Node 22.23.5 as nodejs.org releases it (the Linux x64 tarball, its sha256 the one the release's `SHASUMS256.txt` lists; that file's signature was not checked); CPython 3.13.5 as the distribution packages it (`python3.13` `3.13.5-2+deb13u5`, as `dpkg` reported it on the machine after the run); Rust 1.100.0 at `-C opt-level=3`, what `cargo build --release` uses. LuaJIT is measured twice: with its compiler off (`-joff`), which is its interpreter alone, and as it runs by default. Each binary's sha256 is in the conditions below.
- **Nothing tuned.** No process is pinned to a core and nothing is warmed between rounds; the machine runs as it is set up, under its own lock, one job at a time, and the conditions below give its clock, governor, power profile and temperature.

## Conditions

|  | pass 1 | pass 2 |
|---|---|---|
| started (UTC) | 2026-11-15T16:21:07Z | 2026-11-15T16:34:06Z |
| processor | Intel(R) Core(TM) i5-8265U CPU @ 1.60GHz | Intel(R) Core(TM) i5-8265U CPU @ 1.60GHz |
| logical cores | 8 | 8 |
| highest clock, MHz | 3900 | 3900 |
| kernel | 7.2.11+parrot7.1-amd64 | 7.2.11+parrot7.1-amd64 |
| frequency driver | intel_pstate | intel_pstate |
| CPU governor | powersave | powersave |
| energy preference | balance_performance | balance_performance |
| turbo | on | on |
| power profile | balanced | balanced |
| transparent huge pages | [always] madvise never | [always] madvise never |
| on mains power | no | no |
| load average at the start | 0.68 0.39 0.17 | 1.01 1.07 0.74 |
| package °C at the start | 60 | 64 |
| cig's run records at the start | 0 | 2660 |
| harness | Python 3.13.5 | Python 3.13.5 |
| rounds | 31 | 31 |
| seed of the rounds' order | 1 | 2 |
| `true`, median ms | 0.633 | 0.813 |
| cig's run records at the last round | 2356 | 5016 |
| mean clock at a round's start, lowest, MHz | 787 | 738 |
| mean clock at a round's start, highest, MHz | 3357 | 3400 |
| package °C at a round's start, highest | 92 | 87 |
| cig | cig 1.24.0 | cig 1.24.0 |
| cigcompile | cigcompile 0.5.0 (makes programs for cig 1.24.0) | cigcompile 0.5.0 (makes programs for cig 1.24.0) |
| Lua | Lua 5.4.9  Copyright (C) 1994-2026 Lua.org, PUC-Rio | Lua 5.4.9  Copyright (C) 1994-2026 Lua.org, PUC-Rio |
| LuaJIT | LuaJIT 2.1.1794386141 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/ | LuaJIT 2.1.1794386141 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/ |
| CPython | Python 3.13.5 | Python 3.13.5 |
| Node | v22.23.5 | v22.23.5 |
| rustc | rustc 1.100.0 (0ef1bc386 2026-11-09) | rustc 1.100.0 (0ef1bc386 2026-11-09) |
| sha256 of `cig-v1.24.0-linux-x86_64` | `957ddff2c27c9baa38b5aa43361e165e209ebb3d6a3c82cfa28d7b7777e91b3f` | `957ddff2c27c9baa38b5aa43361e165e209ebb3d6a3c82cfa28d7b7777e91b3f` |
| sha256 of `cigcompile-v0.5.0-linux-x86_64` | `4ba7d3e10d5c82b118d7f6c792b6c96375b09de68897dfb9a02e947bb77c7ef0` | `4ba7d3e10d5c82b118d7f6c792b6c96375b09de68897dfb9a02e947bb77c7ef0` |
| sha256 of `lua5.4` | `adc4b6a990abb309187207b1650489b02173bd40e69710130e37a77245cfd93f` | `adc4b6a990abb309187207b1650489b02173bd40e69710130e37a77245cfd93f` |
| sha256 of `luajit` | `e525f77c526139de63f130ae8a72aca04d41ec4249c375f96185d581a4a0e5c1` | `e525f77c526139de63f130ae8a72aca04d41ec4249c375f96185d581a4a0e5c1` |
| sha256 of `python3` | `889c603f0d17cb54060951bcf4c4f9b8c9ebd9e52b392c70209bbb9755d797d9` | `889c603f0d17cb54060951bcf4c4f9b8c9ebd9e52b392c70209bbb9755d797d9` |
| sha256 of `node` | `2f822d544601dfff107f57d945b649d15c5bf6302595735a6bd9cdc7ee9669bd` | `2f822d544601dfff107f57d945b649d15c5bf6302595735a6bd9cdc7ee9669bd` |
| sha256 of `rustc` | `5073422953727c9427a76b7fce03aa9404da46315e5a1e4965389512a41e8db0` | `5073422953727c9427a76b7fce03aa9404da46315e5a1e4965389512a41e8db0` |

## Run it again

```sh
# one pass (every side whose tool is on PATH or named; a missing one is said and left out)
python3 bench/compare/compare.py --out pass1 --seed 1 --rounds 31 \
  --cig ./cig-v1.24.0-linux-x86_64 --cigcompile ./cigcompile-v0.5.0-linux-x86_64 \
  --lua lua5.4 --luajit luajit --python python3 --node node --rustc rustc
# this file's numbers, made and checked
python3 bench/compare/report.py --pass bench/results/2026-11-15/pass1 --pass bench/results/2026-11-15/pass2 \
  --numbers numbers.json --render bench/results/2026-11-15/paper.md.in docs/BENCHMARKS-2026-11-15.md
python3 bench/compare/check.py --pass bench/results/2026-11-15/pass1 --pass bench/results/2026-11-15/pass2 \
  --numbers numbers.json --render bench/results/2026-11-15/paper.md.in docs/BENCHMARKS-2026-11-15.md
```

## What this does not show

- **One machine.** A 2018 laptop processor, on battery and on its everyday settings (the conditions are above). Another processor, another operating system or another power setting moves every number, the ratios with them.
- **Small programs, timed whole.** Most of these programs run for tens of milliseconds, so start-up is a large part of most of these times, and a language that starts slowly but runs fast (Node, whose start-up the `hello` row shows) is behind on whole-process time by its start-up alone. That is the time a script's user waits; it is not the speed of a long-running program, where a just-in-time compiler such as LuaJIT's or V8's has time to pay for itself.
- **cig as it ships today.** cig 1.24.0 and cigcompile 0.5.0, both released. Work merged since (the static build's own copy routines, cig-script#251) and work in progress (profile-guided optimization, cig-script#243) are not in these numbers.
- **The other languages as their users get them.** No other runtime was tuned or given flags beyond the defaults (Rust's `opt-level=3` is what `cargo build --release` uses). CPython is 3.13.5, the version the machine's distribution ships.
- **Not every program in every language.** Rust has eight of the 25 programs; Lua has no `json`, `json-deep`, `csv` or `startup-10`; Node has no `csv`. The three counts beside each geometric mean add up to the programs it covers, and two cells with different counts are not over the same programs.
- **Memory is the peak resident set**, read when the process exits: what the program held at most, its own allocations and the pages of its files together, so a program with a big executable starts with a big number.
