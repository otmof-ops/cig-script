# Standard library reference

Generated from `cig --json language` (CigScript 1.18.0) by `scripts/gen-stdlib-doc.py`. Do not edit by hand.

Functions marked **burn** change the world and can only be called inside a `burn { }` block. `read` functions look at the world without changing it; `pure` functions touch nothing.

Methods are called on a value (`"x".upper()`, `list.map(f)`, `map.has("k")`); the receiver is not counted in the arity column.

## globals

| function | args | effect |
|---|---|---|
| `len` | 1 | pure |
| `str` | 1 | pure |
| `repr` | 1 | pure |
| `int` | 1 | pure |
| `float` | 1 | pure |
| `bool` | 1 | pure |
| `type_of` | 1 | pure |
| `range` | 1-3 | pure |
| `keys` | 1 | pure |
| `values` | 1 | pure |
| `entries` | 1 | pure |
| `assert` | 1-2 | pure |
| `exit` | 0-1 | pure |
| `stdin` | 0 | read |
| `light` | 1-2 | pure |

## fs

Reads are free; anything that changes a file is a **burn**, snapshotted before and put back by `cig unburn`. Symlinks are seen as links (`fs.exists` is true for a dangling one) and restored as links.

| function | args | effect |
|---|---|---|
| `fs.read_text` | 1 | read |
| `fs.read_lines` | 1 | read |
| `fs.exists` | 1 | read |
| `fs.is_file` | 1 | read |
| `fs.is_dir` | 1 | read |
| `fs.size` | 1 | read |
| `fs.modified_ms` | 1 | read |
| `fs.list` | 1 | read |
| `fs.glob` | 1 | read |
| `fs.cwd` | 0 | read |
| `fs.home` | 0 | read |
| `fs.write_text` | 2-3 | **burn** |
| `fs.append_text` | 2-3 | **burn** |
| `fs.mkdir` | 1 | **burn** |
| `fs.rm` | 1 | **burn** |
| `fs.cp` | 2 | **burn** |
| `fs.mv` | 2 | **burn** |

## path

| function | args | effect |
|---|---|---|
| `path.join` | 1+ | pure |
| `path.base` | 1 | pure |
| `path.dir` | 1 | pure |
| `path.ext` | 1 | pure |
| `path.stem` | 1 | pure |
| `path.with_ext` | 2 | pure |
| `path.normalize` | 1 | pure |
| `path.is_abs` | 1 | pure |
| `path.abs` | 1 | read |
| `path.parts` | 1 | pure |

## json

| function | args | effect |
|---|---|---|
| `json.parse` | 1 | pure |
| `json.stringify` | 1-2 | pure |
| `json.load` | 1 | read |
| `json.save` | 2-3 | **burn** |

## text

| function | args | effect |
|---|---|---|
| `text.matches` | 2 | pure |
| `text.find` | 2 | pure |
| `text.find_all` | 2 | pure |
| `text.captures` | 2 | pure |
| `text.replace` | 3 | pure |
| `text.split` | 2 | pure |
| `text.template` | 2 | pure |

## proc

**Hops.** Every `proc.*` call crosses a process boundary (a *hop*, in the lexicon): the kernel cannot see inside, so a hop is journaled as **irreversible** and the plan says so out loud. `proc.run` is the one call shape; `proc.text`, `proc.lines`, `proc.json`, `proc.csv` and `proc.kv` parse stdout into a real value under the contract `ok: [0]`; `proc.pipe` connects stages without a shell; `proc.shell` is the shell, by name. Never a shell otherwise, both streams read concurrently and never merged, a timeout on every hop, the whole process group killed on timeout, explicit encoding, and a distinct code (E550-E557) for every way a hop can fail. The rules, each with a test, are in [HOPS.md](HOPS.md).

| function | args | effect |
|---|---|---|
| `proc.run` | 1-3 | **burn** |
| `proc.text` | 1-3 | **burn** |
| `proc.lines` | 1-3 | **burn** |
| `proc.json` | 1-3 | **burn** |
| `proc.csv` | 1-3 | **burn** |
| `proc.kv` | 1-3 | **burn** |
| `proc.pipe` | 1-2 | **burn** |
| `proc.shell` | 1-2 | **burn** |
| `proc.which` | 1 | read |
| `proc.apply_layer` | 1-2 | **burn** |
| `proc.discard_layer` | 1 | **burn** |

## time

| function | args | effect |
|---|---|---|
| `time.now_ms` | 0 | read |
| `time.now_iso` | 0 | read |
| `time.stamp` | 0 | read |
| `time.sleep_ms` | 1 | pure |
| `time.format_ms` | 1-2 | pure |

## hash

| function | args | effect |
|---|---|---|
| `hash.sha256` | 1 | pure |
| `hash.sha256_file` | 1 | read |
| `hash.short` | 1-2 | pure |

## cig

The cig a script runs under: its version as `cig --version` prints it, the platform it was built for (`linux`, `macos`, `windows`; `x86_64`, `aarch64`), and the id of the run in progress, the one `cig unburn` takes (null in a dry run and under `cig check`). A tool written in CigScript checks the cig it was handed against what it was written for with these, and names its own run; the pin (`# cig 1.2` under the shebang) is how a script asks for the right one in the first place.

| function | args | effect |
|---|---|---|
| `cig.version` | 0 | pure |
| `cig.os` | 0 | pure |
| `cig.arch` | 0 | pure |
| `cig.run_id` | 0 | read |

## math

| function | args | effect |
|---|---|---|
| `math.abs` | 1 | pure |
| `math.min` | 1+ | pure |
| `math.max` | 1+ | pure |
| `math.floor` | 1 | pure |
| `math.ceil` | 1 | pure |
| `math.round` | 1-2 | pure |
| `math.sqrt` | 1 | pure |
| `math.pow` | 2 | pure |
| `math.clamp` | 3 | pure |

## env

Reading the environment is free; setting or unsetting it is a burn, journaled as irreversible because the kernel cannot restore another process's view of it.

| function | args | effect |
|---|---|---|
| `env.get` | 1-2 | read |
| `env.has` | 1 | read |
| `env.all` | 0 | read |
| `env.set` | 2 | **burn** |
| `env.unset` | 1 | **burn** |

## csv

| function | args | effect |
|---|---|---|
| `csv.parse` | 1-2 | pure |
| `csv.stringify` | 1-2 | pure |
| `csv.read` | 1-2 | read |
| `csv.write` | 2-3 | **burn** |

## log

| function | args | effect |
|---|---|---|
| `log.debug` | 1+ | pure |
| `log.info` | 1+ | pure |
| `log.warn` | 1+ | pure |
| `log.error` | 1+ | pure |

## string methods

| function | args | effect |
|---|---|---|
| `len` | 0 | pure |
| `upper` | 0 | pure |
| `lower` | 0 | pure |
| `trim` | 0 | pure |
| `trim_start` | 0 | pure |
| `trim_end` | 0 | pure |
| `split` | 0-1 | pure |
| `lines` | 0 | pure |
| `chars` | 0 | pure |
| `contains` | 1 | pure |
| `starts_with` | 1 | pure |
| `ends_with` | 1 | pure |
| `replace` | 2 | pure |
| `find` | 1 | pure |
| `slice` | 1-2 | pure |
| `repeat` | 1 | pure |
| `pad_left` | 1-2 | pure |
| `pad_right` | 1-2 | pure |
| `reverse` | 0 | pure |
| `is_empty` | 0 | pure |
| `to_int` | 0 | pure |
| `to_float` | 0 | pure |

## list methods

| function | args | effect |
|---|---|---|
| `len` | 0 | pure |
| `is_empty` | 0 | pure |
| `push` | 1+ | pure |
| `pop` | 0 | pure |
| `insert` | 2 | pure |
| `remove` | 1 | pure |
| `first` | 0 | pure |
| `last` | 0 | pure |
| `get` | 1-2 | pure |
| `slice` | 1-2 | pure |
| `contains` | 1 | pure |
| `index_of` | 1 | pure |
| `map` | 1 | pure |
| `filter` | 1 | pure |
| `reduce` | 2 | pure |
| `each` | 1 | pure |
| `any` | 1 | pure |
| `all` | 1 | pure |
| `find` | 1 | pure |
| `sort` | 0 | pure |
| `sort_by` | 1 | pure |
| `reverse` | 0 | pure |
| `join` | 0-1 | pure |
| `unique` | 0 | pure |
| `flatten` | 0 | pure |
| `sum` | 0 | pure |
| `min` | 0 | pure |
| `max` | 0 | pure |
| `zip` | 1 | pure |
| `enumerate` | 0 | pure |
| `chunks` | 1 | pure |
| `copy` | 0 | pure |

## map methods

| function | args | effect |
|---|---|---|
| `len` | 0 | pure |
| `is_empty` | 0 | pure |
| `keys` | 0 | pure |
| `values` | 0 | pure |
| `entries` | 0 | pure |
| `has` | 1 | pure |
| `get` | 1-2 | pure |
| `set` | 2 | pure |
| `remove` | 1 | pure |
| `merge` | 1 | pure |
| `copy` | 0 | pure |
