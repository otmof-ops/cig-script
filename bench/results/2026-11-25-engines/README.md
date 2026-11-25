# The engine runs of 2026-11-25

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

The raw data behind [`docs/ENGINES-2026-11-25.md`](../../../docs/ENGINES-2026-11-25.md) and [`docs/ENGINES-FIRST-RUNS-2026-11-25.md`](../../../docs/ENGINES-FIRST-RUNS-2026-11-25.md): `cig run` on each engine, and on the default (cig-script#259's `auto`: the VM when a module is kept for the script, else the tree-walker, with a `cig __lower` left behind after a script's second run to lower it and keep the module). A development machine and builds of the source, not the benchmark paper's test machine.

| folder | what it holds |
|---|---|
| `main/pass1/`, `main/pass2/` | the 25 programs of `bench/compute` on the sides `cig-tree`, `cig-vm`, `cig-vm-cold` and `cig-auto`, and, for `cig run` against other languages, `cig`, `lua`, `luajit-off`, `luajit`, `python`, `node` and `rust`: the files [`bench/compare/compare.py`](../../compare/README.md) writes (`samples.tsv`, `checks.tsv`, `memory.tsv`, `rounds.tsv`, `inputs.tsv`, `conditions.txt`) |
| `start/pass1/`, `start/pass2/` | three scripts on those sides and on `cig-auto-cold` and `cig-auto-second` (a script's first and second run under the default): `bench/compute/hello.cig`, `bench/engines/lines-500.cig` and cigo's `orchestrate.cig` (cig-orchestrator at `e0c21a7`, run with no arguments), given to the harness as its `--compute` folder. Two CPUs for each measured process |
| `start-one-cpu/pass1/`, `start-one-cpu/pass2/` | the same with every measured process held to one CPU, so a `cig __lower` left behind shares it with the run that started it |
| `before/pass1/`, `before/pass2/`, `before-one-cpu/pass1/`, `before-one-cpu/pass2/` | the three scripts with the cig built before the default lowered a script on its second run (`main` at `da5cd6f`), where `cig-auto-cold` is a first run that starts the lowering; two CPUs and one |
| `numbers.json` in each of the five | every number, as `report.py` made it |
| `main/engines.md.in`, `start/first-runs.md.in` | the templates of the two documents; the second quotes the other three groups of start passes beside its own |
| `main/readme.md.in`, `start/readme.md.in`, and `readme.md` beside each | the templates of the two paragraphs the README quotes under "How fast it is", and each as rendered |

Every number in the two documents is made from the passes and checked by a second program written apart from the first. The full chain is [`run.sh`](run.sh), which takes the `cig` to measure, the `cig` built before the change, and a scratch folder outside the repository (`$WORK` here):

```sh
CIGO_SCRIPT=../cig-orchestrator/orchestrate.cig BEFORE_CIG=./cig-before \
  LUA=./lua LUAJIT=./luajit RUSTC=./rustc \
  bench/results/2026-11-25-engines/run.sh ./cig "$WORK"
```

Both were built from the source as the release workflow builds for Linux (`cargo build --release --target x86_64-unknown-linux-musl`), `./cig` from commit `7e76bb8` (merged to `main` as `cebd9dd`, #287; sha256 `408988111d0320c06f5f650e96b12a11dbfc91e8ed5459a3ebe362dd1321ab04`, which the main passes' `conditions.txt` also records) and `./cig-before` from `main` at `da5cd6f` (sha256 `b3b5481c8e1b9d2659a72bb3992644870bfd257ecedfe6a81040a2c599084673`). No release has `auto` yet, and both still report themselves as `cig 1.25.0`.

How a pass is made, inside `run.sh` (`--out` and the cig home are folders of `run.sh`'s choosing; `start-src` holds the three scripts and their expected output):

```sh
python3 bench/compare/compare.py --out main/pass1 --seed 1 --rounds 21 --cpus 2 --rest 1 --cool 70 --cig ./cig \
  --lua ./lua --luajit ./luajit --rustc ./rustc \
  --sides cig-tree,cig-vm,cig-vm-cold,cig-auto,cig,lua,luajit-off,luajit,python,node,rust
python3 bench/compare/compare.py --out start/pass1 --seed 1 --rounds 101 --cpus 2,3 --warm 5 --cool 70 --cig ./cig \
  --compute start-src --sides cig-tree,cig-vm,cig-vm-cold,cig-auto,cig-auto-cold,cig-auto-second
python3 bench/compare/compare.py --out start-one-cpu/pass1 --seed 1 --rounds 101 --cpus 2 --warm 5 --cool 70 --cig ./cig \
  --compute start-src --sides cig-tree,cig-vm,cig-vm-cold,cig-auto,cig-auto-cold,cig-auto-second
python3 bench/compare/compare.py --out before/pass1 --seed 1 --rounds 101 --cpus 2,3 --warm 5 --cool 70 --cig ./cig-before \
  --compute start-src --sides cig-tree,cig-vm,cig-vm-cold,cig-auto,cig-auto-cold
python3 bench/compare/compare.py --out before-one-cpu/pass1 --seed 1 --rounds 101 --cpus 2 --warm 5 --cool 70 --cig ./cig-before \
  --compute start-src --sides cig-tree,cig-vm,cig-vm-cold,cig-auto,cig-auto-cold
```

Pass 2 of each is the same with `--seed 2`. The compute programs rest after each run for as long as the run took (`--rest 1`); the start scripts, which run for a few milliseconds, are each started from a CPU the harness has just kept busy for 5 ms (`--warm 5`); every round of every pass waits for the CPU package to be below 70 °C (`--cool 70`).

Then the numbers and the documents, made by `report.py` and made again by `check.py`:

```sh
r=bench/results/2026-11-25-engines
python3 bench/compare/report.py --pass $r/main/pass1 --pass $r/main/pass2 --numbers $r/main/numbers.json \
  --render $r/main/engines.md.in docs/ENGINES-2026-11-25.md --render $r/main/readme.md.in $r/main/readme.md
for g in start-one-cpu before before-one-cpu; do
  python3 bench/compare/report.py --pass $r/$g/pass1 --pass $r/$g/pass2 --numbers $r/$g/numbers.json
done
python3 bench/compare/report.py --pass $r/start/pass1 --pass $r/start/pass2 --numbers $r/start/numbers.json \
  --render $r/start/first-runs.md.in docs/ENGINES-FIRST-RUNS-2026-11-25.md --render $r/start/readme.md.in $r/start/readme.md \
  --earlier start-one-cpu $r/start-one-cpu/numbers.json --earlier before $r/before/numbers.json \
  --earlier before-one-cpu $r/before-one-cpu/numbers.json
```

`check.py` takes the same arguments, and `--contains README.md $r/main/readme.md` (and the same for `start/readme.md`) to check that each rendered paragraph is in the repository's README exactly once.

What each side of the default is: `cig-auto` is timed with the script's module kept (the script is run twice before the rounds, and its lowering waited for); `cig-auto-cold` has the tin's modules folder removed before every timed run, so each is the first run of a script the tin has not seen; `cig-auto-second` has the folder put back before every timed run as the script's first run left it (a copy the harness keeps from its checked run), so each timed run is a script's second, with no run of cig just before it. Starting `cig __lower` is inside the run that starts it; the lowering goes on after that run has exited and is not timed. Between runs the harness waits (not timed) until no `cig __lower` is running, up to 30 s, and a pass's `conditions.txt` has a `lowering_timeouts` line with the number of waits that ran past that.
