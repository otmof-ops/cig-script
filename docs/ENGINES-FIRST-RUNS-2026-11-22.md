# A first run on the VM, 2026-11-22: a 500-line script and cigo

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

`cig run --engine vm` lowers a script to bytecode the first time it runs it, and keeps the module in the tin for every run after. This is what the first run costs and what the later ones gain, on two scripts longer than the benchmark programs: [`bench/engines/lines-500.cig`](../bench/engines/lines-500.cig), 500 lines of small functions, and cigo's `orchestrate.cig`, 20,664 lines (cig-orchestrator at `0d6c934`, run with no arguments, which prints its help). It was measured for [#254](https://github.com/otmof-ops/cig-script/issues/254), beside [the 25 benchmark programs](ENGINES-2026-11-22.md), on the same machine with the same build, by the same method.

Every number is made from the samples in [`bench/results/2026-11-22-engines/start/`](../bench/results/2026-11-22-engines/start/) by [`bench/compare/report.py`](../bench/compare/report.py), and made again by [`bench/compare/check.py`](../bench/compare/check.py).

## In short

- **From the second run on, the VM starts cigo in 10.8 ms where the tree-walker takes 52.1**: 0.21 of the time. The tree-walker parses and checks 20,664 lines at every start; the VM loads the module it kept. The 500-line script takes 2.5 ms against 2.9, 0.85 of the time.
- **The first run pays for the lowering**: 8.2 ms for the 500-line script, 2.85 times the tree-walker's 2.9; and 384.0 ms for cigo, 7.36 times the tree-walker's 52.1. It is paid once for a script on a cig: an edit to the script, or an update of cig, pays it again.
- **This is why the tree-walker is still the default.** One of #254's conditions for making the VM the default is that a first run of a 500-line script is within 10% of the tree-walker's. It is 2.85 times. The VM is there for a run, a shell or a machine that asks for it (`--engine vm`, `CIG_ENGINE=vm`, `cig config engine vm`).

## The two scripts

The median time in milliseconds over 31 rounds in each of 2 passes, and each VM side's time divided by the tree-walker's:

| program | `cig run --engine tree` | `cig run --engine vm` | `cig run --engine vm`, empty tin | vm ÷ tree | vm, empty tin ÷ tree |
|---|---:|---:|---:|---:|---:|
| `lines-500` | 2.9 | 2.5 | 8.2 | 0.85 | 2.85 |
| `orchestrate` | 52.1 | 10.8 | 384.0 | 0.21 | 7.36 |

Failed pairs: 0 in pass 1, 0 in pass 2. The method is [the benchmark programs' run's](ENGINES-2026-11-22.md#how-it-was-measured): whole processes, every output checked, rounds in a random order, a home for each side.

## Conditions

|  | pass 1 | pass 2 |
|---|---|---|
| started (UTC) | 2026-11-22T11:24:48Z | 2026-11-22T11:26:42Z |
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
| load average at the start | 1.13 0.98 1.23 | 1.13 1.03 1.22 |
| package °C at the start | 70 | 71 |
| cig's run records at the start | 0 | 70 |
| harness | Python 3.12.3 | Python 3.12.3 |
| rounds | 31 | 31 |
| seed of the rounds' order | 1 | 2 |
| `true`, median ms | 0.280 | 0.283 |
| cig's run records at the last round | 62 | 132 |
| mean clock at a round's start, lowest, MHz | 983 | 1017 |
| mean clock at a round's start, highest, MHz | 3047 | 3323 |
| package °C at a round's start, highest | 80 | 80 |
