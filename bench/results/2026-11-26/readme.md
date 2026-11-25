### Stage c: native code for loops, lists and records (on `main`)

[cigcompile](https://github.com/otmof-ops/cig-compile) can build a script with native code for its loops and for its functions on numbers (`cigcompile build --stage c`). The native code runs on the same registers as cig's VM, so it sits beside anything a script does: what it has no machine code for, the runtime runs, and a loop is compiled only where the compiler counts it as coming out ahead. It is on `main` and in no release yet, so this is a run of the source, on the 2025 laptop of the engine run above and not the 2018 one of this section's first table ([the paper](docs/BENCHMARKS-2026-11-26.md)): the same 25 programs, every side in the same rounds, pinned to one core.

| program | stage b, ms | stage c, ms | stage c ÷ stage b | Lua 5.4, ms | CPython 3.12, ms |
|---|---:|---:|---:|---:|---:|
| `mandel` | 23.9 | 4.3 | 0.18 | 15.6 | 52.3 |
| `fib` | 9.3 | 2.7 | 0.29 | 4.2 | 12.4 |
| `fannkuch` | 28.7 | 9.3 | 0.32 | 22.2 | 32.8 |
| `loop` | 4.9 | 2.6 | 0.52 | 4.1 | 21.2 |
| `nbody` | 56.5 | 32.8 | 0.58 | 28.2 | 82.3 |
| `sieve` | 20.8 | 14.4 | 0.69 | 11.2 | 24.8 |
| `richards` | 5.2 | 4.4 | 0.86 | 2.4 | 10.6 |

- **A stage c program takes 0.94 times Lua 5.4's time** at the geometric mean of the compute programs both have, 0.45 of CPython 3.12's and 1.79 times LuaJIT's. In the run before, fifteen hours earlier on the same machine and the same core, it took 0.98, 0.48 and 1.84. A stage b program takes 1.31 times Lua's time (1.36) and 0.60 of CPython's (0.63): the VM's loop counts a turn's steps in one instruction and runs a loop over a list itself.
- **What is compiled**: a loop of arithmetic, a list's elements read and written, a loop over a list and `push`, a record's fields read and written where they lie (`nbody`, `richards`), a function on numbers alone (`fib`). 14 of the 21 compute programs have native code; over all 21 a stage c program takes 0.76 of its stage b time: faster on 9, level on 9, slower on 3.
- **What comes out slower**: `maps`, `strings` and `words` (1.04, 1.03, 1.05), which were level in the run before. Timed alone, `maps` and `words` are 5% behind their bytecode and `strings` is at the 3% line. Their loops are strings and dictionary lookups, which the runtime does for the native code one call at a time; what moved them is being separated ([cig-compile#117](https://github.com/otmof-ops/cig-compile/issues/117)).
- **What is not faster yet**: a loop whose work is calls. It runs as stage b does.

Whole processes, start-up included (2.0 ms for `hello`, where CPython takes 7.7), on a 2025 laptop on mains power.
