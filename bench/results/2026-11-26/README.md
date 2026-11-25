# The benchmark run of 2026-11-26

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The raw data behind [`docs/BENCHMARKS-2026-11-26.md`](../../../docs/BENCHMARKS-2026-11-26.md) and the README's "Stage c: native code for loops, lists and records": the run after `cig run` took the VM by default, the VM's fast loop counted a loop turn's steps in one instruction and ran `for x in xs` itself, and stage `c` freed a loop's scratch registers once. It began at 20:02 UTC on 2026-11-25, fifteen hours after the run of that date, and is named for the day it was where it was made. cig is built from `main` at commit `ca5236b` and cigcompile from its `main` at commit `6882f02`, each as its release workflow builds it for Linux (`cargo build --release --target x86_64-unknown-linux-musl`); no release carries any of it yet. cigcompile at that commit lowers with cig-script at `da5cd6f`, and between that commit and `ca5236b` the lowering, the bytecode and the VM differ by nothing that runs. The machine, the core, the programs and the other languages' builds are those of [the run of 2026-11-25](../2026-11-25/README.md), so the paper sets the two runs side by side.

| file | what it holds |
|---|---|
| `pass1/`, `pass2/` | the two passes of [`bench/compare/compare.py`](../../compare/README.md), 21 rounds each: every timed run (`samples.tsv`), every check (`checks.tsv`), three readings of each pair's peak memory (`memory.tsv`), each round's start, clock, temperature and cig's run history (`rounds.tsv`), the sha256 of every program, expected output, reference and built program (`inputs.tsv`), what of each program got native code (`native.tsv`) and the machine's conditions (`conditions.txt`) |
| `numbers.json` | every number, as `report.py` made it |
| `alone/` | `maps`, `strings`, `words`, `json-deep` and `deltablue` as stage `b` and stage `c` programs, and the stage `c` ones without their native code, timed with nothing else for 201 rounds under each of two other seeds (`pass1/`, `pass2/`, `numbers.json`): whether the three that came out slower with native code are (two are; one is at the line), and what two copies of one three-millisecond program differ by when the run is theirs alone |
| `paper.md.in` | the paper's template |
| `readme.md.in`, `readme.md` | the README section's template, and the section as rendered: the README holds it word for word |

Every number in the paper and the README section is made from `pass1/` and `pass2/`, and those quoted from the run of 2026-11-25 from that run's `numbers.json`; all of it is checked by a second program written from the specification alone:

```sh
python3 bench/compare/report.py --pass bench/results/2026-11-26/pass1 --pass bench/results/2026-11-26/pass2 \
  --numbers bench/results/2026-11-26/numbers.json \
  --earlier 2026-11-25 bench/results/2026-11-25/numbers.json \
  --earlier alone bench/results/2026-11-26/alone/numbers.json \
  --render bench/results/2026-11-26/paper.md.in docs/BENCHMARKS-2026-11-26.md \
  --render bench/results/2026-11-26/readme.md.in bench/results/2026-11-26/readme.md
python3 bench/compare/check.py --pass bench/results/2026-11-26/pass1 --pass bench/results/2026-11-26/pass2 \
  --numbers bench/results/2026-11-26/numbers.json \
  --earlier 2026-11-25 bench/results/2026-11-25/numbers.json \
  --earlier alone bench/results/2026-11-26/alone/numbers.json \
  --render bench/results/2026-11-26/paper.md.in docs/BENCHMARKS-2026-11-26.md \
  --render bench/results/2026-11-26/readme.md.in bench/results/2026-11-26/readme.md \
  --contains README.md bench/results/2026-11-26/readme.md
```

The passes were made with:

```sh
python3 bench/compare/compare.py --out pass1 --seed 1 --rounds 21 \
  --cig bin/cig --cigcompile bin/cigcompile --runtime bin/cig \
  --lua bin/lua --luajit bin/luajit --python python3 --node node --rustc bin/rustc --cpus 2 \
  --sides cig,cig-dry,cig-first,cig-tree,cig-vm,cig-vm-cold,stage-a,stage-a-dry,stage-b,stage-b-dry,stage-c,stage-c-dry,stage-c-off,lua,luajit-off,luajit,python,python-idiom,node,node-idiom,rust,rust-idiom
```

and `--out pass2 --seed 2` for the second, with one cig home for both, so cig's run history grows through the run as it did in the runs before. The sides are the 22 of the run of 2026-11-25: `cig run` is now the default engine, which the harness runs twice before the rounds so that every timed run finds its module, and the sides for a script's first and second run under the default are left to [the engine run](../2026-11-25-engines/README.md). `alone/` was made with `--only json-deep,deltablue,maps,strings,words --sides stage-b,stage-c,stage-c-off --rounds 201`, `--seed 7` and `--seed 8`, and its numbers with `report.py --pass alone/pass1 --pass alone/pass2 --numbers alone/numbers.json` (and checked with `check.py` the same way).
