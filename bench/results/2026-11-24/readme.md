### Stage c: native code for loops (on `main`)

[cigcompile](https://github.com/otmof-ops/cig-compile) can build a script with native code for its loops and for its functions on numbers (`cigcompile build --stage c`). The native code runs on the same registers as cig's VM, so it sits beside anything a script does: what it has no machine code for, the interpreter runs, and a loop is compiled only where that comes out ahead. It is on `main` and in no release yet, so this is a run of the source, on another machine than the table above ([the paper](docs/BENCHMARKS-2026-11-24.md)): the same 25 programs, every side in the same rounds, pinned to one core.

| program | stage b, ms | stage c, ms | stage c ÷ stage b | Lua 5.4, ms | CPython 3.12, ms |
|---|---:|---:|---:|---:|---:|
| `mandel` | 40.2 | 7.4 | 0.18 | 16.2 | 53.0 |
| `fib` | 16.8 | 4.4 | 0.26 | 4.4 | 12.6 |
| `fannkuch` | 40.7 | 12.3 | 0.30 | 22.4 | 33.2 |
| `loop` | 9.3 | 4.6 | 0.50 | 4.3 | 21.7 |
| `sieve` | 32.8 | 25.5 | 0.78 | 11.1 | 25.2 |

- **Where a program has native code it takes 0.52 of its stage b time** (8 of the 21 compute programs have some; the five above gain most), and 0.78 over all 21: faster on 7, level on 14, slower on 0. A program with nothing worth compiling is its stage b program.
- **Against the other languages**, a stage c program takes 0.66 of CPython 3.12's time over the compute programs (stage b: 0.84), 1.37 times Lua 5.4's (stage b: 1.83) and 2.54 times LuaJIT's (stage b: 3.40).
- **What is not compiled yet**: a loop that calls a function, reads a map's field or builds a string each turn. Those run as stage b does.

Whole processes, start-up included (4.0 ms for `hello`), on a 2025 laptop on mains power.
