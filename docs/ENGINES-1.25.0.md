# `cig run` on each engine, cig 1.25.0 as released: the tree-walker and the VM

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The 25 programs of [`bench/compute`](../bench/compute/), run by one build of cig three ways: `cig run --engine tree`; `cig run --engine vm` with the script's module already in the tin; and `cig run --engine vm` with nothing in the tin, so every run lowers the script before it runs it. It was measured for [#254](https://github.com/otmof-ops/cig-script/issues/254), which lets `cig run` use the VM.

The cig is 1.25.0 as released: the static Linux x86_64 asset, held to its release's sha256 before the run (`423d648340fd0db8fd541319f5dec3f1adf0329a387ebdf5d98c064232a90e46`). The machine is a development machine (the conditions are below), not the benchmark paper's test machine: read the ratios; the times belong to this machine. [The run of 2026-11-22](ENGINES-2026-11-22.md) measured the same three ways on a development build of the change, before the release.

Every measured number here is made from the samples in [`bench/results/1.25.0-engines/main/`](../bench/results/1.25.0-engines/main/) by [`bench/compare/report.py`](../bench/compare/report.py), and made again by [`bench/compare/check.py`](../bench/compare/check.py), a second program written apart from the first to [the same definitions](../bench/compare/SPEC.md).

## In short

- **With its module in the tin, the VM runs the 21 compute programs in 0.57 of the tree-walker's time**, 1.74 times as fast: faster on 19, level on 2, slower on 0. It gains the most on `mandel` and the least on `json`.
- **On the 4 start-up programs** the VM takes 1.01 of the tree-walker's time at the geometric mean: 1 faster, 1 level and 2 slower. `hello` takes 2.5 ms on the tree-walker and 2.7 ms on the VM.
- **A first run pays for the lowering.** With nothing in the tin a run lowers the script, verifies and loads the module, and keeps it. On the compute programs that comes to 0.60 of the tree-walker's time, faster on 18 of 21; on the start-up programs, to 1.10: `hello` takes 2.7 ms against 2.5. These are scripts of 4 to 14 lines; [the first runs of longer scripts](ENGINES-FIRST-RUNS-1.25.0.md) cost far more.

## Every program

The median time of each program, in milliseconds, over 31 rounds in each of 2 passes, and the VM's time divided by the tree-walker's. Under 1, the VM took less time. A program counts as faster below 0.97, slower above 1.03, and level between, judged on the ratio before it is rounded for the table.

| program | `cig run --engine tree` | `cig run --engine vm` | `cig run --engine vm`, empty tin | vm ÷ tree | vm, empty tin ÷ tree |
|---|---:|---:|---:|---:|---:|
| `closures` | 21.6 | 17.6 | 17.8 | 0.81 | 0.82 |
| `csv` | 25.5 | 23.9 | 24.4 | 0.93 | 0.95 |
| `deltablue` | 5.1 | 3.2 | 3.6 | 0.62 | 0.70 |
| `dispatch` | 34.3 | 16.6 | 17.1 | 0.48 | 0.50 |
| `fannkuch` | 159.9 | 39.2 | 40.5 | 0.24 | 0.25 |
| `fib` | 28.9 | 16.6 | 17.0 | 0.58 | 0.59 |
| `json` | 37.8 | 37.1 | 37.5 | 0.98 | 0.99 |
| `json-deep` | 3.9 | 3.8 | 4.2 | 0.96 | 1.08 |
| `lists` | 16.9 | 15.4 | 15.8 | 0.91 | 0.94 |
| `loop` | 30.8 | 8.2 | 8.5 | 0.27 | 0.28 |
| `mandel` | 195.8 | 39.3 | 39.7 | 0.20 | 0.20 |
| `maps` | 18.4 | 15.7 | 16.0 | 0.86 | 0.87 |
| `nbody` | 244.9 | 81.6 | 82.0 | 0.33 | 0.33 |
| `regex-ish` | 16.2 | 12.8 | 13.4 | 0.79 | 0.83 |
| `richards` | 12.3 | 6.2 | 7.0 | 0.51 | 0.57 |
| `sieve` | 89.3 | 38.4 | 39.0 | 0.43 | 0.44 |
| `spectral` | 105.6 | 36.3 | 37.2 | 0.34 | 0.35 |
| `strbuild` | 5.0 | 4.8 | 5.1 | 0.97 | 1.02 |
| `strings` | 10.4 | 8.0 | 8.4 | 0.77 | 0.80 |
| `trees` | 181.6 | 111.9 | 112.8 | 0.62 | 0.62 |
| `words` | 17.9 | 13.2 | 13.8 | 0.74 | 0.77 |
| `hello` | 2.5 | 2.7 | 2.7 | 1.06 | 1.10 |
| `startup-0` | 2.5 | 2.6 | 2.8 | 1.03 | 1.10 |
| `startup-1` | 2.6 | 2.6 | 2.9 | 1.00 | 1.13 |
| `startup-10` | 2.7 | 2.6 | 2.9 | 0.95 | 1.06 |

## How it was measured

As [the benchmark paper](BENCHMARKS-2026-11-15.md#how-it-was-measured) measures, with [`bench/compare/compare.py`](../bench/compare/compare.py):

- **A whole process.** Each time is the wall time of one `cig run`, from just before it is started until it has exited and been reaped. Start-up is in every number.
- **Every run checked.** A run counts only if it exits 0 and prints exactly the program's `.expect` file. Failed pairs: 0 in pass 1, 0 in pass 2.
- **Rounds in a random order.** In each of the 31 rounds of a pass every program runs once on every side, all 75 of them, in an order drawn from a seeded generator. A time is the median of a pair's samples over both passes.
- **One home for each side.** Each of the three sides keeps its own cig home, so one side's tin is not another's and all three carry the same number of run records at every round (a run's start-up grows with its history, [#252](https://github.com/otmof-ops/cig-script/issues/252)).
- **With its module in the tin** means the checked run before the rounds lowered the script and kept its module; every timed run then loads it. **With nothing in the tin** means the side's lowered modules are removed before each run, outside the timed part, and the disk is synced.
- **The same cig settings as the paper**: `CIG_SEAL_KEY=file`, `CIG_MAX_STEPS=0`, `CIG_REPORTS=never`, `CIG_DOCTOR=0` and `NO_COLOR=1`.

## Conditions

|  | pass 1 | pass 2 |
|---|---|---|
| started (UTC) | 2026-11-22T22:23:06Z | 2026-11-22T22:24:32Z |
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
| load average at the start | 0.70 0.92 0.88 | 1.09 1.00 0.92 |
| package °C at the start | 57 | 72 |
| cig's run records at the start | 0 | 875 |
| harness | Python 3.12.3 | Python 3.12.3 |
| rounds | 31 | 31 |
| seed of the rounds' order | 1 | 2 |
| `true`, median ms | 0.291 | 0.278 |
| cig's run records at the last round | 775 | 1650 |
| mean clock at a round's start, lowest, MHz | 1113 | 1077 |
| mean clock at a round's start, highest, MHz | 3394 | 2892 |
| package °C at a round's start, highest | 97 | 98 |

## How steady the numbers are

| side | programs | pass 2 ÷ pass 1 | last third ÷ first third, pass 1 | last third ÷ first third, pass 2 | CV over 5%, pass 1 | CV over 5%, pass 2 |
|---|---:|---:|---:|---:|---:|---:|
| `cig run --engine tree` | 25 | 1.08 | 1.05 | 1.02 | 12 | 8 |
| `cig run --engine vm` | 25 | 1.09 | 1.05 | 1.05 | 15 | 14 |
| `cig run --engine vm`, empty tin | 25 | 1.09 | 1.05 | 1.04 | 14 | 12 |

## What this does not show

- **Not the paper's machine.** The paper's numbers against Lua, CPython, Node and Rust were measured on the test machine with cig 1.24.0 as released. This run compares cig 1.25.0 with itself, on another machine. `cig run` on the VM against the other languages waits for a run on the test machine.
- **Small programs, timed whole.** Most of these programs run for tens of milliseconds; on the shortest the engine is a small part of the time.
- **Not a quiet machine.** The package reached 97 °C in pass 1 and 98 °C in pass 2, and the samples of many pairs vary by more than 5% (the table above). On the programs that take about 3 ms, a ratio within a few hundredths of 1 is level here, whichever side of 1 it falls.
- **A module is one build's.** A cig that has just been updated lowers every script again on its first run: the build of cig is part of the key a module is kept under.
