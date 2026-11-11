<!--
SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
SPDX-License-Identifier: CC-BY-4.0
-->

# Evaluator profiles (#146)

`perf` on this machine started to work after the earlier round (the stub
for another kernel is gone, replaced by `linux-hwe-6.17-tools`), so the
flame graphs the issue asks for are now here. One `.svg` and one
`.folded` per benchmark per side, read as the left column before the
tree-walker work and the right column after.

- `loop-before.svg`, `loop-after.svg`
- `fib-before.svg`, `fib-after.svg`
- `nbody-before.svg`, `nbody-after.svg`
- `fannkuch-before.svg`, `fannkuch-after.svg`

Each `.folded` is the collapsed-stack text the flame graph was drawn
from (frames joined by `;`, one sample count per line); open the `.svg`
in a browser to zoom and search.

## How they were made

- Binaries: `cig 1.20.0`, built with the repository's release profile
  (`opt-level=3`, full LTO, `codegen-units=1`) plus `RUSTFLAGS="-C
  force-frame-pointers=yes"` so every stack is whole. Compiler: rustc
  1.98.1, glibc host (`x86_64-unknown-linux-gnu`). Two trees: the
  `before` side was built from `08cf594` (origin/main, without the
  walker); the `after` side was built from the walker tree (PR #202).
- Sampler: `perf record -F 999 -g --call-graph fp` from
  `linux-hwe-6.17-tools` (perf 6.17.13 on kernel 6.17.0-1033-oem), run
  as root. `perf_event_paranoid` stays at 4 and `ptrace_scope` at 1;
  root does not need either lowered.
- Each run is one invocation of `cig run bench/compute/<name>.cig`
  under a scratch `HOME`, `CIGSCRIPT_HOME`, `CIG_KEYS_DIR` and the
  three `XDG_*` roots (passed through `sudo` with `env -i`) so `cig`
  never touches the real `~/.cigscript`. The program's stdout was
  compared against its `.expect` before the capture was kept.
- Host is an Intel Core Ultra 7 255H (hybrid P and E cores); perf
  records `cpu_core/cycles/P` and `cpu_atom/cycles/P` as two streams.
  Both are kept: before the collapse pass the event name is rewritten
  to `cycles` so both streams fold into one graph.
- Folding and drawing: `inferno 0.12.8` (`inferno-collapse-perf`
  piped into `inferno-flamegraph`).
- The machine is shared; load average during the captures was between
  three and seven. Sample counts are what one run of each program
  gives (the programs are short enough that one capture is one run,
  so the counts move with each benchmark's wall time): 124 and 109
  (loop), 127 and 71 (fib), 881 and 572 (nbody), 475 and 396
  (fannkuch), before and after.

## Three heaviest self-time frames per program

Read from the `.folded` files (sample count = exclusive, i.e. the leaf
of the stack). The `[unknown]` frames at kernel addresses and inside
the dynamic loader are dropped. Each frame is prefixed by its self
sample count; the total for that side is the denominator.

### loop (154 to 115 ms, min of five; S1+S2 in the walker)

| rank | before (124 samples) | after (109 samples) |
| --- | --- | --- |
| 1 | `Interp::eval` (29) | `Interp::eval` (34) |
| 2 | `Interp::exec` (24) | `Interp::assign` (27) |
| 3 | `Scope::child` (11) | `Interp::binary` (7) |

`Scope::child` leaves the top three on the after side: the free list
of whole scopes (S1) is what removes it. `Interp::assign` shows up
new because the walker's split lifted the per-statement assignment
out of `exec` into its own hot function.

### fib (138 to 115 ms)

| rank | before (127 samples) | after (71 samples) |
| --- | --- | --- |
| 1 | `Interp::eval` (42) | `Interp::eval` (25) |
| 2 | `Interp::exec` (18) | `Interp::exec` (13) |
| 3 | `Interp::call` (8) | `Scope::get_at` (8) |

`Interp::call` drops out of the top three on the after side: arguments
are evaluated straight into the callee frame (no `Vec`), and the call
frame comes off the scope free list. What is left on top is the two
recursive functions and the name resolve.

### nbody (889 to 577 ms)

| rank | before (881 samples) | after (572 samples) |
| --- | --- | --- |
| 1 | `Interp::eval` (325) | `Interp::eval` (269) |
| 2 | `Scope::get_at` (73) | `Scope::get_at` (65) |
| 3 | `Interp::exec` (66) | `Interp::assign` (48) |

Same before and after: `eval` dominates, name resolution is next,
`exec` or the walker's `assign` third. The field-hint cache keeps
`IndexMap::get_index_of::<str>` out of the after side's top three
(it was fourth before, 45 samples); `__memcmp_avx2_movbe` still
appears below the cut (47 samples) because the field hint is verified
with a short key compare.

### fannkuch (535 to 378 ms)

| rank | before (475 samples) | after (396 samples) |
| --- | --- | --- |
| 1 | `Interp::eval` (114) | `Interp::assign` (92) |
| 2 | `Interp::exec` (111) | `Interp::eval` (84) |
| 3 | `Scope::get_at` (56) | `Scope::get_at` (53) |

The list-swap inner loop puts the walker's new `assign` on top.
`exec` drops from 111 to 37 samples (not in the top three on the
after side) because leaves answer without entering `eval`, the slow
paths are out of line, and the frame prologue is small enough that
the recursion is cheap.

## Reading these

Four things stand out across the four graphs:

1. `Interp::eval` is the single hottest frame on every side. The
   evaluator's recursive shape and the `Result<Value, Diagnostic>`
   return (a `Diagnostic` is about 130 bytes, so every eval frame is
   that wide) is where the time goes, more than scopes or hashing.
2. `Interp::assign` is new on the after side and often takes the
   second slot (loop, nbody, fannkuch). The walker's `exec`/`eval`
   split put per-statement assignment on its own hot arm.
3. `Scope::child` is gone from the after side's top three. The free
   list of whole scopes removed it: a call, a loop turn and a
   declaring block all get a cached scope.
4. `Scope::get_at` stays in the top three on both sides for the heavy
   programs (nbody, fannkuch) because the field hint cuts hashing but
   not the chain of parent pointers. Name resolution is what the next
   step pays for.

## Reproducing

```
# "before"
git -C <clone> archive 08cf594 | tar -x -C before/
RUSTFLAGS="-C force-frame-pointers=yes" CARGO_TARGET_DIR=before-target \
  cargo build --release --locked --manifest-path before/Cargo.toml

# "after"
RUSTFLAGS="-C force-frame-pointers=yes" CARGO_TARGET_DIR=after-target \
  cargo build --release --locked

# one capture (repeat for each name, each side)
sudo env HOME=<scratch>/h CIGSCRIPT_HOME=<scratch>/c CIG_KEYS_DIR=<scratch>/k \
  XDG_CONFIG_HOME=<scratch>/xc XDG_DATA_HOME=<scratch>/xd XDG_CACHE_HOME=<scratch>/xx \
  perf record -F 999 -g --call-graph fp -o <name>-<side>.data \
  -- <target>/release/cig run bench/compute/<name>.cig
sudo chown $(id -un):$(id -gn) <name>-<side>.data

# fold (combine the hybrid-CPU event streams first) and draw
sudo perf script -f -i <name>-<side>.data --no-inline \
  | sed -E 's|cpu_(atom|core)/cycles/P:|cycles:|g' \
  | inferno-collapse-perf > <name>-<side>.folded
inferno-flamegraph --title "<name> <side>" --countname samples \
  < <name>-<side>.folded > <name>-<side>.svg
```

The scratch roots can be thrown away between runs; the raw `perf.data`
file can be too once the `.folded` is written.
