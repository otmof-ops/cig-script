# The numbers of a benchmark comparison, and the documents that quote them

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

This is what `report.py` and `check.py` are both written to. `compare.py` measures and writes the raw files; `report.py` computes every number from them, writes `numbers.json` and renders the documents; `check.py`, written apart from `report.py` and sharing no code with it, computes every number again and renders every document again. The documents are right when the two agree **byte for byte**.

Every definition below is exact. Where Python is named (a function, a format), that function or format is the definition.

## 1. The input: one folder per pass

A run is one or more **passes**, given in order (`--pass DIR`, repeated): pass 1, pass 2, ... Each folder holds these files, all UTF-8, lines ending in `\n`, fields separated by one tab. Blank lines are skipped. The first line of each `.tsv` is its header and is not data. A line may have more fields than the columns named here; the others are not used.

| file | columns used | each data line |
|---|---|---|
| `samples.tsv` | `program side round ms` | one timed run: program name, side name, round (an integer from 1), wall time in milliseconds (a decimal) |
| `checks.tsv` | `program side status` | one (program, side) pair's check; `status` is `ok`, `WRONG` or `failed`. A pair can have more than one line |
| `memory.tsv` | `program side reading peak_kb anon_kb` | one reading of a pair's peak memory: `reading` is 1, 2, 3, ...; `peak_kb` the peak resident set and `anon_kb` the anonymous part at exit, in kB (integers). A pair has a line per reading |
| `rounds.tsv` | `round started_utc cpu_mhz temp_c history` | one round: its number, the time it started, the mean CPU clock in MHz, the package temperature in °C, and how many run records cig keeps, each an integer or `unknown` |
| `conditions.txt` | (no header) | `KEY` then one or more tab-separated values; a key can repeat (`side`, `command`, `env`, `sha256`, ...) |

`condition(pass, KEY)` is the values of the **first** line of that pass's `conditions.txt` whose first field is `KEY` (a list of strings), or nothing.

The `conditions.txt` lines this spec reads: `date_utc`, `cpu`, `cores`, `max_mhz`, `kernel`, `driver`, `governor`, `epp`, `no_turbo`, `profile`, `transparent_hugepages`, `mains`, `load_average`, `temp_c`, `history`, `harness`, `rounds`, `seed`, `floor_ms` (each `KEY<TAB>VALUE...`); `side<TAB>NAME<TAB>LABEL<TAB>VERSION`; and `sha256<TAB>SIDE<TAB>FILE<TAB>HEX`.

## 2. Programs and sides

- **Programs:** every program name that appears in any pass's `samples.tsv`.
- **The start-up set** is `hello`, `startup-0`, `startup-1`, `startup-10`, in that order, keeping only those among the programs.
- **The compute set** is every other program, sorted by Python's `sorted()` on the name.
- **The order of programs** in every table and list is the compute set, then the start-up set.
- **Sides:** every side name that appears in any pass's `samples.tsv`.

Named groups of sides (each in this order):

- `CIG_SIDES` = `cig`, `stage-a`, `stage-b`, `cig-dry`, `stage-a-dry`, `stage-b-dry`
- `LANGUAGES` = `lua`, `luajit-off`, `luajit`, `python`, `node`, `rust`
- `IDIOMS` = `python-idiom` (its base is `python`), `node-idiom` (base `node`), `rust-idiom` (base `rust`)
- `ENGINES` = `cig-tree`, `cig-vm`, `cig-vm-cold`: `cig run` under each engine, with the script's module in the tin and with nothing in the tin
- `MAIN` = `cig`, `stage-b`, then `LANGUAGES`
- `ALL` = `CIG_SIDES`, then `cig-first`, then `ENGINES`, then `LANGUAGES`, then the three of `IDIOMS`

## 3. Which numbers exist: a valid pair

A (program, side) pair is **valid** when, in **every** pass, `samples.tsv` has at least one sample for it **and** `checks.tsv` has no line for it whose status is anything but `ok`. Only valid pairs have numbers. A key whose pair, pairs or set is not valid does not exist.

## 4. Statistics

For a valid pair:

- `samples(pass k)` = the `ms` values of its lines in pass k's `samples.tsv`, as floats.
- `pooled` = the samples of every pass together.
- `median` = `statistics.median(pooled)`; `median_k` = `statistics.median(samples(pass k))`.
- `p10` and `p90` are nearest-rank quantiles of `pooled`: with `x = sorted(pooled)` and `n = len(x)`, the q-quantile is `x[max(1, math.ceil(q * n)) - 1]`, for q = 0.10 and 0.90.

A **ratio** of side A to side B on program p is `median(p, A) / median(p, B)`.

A **geometric mean** of a list of ratios is `math.exp(math.fsum(math.log(r) for r in ratios) / len(ratios))`.

On a set of programs, with the ratios of A to B on the programs of the set (in the order of section 2) where both pairs are valid: **faster** counts the ratios below `0.97`, **slower** those above `1.03`, and **level** the rest (`n - faster - slower`).

## 5. Display formats

| kind | format |
|---|---|
| ms | `"%.1f" % v` |
| ratio | `"%.2f" % v` if `v < 10`; else `"%.1f" % v` if `v < 100`; else `"%.0f" % v` |
| MB | `"%.1f" % (kb * 1024 / 1e6)`, kb the value in kB |
| count | `str(n)` |
| a missing number in a table | `–` (U+2013) |

## 6. The keys

`numbers.json` is one JSON object: each key maps to `{"text": TEXT, "value": VALUE}`, written as `json.dump(obj, f, indent=1, sort_keys=True)` followed by one newline. `value` is the unrounded number (a float or an int) or, for a date, the string. `check.py` holds each `text` exactly, an int `value` exactly, and a float `value` to within a relative difference of 1e-9.

In what follows, `p` is a program, `s` a side, `k` a pass number from 1 (written `passK`, for example `pass1`), `SET` is `compute` or `start`, and `A`, `B` are sides.

**Per pair** (for each valid (p, s)):

| key | value | text |
|---|---|---|
| `median.p.s` | `median` | ms |
| `p10.p.s`, `p90.p.s` | the quantile | ms |
| `median.passK.p.s` | `median_k`, for each pass k | ms |
| `peak.p.s` | `statistics.median` of the `peak_kb` of **every** reading of the pair in every pass, when every pass has at least one reading for it | MB |
| `anon.p.s` | the same with `anon_kb` | MB |

**Comparisons**, for each (A, B) in PAIRS below:

| key | exists when | value | text |
|---|---|---|---|
| `ratio.p.A.B` | (p, A) and (p, B) both valid | the ratio | ratio |
| `geomean.SET.A.B` | at least one program of SET has both pairs valid | the geometric mean of the ratios over those programs | ratio |
| `inverse.SET.A.B` | as `geomean.SET.A.B` | `1 / geomean` | ratio |
| `faster.SET.A.B`, `level.SET.A.B`, `slower.SET.A.B` | as `geomean.SET.A.B` | as section 4 | count |
| `n.SET.A.B` | as `geomean.SET.A.B` | how many programs | count |
| `geomean.passK.SET.A.B` | as `geomean.SET.A.B`, for each pass k | the geometric mean over the same programs of `median_k(p, A) / median_k(p, B)` | ratio |
| `lowest.SET.A.B`, `highest.SET.A.B` | as `geomean.SET.A.B` | the smallest and the largest of those programs' ratios | **the name of the program** it belongs to; of two equal ratios, the program that comes first in the order of section 2 |

**PAIRS**, in this order (the order changes no number; each key is made once):

1. (A, B) for each A in `CIG_SIDES`, for each B in `LANGUAGES`;
2. (`stage-b`, `cig`), (`stage-a`, `cig`), (`stage-b-dry`, `cig-dry`);
3. (`python-idiom`, `python`), (`node-idiom`, `node`), (`rust-idiom`, `rust`);
4. (`stage-b`, `python-idiom`), (`stage-b`, `node-idiom`), (`stage-b`, `rust-idiom`);
5. (`cig-vm`, `cig-tree`), (`cig-vm-cold`, `cig-tree`), (`cig-vm`, `stage-b`);
6. (`cig-vm`, B) for each B in `LANGUAGES`.

**The run record**, for R in `cig`, `stage-a`, `stage-b`, with R-dry being R followed by `-dry`, over the programs (in the order of section 2) where (p, R) and (p, R-dry) are both valid, when there is at least one:

| key | value | text |
|---|---|---|
| `record.n.R` | how many such programs | count |
| `record.ratio.R` | the geometric mean of `median(p, R) / median(p, R-dry)` | ratio |
| `record.extra.R` | `statistics.median` of the list of `median(p, R) - median(p, R-dry)` | ms |

**Per side**, for each side s with at least one valid pair; "its programs" are the programs (in order) where (p, s) is valid:

| key | value | text |
|---|---|---|
| `programs.s` | how many programs | count |
| `repeat.passK.s` | for each pass k from 2: the geometric mean over its programs of `median_k(p, s) / median_1(p, s)` | ratio |
| `drift.passK.s` | for each pass k where `T = R // 3 > 0`, R being `int(condition(pass k, "rounds")[0])`: the geometric mean over its programs of `statistics.median(late) / statistics.median(early)`, `early` being the pair's pass-k samples of rounds 1 to T and `late` those of rounds R − T + 1 to R | ratio |
| `noisy.passK.s` | for the same passes as `drift`: how many of its programs have `statistics.pstdev(x) / statistics.fmean(x) > 0.05`, x being the pair's pass-k samples | count |

**Per pass** (for each pass k):

| key | exists when | value | text |
|---|---|---|---|
| `floor.passK` | always | `float(condition(pass k, "floor_ms")[0])` | `"%.2f" % value` |
| `rounds.passK` | always | `int(condition(pass k, "rounds")[0])` | the field as written |
| `date.passK` | always | the string `condition(pass k, "date_utc")[0]` | the same string |
| `samples.passK` | always | the number of data lines in pass k's `samples.tsv` | count |
| `pairs.passK` | always | the number of distinct (program, side) pairs in pass k's `samples.tsv` | count |
| `failed.passK` | always | the number of distinct (program, side) pairs that have a line in pass k's `checks.tsv` whose status is anything but `ok` | count |
| `history.first.passK`, `history.last.passK` | pass k's `rounds.tsv` has a data line | `int` of the `history` field of its first and of its last data line | count |
| `mhz.min.passK`, `mhz.max.passK` | at least one data line of pass k's `rounds.tsv` has a `cpu_mhz` of digits only | the smallest and the largest such value, as ints | count |
| `temp.max.passK` | at least one data line has a `temp_c` that is digits, perhaps after one `-` | the largest such value, as an int | count |

**For the whole run:** `passes` (the number of passes), `programs.compute` and `programs.start` (the sizes of the two sets): each a count.

## 7. The tables

A table is Markdown: a header line `| H1 | H2 | ... |`, an alignment line, then a line per row `| C1 | C2 | ... |` (cells joined by ` | `, with `| ` before and ` |` after), lines joined by `\n` with no newline after the last. The alignment line is `|` + the column marks joined by `|` + `|`, where a text column's mark is `---` and a number column's is `---:`. Unless a table says otherwise its first column is text and the rest are numbers. **A row whose every number cell is `–` is left out**; a table whose columns are all text keeps every row.

A cell that names a key is that key's text, or `–` when the key does not exist. A program's name in a cell is in backticks: `` `fib` ``.

**Labels** of sides (the label is everything between the two `|` of its cell, trimmed):

| side | label |
|---|---|
| `cig` | `` `cig run` `` |
| `cig-dry` | `` `cig run --dry-run` `` |
| `cig-first` | `` `cig run`, new folder `` |
| `cig-tree` | `` `cig run --engine tree` `` |
| `cig-vm` | `` `cig run --engine vm` `` |
| `cig-vm-cold` | `` `cig run --engine vm`, empty tin `` |
| `stage-a` | stage a |
| `stage-a-dry` | stage a, `` `--dry-run` `` |
| `stage-b` | stage b |
| `stage-b-dry` | stage b, `` `--dry-run` `` |
| `lua` | Lua |
| `luajit-off` | LuaJIT `` `-joff` `` |
| `luajit` | LuaJIT |
| `python` | CPython |
| `node` | Node |
| `rust` | Rust |
| `python-idiom` | CPython, idiomatic |
| `node-idiom` | Node, idiomatic |
| `rust-idiom` | Rust, idiomatic |

In plain characters: `cig`'s label is backtick, `cig run`, backtick; `cig-dry`'s is backtick, `cig run --dry-run`, backtick; `cig-first`'s is backtick, `cig run`, backtick, `, new folder`; `cig-tree`'s is backtick, `cig run --engine tree`, backtick; `cig-vm`'s is backtick, `cig run --engine vm`, backtick; `cig-vm-cold`'s is backtick, `cig run --engine vm`, backtick, `, empty tin`; `stage-a-dry`'s is `stage a, ` then backtick, `--dry-run`, backtick (and `stage-b-dry`'s the same with `b`); `luajit-off`'s is `LuaJIT ` then backtick, `-joff`, backtick.

| name | header | rows (in order) | cells |
|---|---|---|---|
| `medians` | `program`, then the label of each side in `MAIN` | each program | `median.p.s` for each side in `MAIN` |
| `medians-cig` | `program`, then the labels of `cig`, `cig-dry`, `stage-a`, `stage-a-dry`, `stage-b`, `stage-b-dry` | each program | `median.p.s` for each of those sides |
| `spread` | `program`, then the label of each side in `MAIN` | each program | `P10–P90` (the texts of `p10.p.s` and `p90.p.s` joined by U+2013), or `–` when `p10.p.s` does not exist |
| `ratios-compute` | `time ÷ theirs`, then the label of each side in `LANGUAGES` | the labels of `cig`, `stage-a`, `stage-b` | for row side A and column side B: `G (F/L/S)`, the texts of `geomean.compute.A.B`, `faster.compute.A.B`, `level.compute.A.B`, `slower.compute.A.B`; or `–` when `geomean.compute.A.B` does not exist |
| `ratios-start` | as `ratios-compute` | as `ratios-compute` | as `ratios-compute`, with `start` for `compute` |
| `ratios-compute-dry` | as `ratios-compute` | the labels of `cig-dry`, `stage-a-dry`, `stage-b-dry` | as `ratios-compute` |
| `memory` | `program`, then the label of each side in `MAIN` | each program | `peak.p.s` for each side in `MAIN` |
| `memory-own` | as `memory` | each program | `anon.p.s` for each side in `MAIN` |
| `record` | (an empty string), `programs`, `run ÷ dry run`, `run − dry run, median ms` | for R in `cig`, `stage-a`, `stage-b` where `record.n.R` exists | the label of R, `record.n.R`, `record.ratio.R`, `record.extra.R` |
| `idiom` | `program`, `language`, `translation, ms`, `idiomatic, ms`, `idiomatic ÷ translation`, `stage b ÷ idiomatic`. **Two text columns.** | for each idiom side I in `IDIOMS` order, for each program p in order, where (p, I) and (p, base) are both valid | `` `p` ``, the label of the base side, `median.p.base`, `median.p.I`, `ratio.p.I.base`, `ratio.p.stage-b.I` |
| `passes` | `compute set`, then for A in `cig`, `stage-b`, for each pass k: the label of A, then ` ÷, pass `, then k | the label of each side in `LANGUAGES` | for A in `cig`, `stage-b`, for each pass k: `geomean.passK.compute.A.B` |
| `moved` | `program`, `side`, then for each pass k: `pass k, ms`. **Two text columns.** | for each program p in order, for each side s in `ALL` where (p, s) is valid and the largest of its `median_k` over the passes divided by the smallest is above `1.10` | `` `p` ``, the label of s, then `median.passK.p.s` for each pass k |
| `stability` | `side`, `programs`, then for each pass k from 2: `pass k ÷ pass 1`; then for each pass k: `last third ÷ first third, pass k`; then for each pass k: `CV over 5%, pass k` | for each side s in `ALL` where `programs.s` exists | the label of s, `programs.s`, then `repeat.passK.s` for each k from 2, `drift.passK.s` for each k, `noisy.passK.s` for each k |
| `engines` | `program`, then the labels of `cig-tree`, `cig-vm`, `cig-vm-cold`, then `vm ÷ tree`, then `vm, empty tin ÷ tree` | each program | `median.p.cig-tree`, `median.p.cig-vm`, `median.p.cig-vm-cold`, `ratio.p.cig-vm.cig-tree`, `ratio.p.cig-vm-cold.cig-tree` |
| `conditions` | (an empty string), then `pass k` for each pass. **Every column is text**, so no row is left out. | see below | see below |

**`conditions`, its rows in order.** First, for each (KEY, label) below, a row: the label, then for each pass the shown value of `condition(pass, KEY)[0]`, or `–` when the pass has no such line:

| KEY | label | shown value |
|---|---|---|
| `date_utc` | `started (UTC)` | as written |
| `cpu` | `processor` | as written |
| `cores` | `logical cores` | as written |
| `max_mhz` | `highest clock, MHz` | as written |
| `kernel` | `kernel` | as written |
| `driver` | `frequency driver` | as written |
| `governor` | `CPU governor` | as written |
| `epp` | `energy preference` | as written |
| `no_turbo` | `turbo` | `on` for `0`, `off` for `1`, else as written |
| `profile` | `power profile` | as written |
| `transparent_hugepages` | `transparent huge pages` | as written |
| `mains` | `on mains power` | `yes` for `1`, `no` for `0`, else as written |
| `load_average` | `load average at the start` | its first three fields when split on whitespace (`str.split()`), joined by one space |
| `temp_c` | `package °C at the start` | as written |
| `history` | `cig's run records at the start` | as written |
| `harness` | `harness` | as written |
| `rounds` | `rounds` | as written |
| `seed` | `seed of the rounds' order` | as written |
| `floor_ms` | `` `true`, median ms `` | as written |

Then four rows, each the label then, for each pass k, the text of a key (or `–`): `cig's run records at the last round` (`history.last.passK`); `mean clock at a round's start, lowest, MHz` (`mhz.min.passK`); `mean clock at a round's start, highest, MHz` (`mhz.max.passK`); `package °C at a round's start, highest` (`temp.max.passK`).

Then, for each (SIDE, label) in (`cig`, `cig`), (`stage-a`, `cigcompile`), (`lua`, `Lua`), (`luajit`, `LuaJIT`), (`python`, `CPython`), (`node`, `Node`), (`rust`, `rustc`): a row of the label, then for each pass the fourth field (VERSION) of that pass's first `side` line whose second field is SIDE, or `–`; the row is kept only when at least one pass has one.

Then, for each `sha256` line of **pass 1** in file order: a row `` sha256 of `FILE` `` (FILE its third field), then for each pass the fourth field (HEX) of that pass's first `sha256` line whose third field is FILE, in backticks, or `–`; a row identical to one already in the table is not added again.

## 8. Rendering a template

A template is text. Every match of the regular expression `\{\{([^{}]+)\}\}` is replaced (Python's `re.sub`), with what is inside the braces:

- `table:NAME`: the table NAME of section 7. An unknown NAME is an error.
- anything else: the text of that key. A key that does not exist is an error.

Nothing else in the template changes.

## 9. The two programs

```
report.py --pass DIR [--pass DIR ...] --numbers OUT.json [--render TEMPLATE OUT ...]
check.py  --pass DIR [--pass DIR ...] --numbers OUT.json [--render TEMPLATE OUT ...]
          [--contains FILE SNIPPET ...]
```

`check.py` computes the keys itself and compares them with the given `numbers.json`: the same set of keys, each key's text and value as section 6 says, and the file's bytes exactly as section 6 writes it. It renders each TEMPLATE itself and compares the result with OUT, byte for byte. For each `--contains FILE SNIPPET`, both files, it checks that FILE's text contains SNIPPET's text exactly once. It prints every difference it finds (the key, or the first differing line of a document, with both values) and exits 1 when there is any, 0 when there is none, saying how many keys, documents and snippets it checked.
