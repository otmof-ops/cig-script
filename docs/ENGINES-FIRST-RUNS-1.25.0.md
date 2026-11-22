# A first run on the VM, cig 1.25.0 as released: `hello`, a 500-line script and cigo

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

`cig run --engine vm` lowers a script to bytecode the first time it runs it, and keeps the module in the tin for the runs after, until the script or cig changes. This is what the first run costs and what the later ones gain, on three scripts: `hello`, the smallest of [the benchmark programs](../bench/compute/hello.cig); [`bench/engines/lines-500.cig`](../bench/engines/lines-500.cig), 500 lines of small functions; and cigo's `orchestrate.cig`, 20,664 lines (cig-orchestrator at `0d6c934`, run with no arguments, which prints its help). It was measured for [#254](https://github.com/otmof-ops/cig-script/issues/254), beside [the benchmark programs](ENGINES-1.25.0.md), on the same machine with the same cig, 1.25.0 as released, by the same method. [The run of 2026-11-22](ENGINES-FIRST-RUNS-2026-11-22.md) measured the same on a development build, whose tree-walker was the slower one: it started cigo in 53.4 ms.

Every measured number here is made from the samples in [`bench/results/1.25.0-engines/start/`](../bench/results/1.25.0-engines/start/) by [`bench/compare/report.py`](../bench/compare/report.py), and made again by [`bench/compare/check.py`](../bench/compare/check.py).

## In short

- **From the second run on, the VM starts cigo in 10.6 ms where the tree-walker takes 36.2**: 0.29 of the time. The tree-walker parses and checks 20,664 lines at every start; the VM loads the module it kept. The 500-line script takes 2.4 ms against 2.5, 0.96 of the time, and `hello` 2.0 ms against 1.9, 1.04.
- **The first run pays for the lowering**: 2.0 ms for `hello`, 1.05 times the tree-walker's 1.9; 7.7 ms for the 500-line script, 3.14 times the tree-walker's 2.5; and 305.9 ms for cigo, 8.45 times the tree-walker's 36.2. It is paid once for a script on a build of cig: an edit to the script, or an update of cig, pays it again.
- **This is why the tree-walker is still the default.** One of #254's conditions for making the VM the default is that, on the test machine with cig as released, a first run of `hello` and of a 500-line script is within 10% of the tree-walker's. On this development machine, with cig as released, they take 1.05 and 3.14 times as long. The VM is there for a run, a shell or a machine that asks for it (`--engine vm`, `CIG_ENGINE=vm`, `cig config engine vm`).

## The three scripts

The median time in milliseconds over 31 rounds in each of 2 passes, and each VM side's time divided by the tree-walker's:

| program | `cig run --engine tree` | `cig run --engine vm` | `cig run --engine vm`, empty tin | vm ÷ tree | vm, empty tin ÷ tree |
|---|---:|---:|---:|---:|---:|
| `lines-500` | 2.5 | 2.4 | 7.7 | 0.96 | 3.14 |
| `orchestrate` | 36.2 | 10.6 | 305.9 | 0.29 | 8.45 |
| `hello` | 1.9 | 2.0 | 2.0 | 1.04 | 1.05 |

Failed pairs: 0 in pass 1, 0 in pass 2. The method is [the benchmark programs' run's](ENGINES-1.25.0.md#how-it-was-measured): whole processes, every output checked, rounds in a random order, a home for each side.

## Conditions

|  | pass 1 | pass 2 |
|---|---|---|
| started (UTC) | 2026-11-22T22:26:06Z | 2026-11-22T22:26:20Z |
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
| load average at the start | 1.29 1.12 0.97 | 1.22 1.11 0.97 |
| package °C at the start | 66 | 67 |
| cig's run records at the start | 0 | 105 |
| harness | Python 3.12.3 | Python 3.12.3 |
| rounds | 31 | 31 |
| seed of the rounds' order | 1 | 2 |
| `true`, median ms | 0.285 | 0.276 |
| cig's run records at the last round | 93 | 198 |
| mean clock at a round's start, lowest, MHz | 818 | 909 |
| mean clock at a round's start, highest, MHz | 2968 | 3050 |
| package °C at a round's start, highest | 83 | 82 |
