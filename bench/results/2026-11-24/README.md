# The benchmark run of 2026-11-24

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The raw data behind [`docs/BENCHMARKS-2026-11-24.md`](../../../docs/BENCHMARKS-2026-11-24.md) and the README's "Stage c: native code for loops": the first run with stage `c` as a side. cig is built from `main` at commit `277e0e2` as its release workflow builds it for Linux (`cargo build --release --target x86_64-unknown-linux-musl`), cigcompile from its `main` at commit `be2a18e`; no release carries stage `c`'s native code for loops yet. One laptop, on mains power, 13:30 to 13:40 UTC, every measured process pinned to one performance core.

| file | what it holds |
|---|---|
| `pass1/`, `pass2/` | the two passes of [`bench/compare/compare.py`](../../compare/README.md), 21 rounds each: every timed run (`samples.tsv`), every check (`checks.tsv`), three readings of each pair's peak memory (`memory.tsv`), each round's start, clock, temperature and cig's run history (`rounds.tsv`), the sha256 of every program, expected output, reference and built program (`inputs.tsv`), what each program built at `--stage c` came out as and what of it has native code (`native.tsv`), and the machine, its settings, the CPUs the runs were pinned to and every tool's version and sha256 (`conditions.txt`) |
| `numbers.json` | every number, as `report.py` made it |
| `paper.md.in` | the paper's template |
| `readme.md.in`, `readme.md` | the README section's template, and the section as rendered: the README holds it word for word |

Every number in the paper and the README section is made from `pass1/` and `pass2/` and checked by a second program written from the specification alone:

```sh
python3 bench/compare/report.py --pass bench/results/2026-11-24/pass1 --pass bench/results/2026-11-24/pass2 \
  --numbers bench/results/2026-11-24/numbers.json \
  --render bench/results/2026-11-24/paper.md.in docs/BENCHMARKS-2026-11-24.md \
  --render bench/results/2026-11-24/readme.md.in bench/results/2026-11-24/readme.md
python3 bench/compare/check.py --pass bench/results/2026-11-24/pass1 --pass bench/results/2026-11-24/pass2 \
  --numbers bench/results/2026-11-24/numbers.json \
  --render bench/results/2026-11-24/paper.md.in docs/BENCHMARKS-2026-11-24.md \
  --render bench/results/2026-11-24/readme.md.in bench/results/2026-11-24/readme.md \
  --contains README.md bench/results/2026-11-24/readme.md
```

The passes were made with:

```sh
python3 bench/compare/compare.py --out pass1 --seed 1 --rounds 21 \
  --cig bin/cig --cigcompile bin/cigcompile --runtime bin/cig \
  --lua bin/lua --luajit bin/luajit --python python3 --node node --rustc bin/rustc --cpus 2
```

and `--out pass2 --seed 2` for the second, with one cig home for both, so cig's run history grows through the run as the paper says.
