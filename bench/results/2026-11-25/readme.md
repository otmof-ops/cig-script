### Stage c: native code for loops, lists and records (on `main`)

[cigcompile](https://github.com/otmof-ops/cig-compile) can build a script with native code for its loops and for its functions on numbers (`cigcompile build --stage c`). The native code runs on the same registers as cig's VM, so it sits beside anything a script does: what it has no machine code for, the runtime runs, and a loop is compiled only where that comes out ahead. It is on `main` and in no release yet, so this is a run of the source, on another machine than the table above ([the paper](docs/BENCHMARKS-2026-11-25.md)): the same 25 programs, every side in the same rounds, pinned to one core.

| program | stage b, ms | stage c, ms | stage c ÷ stage b | Lua 5.4, ms | CPython 3.12, ms |
|---|---:|---:|---:|---:|---:|
| `mandel` | 26.5 | 6.3 | 0.24 | 16.0 | 52.6 |
| `fib` | 9.7 | 2.9 | 0.30 | 4.3 | 12.4 |
| `fannkuch` | 30.5 | 9.8 | 0.32 | 22.4 | 33.1 |
| `nbody` | 61.0 | 34.0 | 0.56 | 28.9 | 82.9 |
| `sieve` | 26.6 | 14.2 | 0.53 | 11.2 | 25.0 |
| `loop` | 5.7 | 3.0 | 0.54 | 4.3 | 21.7 |
| `richards` | 5.5 | 4.8 | 0.88 | 2.7 | 10.6 |

- **A stage c program takes 0.98 times Lua 5.4's time** at the geometric mean of the compute programs both have, 0.48 of CPython 3.12's and 1.84 times LuaJIT's. A day earlier, on the same machine and the same core, it took 1.37, 0.66 and 2.54. A stage b program takes 1.36 times Lua's time (1.83) and 0.63 of CPython's (0.84): the VM itself got faster.
- **What is compiled**: a loop of arithmetic, a list's elements read and written, a loop over a list and `push`, a record's fields read and written where they lie (`nbody`, `richards`), a function on numbers alone (`fib`). 14 of the 21 compute programs have native code; over all 21 a stage c program takes 0.76 of its stage b time: faster on 9, level on 11, slower on 1 (`json-deep`, three milliseconds long, which has no native code: its two programs are the same file).
- **What is not faster yet**: a loop whose work is calls, strings or dictionary lookups. Those run as stage b does.

Whole processes, start-up included (2.2 ms for `hello`, where CPython takes 8.2), on a 2025 laptop on mains power.
