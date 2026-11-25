# A script's first runs on each engine, 2026-11-25: `hello`, a 500-line script and cigo

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

`cig run --engine vm` lowers a script to bytecode the first time it runs it, and keeps the module in the tin for the runs after. `cig run` on the default (cig-script#259's `auto`) gets a script onto the VM without the user asking and without a run that does the lowering itself:

| run of a script under the default | what runs it | what else happens |
|---|---|---|
| first | the tree-walker | an empty mark is left in the tin; nothing is started |
| second | the tree-walker | a `cig __lower` is left behind, which lowers the script and keeps its module |
| third and later | the VM | |

This is what each of those runs costs, on three scripts: `hello`, the smallest of [the benchmark programs](../bench/compute/hello.cig); [`bench/engines/lines-500.cig`](../bench/engines/lines-500.cig), 500 lines of small functions; and cigo's `orchestrate.cig`, 20,992 lines (cig-orchestrator at `e0c21a7`, run with no arguments, which prints its help). It was measured for [#254](https://github.com/otmof-ops/cig-script/issues/254) and [#259](https://github.com/otmof-ops/cig-script/issues/259), beside [the benchmark programs](ENGINES-2026-11-25.md), on the same machine with the same cig: commit `7e76bb8` (merged to `main` as `cebd9dd`, [#287](https://github.com/otmof-ops/cig-script/pull/287)), built as a release is.

Every measured number here is made from the samples in [`bench/results/2026-11-25-engines/`](../bench/results/2026-11-25-engines/) (`start/`, and where the text says so `start-one-cpu/`, `before/` and `before-one-cpu/`) by [`bench/compare/report.py`](../bench/compare/report.py), and made again by [`bench/compare/check.py`](../bench/compare/check.py).

## In short

- **A first run under the default is the tree-walker's.** `hello` takes 2.6 ms against the tree-walker's 2.7, 0.96 of the time (within 10%); the 500-line script 3.2 ms against 3.3, 0.97 (within 10%); cigo 30.7 ms against 30.5, 1.01 (within 10%). With every process held to one CPU the three come to 0.92, 0.95 and 1.00. #259's bar for making `auto` the default is a first run within 10% of the tree-walker's.
- **A second run starts the lowering, and pays for starting it.** The tree-walker runs the script again, and before `cig` returns it starts a `cig __lower`: `hello` takes 3.0 ms, 1.13 of the tree-walker's time; the 500-line script 3.6 ms, 1.10; cigo 30.9 ms, 1.01. That is with a CPU free for the process left behind. Held to one CPU, that process shares it with the run that started it, and the run's exit waits behind the lowering: 1.35 for `hello`, 1.77 for the 500-line script and 1.08 for cigo. A script pays it once for each text it has had under one build of cig; `auto_lower = off` turns the lowering off.
- **From the third run on, the default is on the VM.** It starts cigo in 10.5 ms where the tree-walker takes 30.5: 0.35 of the time. The tree-walker parses and checks the whole script at every start; the VM loads the module it kept. The 500-line script takes 2.7 ms against 3.3, 0.84 of the time. On `hello` the engines take the same time, and its three medians (the tree-walker's 2.7 ms, the VM's by name 2.5, the default's 2.8) differ by what copies of one run differ by here.
- **Asked for by name, the VM lowers a script inside its first run.** `cig run --engine vm` with nothing in the tin: 2.7 ms for `hello`, 1.03 times the tree-walker's; 7.4 ms for the 500-line script, 2.28 times; and 304.1 ms for cigo, 9.97 times. The default leaves that work to a process of its own, started once the script's own work is done.
- **Why the lowering waits for the second run.** The build before this one (`main` at `da5cd6f`) started it on a script's first run. Measured the same way, that first run took 1.22 of the tree-walker's time on `hello` and 1.12 on the 500-line script with a CPU free for the process left behind, and 1.58 and 1.82 held to one CPU. Starting a process costs a few tenths of a millisecond, which is little beside most runs and a good part of the smallest; and a script that is run once never gets it back. So a first run now only leaves a mark, and the process is started for a script that has come back.

## Every run

The median time in milliseconds over 101 rounds in each of 2 passes, with two CPUs for each measured process:

| program | `cig run --engine tree` | `cig run` (default), first run | `cig run` (default), second run | `cig run` (default) | `cig run --engine vm`, empty tin | `cig run --engine vm` |
|---|---:|---:|---:|---:|---:|---:|
| `lines-500` | 3.3 | 3.2 | 3.6 | 2.7 | 7.4 | 2.9 |
| `orchestrate` | 30.5 | 30.7 | 30.9 | 10.5 | 304.1 | 10.5 |
| `hello` | 2.7 | 2.6 | 3.0 | 2.8 | 2.7 | 2.5 |

Each of those over the tree-walker's time. Under 1, that run took less time than the tree-walker's:

| program | `cig run` (default), first run | `cig run` (default), second run | `cig run` (default) | `cig run --engine vm`, empty tin | `cig run --engine vm` |
|---|---:|---:|---:|---:|---:|
| `lines-500` | 0.97 | 1.10 | 0.84 | 2.28 | 0.88 |
| `orchestrate` | 1.01 | 1.01 | 0.35 | 9.97 | 0.34 |
| `hello` | 0.96 | 1.13 | 1.04 | 1.03 | 0.93 |

## How it was measured

As [the benchmark programs' run](ENGINES-2026-11-25.md#how-it-was-measured): whole processes, every output checked, rounds in a random order, a home for each side, the same cig settings. Failed pairs: 0 in pass 1, 0 in pass 2. What differs, because these scripts run for a few milliseconds and two of the runs leave a process behind:

- **Two CPUs for each measured process** (`--cpus 2,3`, two performance cores), so a `cig __lower` left behind has one to run on, as it has on any machine with more than one. The figures given for one CPU are from two more passes of the same scripts with `--cpus 2` ([`start-one-cpu/`](../bench/results/2026-11-25-engines/start-one-cpu/)).
- **Each timed run starts on a busy core.** The harness keeps the first of the measured CPUs busy for 5 ms and starts the run from it (`--warm 5`). A run of two milliseconds started on an idle core is timed as much by how far that core's clock had fallen as by the program. There is no rest between runs; each round began once the CPU package was below 70 °C.
- **A first run** (`cig-auto-cold`): the tin's modules folder, which holds the marks too, is removed before every timed run, so each is the first run of a script the tin has not seen.
- **A second run** (`cig-auto-second`): before every timed run the folder is put back as the script's first run left it, from a copy the harness keeps, so each timed run is a script's second and no run of cig comes just before it. Starting `cig __lower` is inside that run; the lowering goes on after `cig` has exited and is not timed.
- **Later runs** (`cig-auto`): the script is run twice before the rounds and its lowering waited for, so every timed run finds the module.
- **Nothing left running.** After each run on the default the harness waits until no `cig __lower` is running before the next pair starts. That wait is not timed.
- **The build before** ([`before/`](../bench/results/2026-11-25-engines/before/) and [`before-one-cpu/`](../bench/results/2026-11-25-engines/before-one-cpu/)): the same passes with cig built from `main` at `da5cd6f`, where `cig-auto-cold` is a first run that starts the lowering.

## Conditions

|  | pass 1 | pass 2 |
|---|---|---|
| started (UTC) | 2026-11-25T17:06:42Z | 2026-11-25T17:08:42Z |
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
| load average at the start | 0.47 0.79 0.97 | 1.06 0.95 1.01 |
| package °C at the start | 84 | 90 |
| cig's run records at the start | 0 | 0 |
| harness | Python 3.12.3 | Python 3.12.3 |
| rounds | 101 | 101 |
| seed of the rounds' order | 1 | 2 |
| pinned to CPUs | 2-3 | 2-3 |
| `true`, median ms | 0.965 | 1.034 |
| CPU kept busy before each run, ms | 5 | 5 |
| a round waits for the package to be below, °C | 70 | 70 |
| rounds begun without it | 0 | 0 |
| waits for a left-behind lowering that gave up | 0 | 0 |
| cig's run records at the last round | 306 | 306 |
| mean clock at a round's start, lowest, MHz | 400 | 400 |
| mean clock at a round's start, highest, MHz | 3220 | 3237 |
| package °C at a round's start, highest | 68 | 68 |

## How steady the numbers are

| side | programs | pass 2 ÷ pass 1 | last third ÷ first third, pass 1 | last third ÷ first third, pass 2 | CV over 5%, pass 1 | CV over 5%, pass 2 |
|---|---:|---:|---:|---:|---:|---:|
| `cig run --engine tree` | 3 | 1.02 | 1.00 | 1.01 | 2 | 2 |
| `cig run --engine vm` | 3 | 1.00 | 0.92 | 0.97 | 3 | 3 |
| `cig run --engine vm`, empty tin | 3 | 0.99 | 1.02 | 1.00 | 2 | 2 |
| `cig run` (default) | 3 | 0.98 | 0.97 | 0.95 | 3 | 3 |
| `cig run` (default), first run | 3 | 1.01 | 0.97 | 1.01 | 2 | 2 |
| `cig run` (default), second run | 3 | 1.01 | 1.03 | 0.95 | 2 | 2 |

## What this does not show

- **Linux only.** A second run's cost is the cost of starting a process, which differs on macOS and Windows. Nothing here was measured there.
- **A development machine**, not the benchmark paper's test machine: read the ratios; the times belong to this machine, and to a core whose clock is up. Started from an idle core, the same runs take longer on every side.
- **Short runs.** On a run of two milliseconds a ratio within a few hundredths of 1 is level here, whichever side of 1 it falls.
