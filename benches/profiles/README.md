<!--
SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
SPDX-License-Identifier: CC-BY-4.0
-->

# Evaluator profiles (#146)

What each step of the faster tree-walker measured, so the next one has a
before. Every figure is the minimum of five interleaved rounds (the old
and the new binary alternate, each with CPython on the same program), a
release build with the repository's own profile, a scratch home, on a
shared machine whose load average stayed between 3.5 and 7 during the
final rounds. Times include process start (about 3.5 ms for cig, 14 ms
for CPython). `bench/compute/*.cig` against cig-compile's `bench/ref/*.py`.

No flame graphs are committed: the machine that took these has no working
`perf` (the distribution stub for another kernel, `perf_event_paranoid=4`)
and `ptrace_scope=1`, so there is no sampler. In their place, the costs
were taken from three counts that do not depend on a clock:

- allocations per call, with the counting allocator in
  `src/interp/resolve_tests.rs` (`no_allocation_per_call_in_steady_state`):
  9 before #146, 3 after the scope pool, 0 now (a call, a loop turn, and
  `fib` recursion);
- the stack frame of the two recursive functions, read from the binary
  (`sub $N,%rsp` in the prologue of `Interp::eval` and `Interp::exec`):
  `exec` 0x5a8 -> 0x1d8 bytes, `eval` 0x4a8 -> 0x4b8;
- per-construct cost from one-line loops of 2,000,000 turns (S1, ns per
  turn, load 14): empty `for` ~60; `x = x + 1` ~90 per statement (235
  before); an empty call ~83; `if x < 5 { }` ~150.

## Steps

| step | what | kept |
|---|---|---|
| S1 | free list of whole scopes (calls, loop turns, blocks that declare); arguments evaluated into the callee's frame; no scope for a block that declares nothing; `fast_binary` on Int/Int and Float/Float; per-`Member` field hint | yes |
| S2 | `exec` split into a hot function and `exec_rare`; leaves (numbers, resolved names) answered without entering `eval`; slow paths `#[inline(never)]`; `tick` inline with its error out of line | yes |
| S3 | `eval_cold` and `assign_place` out of `eval` and `assign` | no: loop +0.7 ms, fannkuch +38 ms (min), wins only on fib and nbody |

glibc, ms, minimum of 5 (loop, fib, nbody, fannkuch), load 5 to 14:

| build | loop | fib | nbody | fannkuch |
|---|---|---|---|---|
| origin/main bd3a135 | 154 | 138 | 889 | 535 |
| S1 | 136 | 124 | 702 | 526 |
| S2 | 115 | 115 | 577 | 378 |

The final table, base against S2 over the whole set with CPython beside it,
is in the changelog entry and in the pull request.

## What is not done

- Keys are not interned: the `Map` type is still `IndexMap<String, Value>`.
  The field hint keeps the answer right under removal (the key at the hinted
  position is compared before it is believed) and replaces the hash with one
  short compare, which is the part of T4 that did not need every stdlib
  function changed.
- Blocks that declare names still get a scope (from the free list); only
  blocks that declare nothing share their parent's.
- List borrows are per access, not per statement.
