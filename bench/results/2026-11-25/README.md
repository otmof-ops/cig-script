# The benchmark run of 2026-11-25

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The raw data behind [`docs/BENCHMARKS-2026-11-25.md`](../../../docs/BENCHMARKS-2026-11-25.md) and the README's "Stage c: native code for loops, lists and records": the run after the VM's calls, its four-byte instructions and its start-up were reworked and stage `c` learned lists and records. cig is built from `main` at commit `ed7ad3a` as its release workflow builds it for Linux (`cargo build --release --target x86_64-unknown-linux-musl`), cigcompile from its `main` at commit `b0a287b`; no release carries any of it yet. The machine, the core, the harness and the other languages' builds are those of [the run of 2026-11-24](../2026-11-24/README.md), so the paper sets the two runs side by side.

| file | what it holds |
|---|---|
| `pass1/`, `pass2/` | the two passes of [`bench/compare/compare.py`](../../compare/README.md), 21 rounds each: every timed run (`samples.tsv`), every check (`checks.tsv`), three readings of each pair's peak memory (`memory.tsv`), each round's start, clock, temperature and cig's run history (`rounds.tsv`), the sha256 of every program, expected output, reference and built program (`inputs.tsv`), what of each program got native code (`native.tsv`) and the machine's conditions (`conditions.txt`) |
| `numbers.json` | every number, as `report.py` made it |
| `alone/` | `json-deep` and `deltablue` timed with nothing else, 201 rounds under each of two other seeds (`pass1/`, `pass2/`, `numbers.json`): what two copies of one three-millisecond program differ by when the run is theirs alone |
| `paper.md.in` | the paper's template |
| `readme.md.in`, `readme.md` | the README section's template, and the section as rendered: the README holds it word for word |

Every number in the paper and the README section is made from `pass1/` and `pass2/`, and those quoted from the run of 2026-11-24 from that run's `numbers.json`; all of it is checked by a second program written from the specification alone:

```sh
python3 bench/compare/report.py --pass bench/results/2026-11-25/pass1 --pass bench/results/2026-11-25/pass2 \
  --numbers bench/results/2026-11-25/numbers.json \
  --earlier 2026-11-24 bench/results/2026-11-24/numbers.json \
  --earlier alone bench/results/2026-11-25/alone/numbers.json \
  --render bench/results/2026-11-25/paper.md.in docs/BENCHMARKS-2026-11-25.md \
  --render bench/results/2026-11-25/readme.md.in bench/results/2026-11-25/readme.md
python3 bench/compare/check.py --pass bench/results/2026-11-25/pass1 --pass bench/results/2026-11-25/pass2 \
  --numbers bench/results/2026-11-25/numbers.json \
  --earlier 2026-11-24 bench/results/2026-11-24/numbers.json \
  --earlier alone bench/results/2026-11-25/alone/numbers.json \
  --render bench/results/2026-11-25/paper.md.in docs/BENCHMARKS-2026-11-25.md \
  --render bench/results/2026-11-25/readme.md.in bench/results/2026-11-25/readme.md \
  --contains README.md bench/results/2026-11-25/readme.md
```

The passes were made with:

```sh
python3 bench/compare/compare.py --out pass1 --seed 1 --rounds 21 \
  --cig bin/cig --cigcompile bin/cigcompile --runtime bin/cig \
  --lua bin/lua --luajit bin/luajit --python python3 --node node --rustc bin/rustc --cpus 2
```

and `--out pass2 --seed 2` for the second, with one cig home for both, so cig's run history grows through the run as it did on the 24th. `alone/` was made with `--only json-deep,deltablue --sides stage-b,stage-c,stage-c-off --rounds 201`, `--seed 7` and `--seed 8`, and its numbers with `report.py --pass alone/pass1 --pass alone/pass2 --numbers alone/numbers.json` (and checked with `check.py` the same way).
