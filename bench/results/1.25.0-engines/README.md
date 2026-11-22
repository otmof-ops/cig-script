# The engine runs on cig 1.25.0 as released

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The raw data behind [`docs/ENGINES-1.25.0.md`](../../../docs/ENGINES-1.25.0.md) and [`docs/ENGINES-FIRST-RUNS-1.25.0.md`](../../../docs/ENGINES-FIRST-RUNS-1.25.0.md): `cig run` on the tree-walker, on the VM with the script's module in the tin, and on the VM with nothing in the tin (cig-script#254). cig 1.25.0 as released (the static Linux x86_64 asset, held to its release's sha256), on a development machine, not the benchmark paper's test machine. [`../2026-11-22-engines/`](../2026-11-22-engines/) is the same measurement on a development build, from before the release.

| folder | what it holds |
|---|---|
| `main/pass1/`, `main/pass2/` | the 25 programs of `bench/compute` on the sides `cig-tree`, `cig-vm` and `cig-vm-cold`: the files [`bench/compare/compare.py`](../../compare/README.md) writes (`samples.tsv`, `checks.tsv`, `memory.tsv`, `rounds.tsv`, `inputs.tsv`, `conditions.txt`) |
| `start/pass1/`, `start/pass2/` | the same sides on three scripts: `bench/compute/hello.cig`, `bench/engines/lines-500.cig` and cigo's `orchestrate.cig` (cig-orchestrator at `0d6c934`, run with no arguments), given to the harness as its `--compute` folder |
| `main/numbers.json`, `start/numbers.json` | every number, as `report.py` made it |
| `main/engines.md.in`, `start/first-runs.md.in` | the templates of the two documents |
| `main/readme.md.in`, `start/readme.md.in`, and `readme.md` beside each | the templates of the two paragraphs the README quotes under "How fast it is", and each as rendered |

Every number in the two documents is made from the passes and checked by a second program written apart from the first:

```sh
for run in "main engines.md.in docs/ENGINES-1.25.0.md" "start first-runs.md.in docs/ENGINES-FIRST-RUNS-1.25.0.md"; do
  set -- $run
  d=bench/results/1.25.0-engines/$1
  python3 bench/compare/report.py --pass $d/pass1 --pass $d/pass2 --numbers $d/numbers.json --render $d/$2 $3
  python3 bench/compare/check.py --pass $d/pass1 --pass $d/pass2 --numbers $d/numbers.json --render $d/$2 $3 \
    --render $d/readme.md.in $d/readme.md --contains README.md $d/readme.md
done
```

How the passes were made, for each run (`--out` and the cig home are folders of the measurer's choosing; `start-src` holds the three scripts and their expected output):

```sh
python3 bench/compare/compare.py --out main/pass1 --seed 1 --rounds 31 --cig ./cig --sides cig-tree,cig-vm,cig-vm-cold
python3 bench/compare/compare.py --out start/pass1 --seed 1 --rounds 31 --cig ./cig --compute start-src --sides cig-tree,cig-vm,cig-vm-cold
```

Pass 2 is the same with `--seed 2`.
