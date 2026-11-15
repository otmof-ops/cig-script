# The benchmark run of 2026-11-15

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The raw data behind [`docs/BENCHMARKS-2026-11-15.md`](../../../docs/BENCHMARKS-2026-11-15.md) and the README's "How fast it is": run `w33-full` on the test machine, 16:21 to 16:44 UTC, cig 1.24.0 and cigcompile 0.5.0 as released.

| file | what it holds |
|---|---|
| `pass1/`, `pass2/` | the two passes of [`bench/compare/compare.py`](../../compare/README.md): every timed run (`samples.tsv`), every check (`checks.tsv`), three readings of each pair's peak memory (`memory.tsv`), each round's start, clock, temperature and cig's run history (`rounds.tsv`), the sha256 of every program, expected output, reference and built program (`inputs.tsv`), and the machine and the tools (`conditions.txt`) |
| `cig-bench.md`, `cig-bench.json` | `cig bench bench/bench.toml` on cig 1.24.0 as released, run between the two passes; in the JSON the measured binary's path is given as `target/release/cig`, where it was a path on the test machine |
| `numbers.json` | every number, as `report.py` made it |
| `paper.md.in`, `readme.md.in` | the templates of the paper and of the README's section |

Every number in the paper and the README section is made from `pass1/` and `pass2/` and checked by a second program written apart from the first:

```sh
python3 bench/compare/report.py --pass bench/results/2026-11-15/pass1 --pass bench/results/2026-11-15/pass2 \
  --numbers bench/results/2026-11-15/numbers.json \
  --render bench/results/2026-11-15/paper.md.in docs/BENCHMARKS-2026-11-15.md
python3 bench/compare/check.py --pass bench/results/2026-11-15/pass1 --pass bench/results/2026-11-15/pass2 \
  --numbers bench/results/2026-11-15/numbers.json \
  --render bench/results/2026-11-15/paper.md.in docs/BENCHMARKS-2026-11-15.md \
  --contains docs/BENCHMARKS-2026-11-15.md bench/results/2026-11-15/cig-bench.md
```
