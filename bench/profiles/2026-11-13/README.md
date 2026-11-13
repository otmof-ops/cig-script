# VM call-graph profiles, 2026-11-13

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

Where the bytecode VM spends its time on four programs of `bench/compute`, before and after the first round of
tuning (#230, cig-compile#31 step 9). Each program has a folded-stack file (`<program>-<when>.folded`, one line per
distinct stack with its sample count) and a flame graph made from it (`<program>-<when>.svg`).

- `before`: the VM as it was at the start of the round (this repository at c12033f).
- `after`: the VM after this round's first seven steps (the boxed error, the loop per frame, the one-copy call,
  the fast path for the hot instructions, comparisons, field caches, calls staying in the loop).

## What ran

Each program was lowered by cigcompile at `-O1`, verified, loaded once and then run on the VM in process, in a loop
(fib 16 times, richards 16, fannkuch once, loop 15), by cig-compile's `tests/vm_speed.rs` harness in loop mode
(`CIG_VM_SPEED_ONLY=<program>:vm CIG_VM_SPEED_LOOPS=<n>`). Parsing and lowering are part of the process, so their
frames appear too, small and to one side of the VM's.

## Conditions

The same laptop as the 2026-11-13 baseline (`docs/BASELINE-2026-11-13.md`): Intel Core i5-8265U, `powersave`
governor, on mains power, one job at a time under the test machine's lock. Release profile (`lto = true`,
`codegen-units = 1`) with line tables kept (`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`,
`CARGO_PROFILE_RELEASE_STRIP=none`), which does not change the generated code.

## Making them again

```sh
cargo --config "patch.'https://github.com/otmof-ops/cig-script'.cigscript.path='<this checkout>'" \
  test --release --features vm --test vm_speed --no-run          # in a cig-compile checkout
CIG_VM_SPEED_ONLY=fib:vm CIG_VM_SPEED_LOOPS=16 CIGSCRIPT_SRC=<this checkout> \
  perf record -e cycles:u -F 499 --call-graph dwarf,16384 --user-callchains -o fib.data \
  <the vm_speed test binary> --ignored --test-threads=1 --exact the_vm_against_the_tree_walker
perf script -i fib.data | inferno-collapse-perf > fib.folded
inferno-flamegraph < fib.folded > fib.svg
```

`--user-callchains` is what lets a user without `CAP_PERFMON` record call graphs when `perf_event_paranoid` is 2.
Before they were added, every file was searched for home directories, user names and host names and none was
found; the copy step rewrites any such path to `<home>` or `<src>` first.

## The 25 programs after each step

The whole of `bench/compute` after each step, the VM against the tree-walker in one process (the same harness,
one warm-up and five samples a side, interleaved, alone on the machine). A ratio above 1 means the VM is the
faster of the two. The steps are cumulative and are the commits of this round, in order: 1 to 4 are the boxed
error, the loop per frame with a one-add step charge and a one-copy call, the fast path for the hot instructions,
and the counter (which changes nothing when it is off); 5 is comparisons in the fast path; 6 the field caches;
7 calls and returns staying in the loop; 8 an index's positions from trailing `nop`s; 9 the fast path in a
function of its own, off the stack of a re-entered call.

| program | before | steps 1 to 4 | 5 | 6 | 7 | 8 | 9 | tree-walker ms | VM ms before | VM ms after |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| closures | 0.35x | 0.70x | 0.68x | 0.67x | 0.62x | 0.63x | 0.67x | 34.51 | 93.09 | 51.79 |
| csv | 0.65x | 0.86x | 0.87x | 0.89x | 0.88x | 0.88x | 0.86x | 30.87 | 47.24 | 35.75 |
| deltablue | 0.13x | 0.41x | 0.61x | 0.60x | 0.56x | 0.57x | 0.52x | 3.78 | 30.70 | 7.25 |
| dispatch | 0.19x | 0.47x | 0.59x | 0.59x | 0.57x | 0.57x | 0.54x | 54.54 | 283.97 | 101.71 |
| fannkuch | 0.14x | 0.35x | 0.49x | 0.47x | 0.44x | 0.46x | 0.44x | 297.74 | 1870.45 | 677.25 |
| fib | 0.32x | 0.77x | 0.78x | 0.76x | 0.75x | 0.71x | 0.73x | 48.25 | 121.22 | 66.46 |
| json | 0.83x | 0.96x | 0.95x | 0.95x | 0.95x | 0.93x | 0.92x | 70.83 | 83.25 | 76.96 |
| json-deep | 0.60x | 0.92x | 0.93x | 0.92x | 0.93x | 0.90x | 0.91x | 2.55 | 3.98 | 2.82 |
| lists | 0.42x | 0.74x | 0.72x | 0.72x | 0.71x | 0.70x | 0.69x | 26.57 | 58.61 | 38.74 |
| loop | 0.32x | 1.40x | 1.41x | 1.37x | 1.38x | 1.37x | 1.40x | 51.88 | 138.71 | 37.17 |
| mandel | 0.31x | 1.18x | 1.32x | 1.29x | 1.38x | 1.15x | 1.32x | 351.46 | 997.45 | 265.49 |
| maps | 0.38x | 0.61x | 0.65x | 0.68x | 0.66x | 0.65x | 0.63x | 39.05 | 90.28 | 62.17 |
| nbody | 0.40x | 0.94x | 1.03x | 1.62x | 1.52x | 1.56x | 1.58x | 478.11 | 1008.80 | 302.14 |
| regex-ish | 0.52x | 0.74x | 0.74x | 0.75x | 0.75x | 0.76x | 0.72x | 39.80 | 64.95 | 55.43 |
| richards | 0.13x | 0.25x | 0.36x | 0.40x | 0.38x | 0.40x | 0.37x | 17.35 | 124.85 | 47.07 |
| sieve | 0.25x | 0.65x | 0.81x | 0.79x | 0.75x | 0.78x | 0.75x | 187.10 | 636.83 | 249.62 |
| spectral | 0.23x | 0.57x | 0.72x | 0.69x | 0.64x | 0.65x | 0.63x | 188.03 | 713.81 | 300.00 |
| strbuild | 0.83x | 1.14x | 1.13x | 1.12x | 1.11x | 1.11x | 1.19x | 5.78 | 5.38 | 4.87 |
| strings | 0.56x | 0.74x | 0.78x | 0.77x | 0.78x | 0.77x | 0.75x | 24.39 | 39.09 | 32.61 |
| trees | 0.21x | 0.41x | 0.53x | 0.52x | 0.49x | 0.48x | 0.49x | 362.05 | 1512.95 | 745.32 |
| words | 0.35x | 0.60x | 0.63x | 0.68x | 0.65x | 0.64x | 0.64x | 41.95 | 120.91 | 66.02 |
| **median of all 25** | **0.38x** | **0.74x** | **0.73x** | **0.76x** | **0.75x** | **0.76x** | **0.69x** | | | |
| geometric mean of all 25 | 0.37x | 0.69x | 0.76x | 0.78x | 0.76x | 0.75x | 0.73x | | | |

The four start-up programs (`hello`, `startup-0`, `startup-1`, `startup-10`) run for microseconds in process and
are in the median and the mean but not in the rows. Times moved by 5 to 16% between sessions on this machine in
both engines alike, and the median by about 0.03 between two runs of one tree, so a step is judged by the
instructions the processor ran, which do not move. Every step lowered them on the programs it touches, with two
costs recorded to recover: step 6 raised them by 4 to 8% on programs that read no field (the fast path grew), and
step 9, which is there so that a builtin calling back into the VM costs less stack than the tree-walker's (3,776
bytes a level in a release build against 4,912; 5,392 before it), lowered them by 6% where whole loops stay on the
fast path and raised them by 1 to 6% on programs that call a lot. The gate this work answers to is three times
the tree-walker at the median (cig-compile#31): it is not met. What the profiles say is in the way next is
written in #230.

## What they show

- `fib`, before: 18.6% of samples are `memmove` and another ~25% are `Result` branching and copying
  (`branch<Value, Diagnostic>`, `branch<Flow, Diagnostic>`, `copied<Span>`): the 150-byte error type moved through
  every instruction. After: the fast path (`fast`, 25.6%) and the frame loop lead; `memmove` is 3.8%.
